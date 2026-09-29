//! Session detection lifecycle: which panes still owe a session id, the
//! floor/latch/give-up rules that govern the filesystem scan for it, and the
//! cross-instance claims (D7) taken on what it finds. `App` supplies the
//! observations and applies the returned `Found` effects (persist, feed);
//! everything `PaneId`-keyed about detection lives and dies here, so a
//! recycled pane id (C23) is pruned by one `forget_pane` call.
//!
//! The *decision* half (is a stored id resumable, which ids are taken) is
//! `session_resolver`; this is the stateful half around it.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime};

use crate::agents::Registry;
use crate::core::layout::PaneId;
use crate::core::session_resolver;
use crate::core::workspace::{PaneSpec, Workspace};
use crate::ports::{ClaimError, ClaimHandle, SessionClaims};

/// Slack subtracted from a promoted pane's "last seen as a shell" bound.
///
/// `App::observe_panes` reads the process tree on a `DETECT_INTERVAL` tick, so
/// its view can lag the truth by up to one tick: an agent that started at T
/// may still be observed as a shell a moment later, and its session file —
/// written at T — would then fall just outside a strict bound and never be
/// found. A few seconds of slack absorbs that while still excluding
/// everything a previous run left behind, which is the whole point.
const PROMOTION_GRACE: Duration = Duration::from_secs(10);

/// How long the filesystem fallback keeps looking for a pane's session file
/// before giving up.
///
/// `pending` was previously only cleared on success or when the pane
/// vanished, so a pane whose agent never wrote a file roost could attribute
/// stayed pending for the life of the process — and since no adapter
/// overrides `detect_session`, each tick meant a full recursive walk of the
/// whole session root (`~/.pi/agent/sessions`: every project, all history)
/// stat-ing every file, twice a minute, forever.
///
/// A minute is far longer than any agent takes to create its session file,
/// so giving up costs nothing real — and when it is wrong the failure is
/// the safe one: the pane simply starts fresh next launch instead of
/// resuming. The exact channel (the agent-side extension, design doc §6.1)
/// does not go through `pending` at all and keeps working after this
/// expires.
const DETECT_GIVE_UP: Duration = Duration::from_secs(60);

/// What `poll` asks `App` to do — `App` owns the workspace and the feed.
pub enum Found {
    /// Session claimed (or claiming was unavailable: `Some(err)`, proceed
    /// unclaimed); persist it on the pane.
    Adopted { id: PaneId, session: String, unavailable: Option<String> },
    /// A candidate is claimed by another instance (D7); report it once.
    Held { id: PaneId, session: String, owner: String },
}

pub struct SessionDetector {
    /// Cross-instance session claims (D7): the port every acquire/release
    /// goes through.
    claims: Box<dyn SessionClaims>,
    /// This instance's open handles keyed by pane, carrying the claimed
    /// session id so a respawn can tell "same session, keep the handle" from
    /// "new session, re-claim" without asking the port twice.
    /// `pub(super)` fields: `core` tests poke them directly.
    pub(super) held: HashMap<PaneId, (String, ClaimHandle)>,
    /// Freshly launched agent panes we still owe a session id, with the
    /// lower bound of the file window.
    pub(super) pending: HashMap<PaneId, SystemTime>,
    /// When each pane was last observed running **no** agent — a plain
    /// shell. The lower bound for a *promoted* pane's session detection.
    ///
    /// A pane promoted by the user typing `pi` at a shell prompt cannot own
    /// a session file older than the last moment it was still a shell, so
    /// this is the honest window. It used to be `SystemTime::UNIX_EPOCH` —
    /// no bound at all — which let the scan claim the newest *unclaimed*
    /// file in the project whenever it was written, i.e. a conversation
    /// from days ago. See `PROMOTION_GRACE`.
    pub(super) last_shell_seen: HashMap<PaneId, SystemTime>,
    /// Panes for which a detection candidate has already been skipped and
    /// reported this conflict (D7: another running instance claims it).
    /// Latched so the feed gets exactly one line per pane, not one every
    /// tick for as long as the other workspace keeps the session — cleared
    /// when the pane adopts a session or closes.
    conflict_latched: HashSet<PaneId>,
}

impl SessionDetector {
    pub fn new(claims: Box<dyn SessionClaims>) -> Self {
        Self {
            claims,
            held: HashMap::new(),
            pending: HashMap::new(),
            last_shell_seen: HashMap::new(),
            conflict_latched: HashSet::new(),
        }
    }

    /// Watch `id` for a session file written from `since` on.
    pub fn queue(&mut self, id: PaneId, since: SystemTime) {
        self.pending.insert(id, since);
    }

    /// The pane has its session (or never needed one): stop scanning for it.
    pub fn unqueue(&mut self, id: PaneId) {
        self.pending.remove(&id);
    }

    /// Drop everything keyed on a closed pane. Ids ARE recycled (C23), so an
    /// unremoved entry would be inherited by an unrelated later pane: a stale
    /// `last_shell_seen` reopens the promotion-window hole
    /// (`a_promoted_pane_never_claims_a_session_older_than_its_shell`), a
    /// stale `pending` keeps hunting on the dead pane's clock, a stale latch
    /// swallows the new pane's first conflict line, and a stale claim's
    /// release-on-drop would later free a claim the new pane never took.
    pub fn forget_pane(&mut self, id: PaneId) {
        self.last_shell_seen.remove(&id);
        self.pending.remove(&id);
        self.conflict_latched.remove(&id);
        self.held.remove(&id);
    }

    /// Release only the pane's claim (session dropped, or overlay pane gone).
    pub fn release(&mut self, id: PaneId) {
        self.held.remove(&id);
    }

    /// Release every claim — before the fleet dies, so a relaunch resumes
    /// immediately.
    pub fn release_all(&mut self) {
        self.held.clear();
    }

    /// D7: claim `(adapter, session)` for pane `id` across all running
    /// instances, keeping one handle per pane. The bookkeeping is
    /// deliberately here, not in the port: a pane respawn re-runs the
    /// restore path, and re-acquiring while still holding the same claim is
    /// refused by the lock itself — so we release before re-acquiring, which
    /// is also what makes the single-instance case never self-block.
    ///
    /// - Same pane, same session: keep the existing handle (idempotent).
    /// - Same pane, new session: drop the old handle, take the new claim.
    /// - `Failed` (claims dir unreadable etc.): proceed **unclaimed** —
    ///   claims are advisory best-effort (see `ports::ClaimError`), and a
    ///   broken claims dir must not brick every restore. `Ok(Some(err))`
    ///   hands the failure back so the caller can keep it from being silent.
    /// - `Held`: refused; the caller owns the user-visible degradation.
    pub fn claim(
        &mut self,
        id: PaneId,
        adapter: &str,
        session: &str,
    ) -> Result<Option<String>, String> {
        if self.held.get(&id).is_some_and(|(s, _)| s == session) {
            return Ok(None);
        }
        self.held.remove(&id);
        match self.claims.acquire(adapter, session) {
            Ok(handle) => {
                self.held.insert(id, (session.to_string(), handle));
                Ok(None)
            }
            Err(ClaimError::Held(desc)) => Err(desc),
            Err(ClaimError::Failed(e)) => Ok(Some(e)),
        }
    }

    /// One observation tick's worth of bookkeeping. `shells`: panes seen
    /// running no agent; `promoted`: panes newly seen running one.
    pub fn observe(&mut self, shells: Vec<PaneId>, promoted: Vec<PaneId>, now: SystemTime) {
        // Not gated on a *transition*: a pane sitting at a shell prompt
        // for an hour must keep moving this bound forward, or the
        // window it eventually gets on promotion would reach back to
        // whenever it last changed state.
        for id in shells {
            self.last_shell_seen.insert(id, now);
        }
        // A newly-recognized agent needs its already-created session file
        // located — it was written moments before roost noticed, so `now()`
        // would miss it. The bound is **the last tick this pane was still a
        // shell**, minus `PROMOTION_GRACE` for observation lag.
        //
        // This used to be `SystemTime::UNIX_EPOCH`, on the reasoning that a
        // wide window "plus the taken-set finds it without cross-wiring".
        // The taken-set does not carry that weight: `claimed_sessions` only
        // knows ids stored on *live* panes, so a conversation from a closed
        // pane or an earlier run is unclaimed and therefore eligible. With
        // no lower bound the scan took the newest such file whenever it was
        // written, `set_session` committed it, and the pane was dropped from
        // `pending` — so the mistake was permanent, and the next relaunch
        // resumed a conversation from days ago. Reported as "it loads the
        // wrong session"; pinned by
        // `a_promoted_pane_never_claims_a_session_older_than_its_shell`.
        for id in promoted {
            let floor = self
                .last_shell_seen
                .get(&id)
                .copied()
                .unwrap_or(now)
                .checked_sub(PROMOTION_GRACE)
                .unwrap_or(SystemTime::UNIX_EPOCH);
            // `max`, not `or_insert`: the window may only ever **tighten**.
            // A pane that promotes, finds nothing (so stays pending),
            // demotes when the agent exits, then promotes again hours later
            // would otherwise keep its first floor — by then old enough to
            // reach conversations from earlier in the same session.
            self.pending.entry(id).and_modify(|f| *f = (*f).max(floor)).or_insert(floor);
        }
    }

    /// Scan for the session file of every pending pane. `spec_of` resolves a
    /// pane (float and popup included); a pane with no spec is dropped.
    pub fn poll<'a>(
        &mut self,
        now: SystemTime,
        registry: &Registry,
        ws: &Workspace,
        spec_of: impl Fn(PaneId) -> Option<&'a PaneSpec>,
    ) -> Vec<Found> {
        let mut found = Vec::new();
        if self.pending.is_empty() {
            return found;
        }
        let mut pending: Vec<(PaneId, SystemTime)> =
            self.pending.iter().map(|(k, v)| (*k, *v)).collect();
        // Newest spawn first: two panes launched into the same cwd share one
        // session root, and `detect_session` just grabs the newest unclaimed
        // file in its window. Processing oldest-first let an earlier pane's
        // wider window see (and steal) a later pane's not-yet-claimed file,
        // starving that pane of a session id forever (HashMap iteration order
        // made this non-deterministic). Claiming newest-spawned-first mirrors
        // file-creation order, so each pane gets its own file.
        pending.sort_by_key(|(_, since)| std::cmp::Reverse(*since));
        // Drop anything past the give-up horizon before scanning for it.
        // Checked against the pane's own `since` rather than a separate
        // clock so the window means the same thing here as it does in the
        // scan: how long we have been looking for *this* pane's file.
        self.pending.retain(|_, since| {
            now.duration_since(*since).map(|age| age < DETECT_GIVE_UP).unwrap_or(true)
        });
        pending.retain(|(id, _)| self.pending.contains_key(id));
        // Sessions adopted earlier in this poll: `App` persists them only
        // after, but the old in-loop persist made them visible to later
        // panes' taken-set (workspace panes only — a float is not in `ws`).
        let mut adopted: HashSet<String> = HashSet::new();
        for (id, since) in pending.clone() {
            let Some((spec, adapter)) =
                spec_of(id).and_then(|s| registry.get(s.adapter.as_str()).map(|a| (s, a)))
            else {
                self.pending.remove(&id);
                continue;
            };
            // Two panes in *different projects* whose adapter gives them the
            // same session root cannot be told apart by this scan at all:
            // the root carries no cwd signal (codex buckets rollouts by
            // date, `~/.codex/sessions` for every project), so the only
            // thing separating the candidates is mtime order, which says
            // nothing about whose they are. Decline rather than guess.
            //
            // **A wrong session is far worse than no session.** Losing a
            // resume pointer costs the user a `--continue`; attaching a
            // pane to another project's conversation corrupts work in it.
            // Declining leaves the pane pending, so the exact channel (the
            // agent-side extension) can still report it, and the scan
            // retries the moment the ambiguity clears.
            //
            // Same-cwd concurrency is *not* ambiguous in this sense and is
            // deliberately still allowed: both panes really are in that
            // project, and the newest-first ordering above mirrors file
            // creation order so each takes its own.
            let root = adapter.session_root(&spec.cwd);
            let ambiguous = root.is_some()
                && pending.iter().any(|(other, _)| {
                    *other != id
                        && spec_of(*other).is_some_and(|o| {
                            o.adapter == spec.adapter
                                && o.cwd != spec.cwd
                                && registry
                                    .get(o.adapter.as_str())
                                    .and_then(|a| a.session_root(&o.cwd))
                                    == root
                        })
                });
            if ambiguous {
                continue;
            }
            // Session ids already owned by other panes — never re-assign one
            // (concurrent same-cwd launches otherwise cross-wire onto it).
            // D7 widens the set with ids other running instances hold claims
            // on, so detection keeps scanning past anything another workspace
            // is already driving instead of adopting it.
            let mut taken = session_resolver::claimed_sessions(ws);
            taken.extend(self.claims.claimed(&spec.adapter));
            taken.extend(adopted.iter().cloned());
            let Some(session) = adapter.detect_session(&spec.cwd, since, &taken) else { continue };
            // Claim before adopting (D7): between the snapshot above and
            // now another instance may have taken it — then keep scanning
            // on the next tick rather than adopting a claimed session.
            // Unlike the restore conflict (once per spawn), detection
            // retries every tick — latched so a held candidate gets exactly
            // one feed line per pane, not one every tick for as long as the
            // other workspace keeps running.
            match self.claim(id, &spec.adapter, &session) {
                Ok(unavailable) => {
                    self.conflict_latched.remove(&id);
                    if ws.tabs.iter().any(|t| t.panes.contains_key(&id)) {
                        adopted.insert(session.clone());
                    }
                    found.push(Found::Adopted { id, session, unavailable });
                }
                Err(owner) => {
                    if self.conflict_latched.insert(id) {
                        found.push(Found::Held { id, session, owner });
                    }
                }
            }
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::fakes::MemClaims;

    #[test]
    fn forget_pane_drops_every_trace_of_a_recycled_id() {
        let mut d = SessionDetector::new(Box::new(MemClaims::default()));
        d.queue(1, SystemTime::now());
        d.observe(vec![1], vec![], SystemTime::now());
        assert_eq!(d.claim(1, "pi", "s"), Ok(None));
        d.conflict_latched.insert(1);
        d.forget_pane(1);
        assert!(d.pending.is_empty() && d.last_shell_seen.is_empty());
        assert!(d.held.is_empty() && d.conflict_latched.is_empty());
    }
}
