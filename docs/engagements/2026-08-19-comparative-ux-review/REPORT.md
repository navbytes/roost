# Comparative UI/UX + keybinding review — roost vs zellij, lazygit, gh dash

**Date:** 2026-08-19 · **Scope:** read-only review. No code changed.
**Method:** read `DESIGN-ui.md` (C1–C32 + §8 key table), `SPEC-ux.md` (U1–U27),
`ROADMAP.md`, and the live code (`src/ui/input.rs`, `src/ui/render.rs`,
`src/infra/config.rs`, `src/core/app.rs`); then the three comparison tools'
own docs. Two findings were confirmed by running the real chord table, not by
reading it (F1's staleness, F2's cross-terminal split).

**Bottom line.** roost's chrome discipline is ahead of all three tools —
theme inheritance, the fit/yield hint bar, the ≤80-column floor arithmetic, a
key table with a test that the help overlay documents every chord. The gaps
are not in polish. They are in three places the comparison tools each solved
years ago and roost has not yet had to: **the keymap surfaces don't know the
user remapped anything**, **there is no way to declare a fleet**, and **the
config file can only choose among roost's own verbs, never add one**.

Findings are ordered by value-per-cost, not severity.

---

## F1 · High · **RESOLVED 2026-08-19 (C34)** · The help overlay and hint bar hard-code the default chords

**What.** `config.json` can disable or remap any `Alt` chord
(`ui::input::Keymap`, `translate_with` — `input.rs:602`), and `App` holds the
parsed map (`app.rs:697`, `:706`). Neither discoverability surface reads it:

- `HELP_GROUPS` (`render.rs:719`) is a `const &[HelpGroup]` of literal
  `("Alt+f", "floating scratch shell")` pairs.
- `hint_pairs` (`render.rs:120`) returns `Vec<(&'static str, &'static str)>`.

**Consequence.** The README's own escape-hatch example — remapping away from
the `Alt+f` / `Alt+b` / `Alt+d` readline collisions — produces a roost whose
`Alt+?` overlay teaches a chord that no longer works and never names the one
that does. The hint bar's seven curated pairs lie in the same way.

The guard that should catch this cannot — and it is weaker than it looks.
`every_bound_chord_is_documented_in_the_keymap` (`render.rs:3271`) is a
**hardcoded list of 27 chord literals** checked for containment against the
hardcoded `HELP_GROUPS` text. Both sides are `const`; neither is derived from
`default_chord_action`, and `default_keymap()` — the swept table `input.rs`
builds for exactly this purpose — is never referenced outside `input.rs`'s own
tests. So the check proves that one const mentions the strings another const
was written to contain. It says nothing about the running binary under any
configuration, and **a newly bound chord passes it silently** unless someone
remembers to extend the literal list by hand.

This is the D2 finding on mouse verbs, one layer up: a check that structurally
cannot see the thing it exists to catch. It is the only place where roost's
spec discipline can be silently, invisibly wrong.

**Corollary for whatever F2 decides.** Because the gate is const-vs-const, the
F2 fix — under *either* option — can bind four new chords and stay green.
Deriving this gate from the effective table is part of F1's work, and it is
what makes the fix self-enforcing rather than remembered.

**What the others do.** lazygit renders its `?` menu from the live config,
including user-defined custom commands, and lists commands that have *no* key
bound so they stay discoverable. zellij's status bar renders the actual bound
keys for the current mode — remap a key and the bar changes with it.

**What shipped (DESIGN-ui.md C34).** A help row and a hint pair carry
`Action`s, not chord strings, and the chord is resolved through
`input::effective_bindings` — the same default-plus-overrides merge
`translate_with` dispatches on. Group titles, row order, descriptions, the
yield order and the ≤80-column rule are untouched; only the key column became
computed.

Three things the implementation turned up that the finding did not predict:

- **One physical chord had to be collapsed to one entry, three different
  ways** — uppercase-delivery letter twins, shifted-punctuation twins, and
  shift states no match arm ever tested (`Alt+Shift+1` is not a binding, it
  is an unguarded arm). Rules 1 and 2 are mirror images and, left symmetric,
  deleted both halves of every letter pair.
- **A compact family spelling has to give way.** `Alt+←↓↑→ / hjkl` is a
  shorthand for the *default* chords; printed unconditionally it would be
  exactly the stale spelling this finding is about, merely wider. It now
  prints only while every member holds its default, and enumerates otherwise.
- **A row can now vanish silently.** A row naming an unbound action renders
  nothing, where the old literal row stayed visible and merely lied. That is
  quieter, so it got a gate of its own.

**And the rule needed a mechanical check, not just contracts.** The first
implementation covered two surfaces while C34 asserted the rule universally;
a design audit found five more places spelling chords as literals. All seven
were caught by a person reading code. `no_surface_spells_a_chord_it_did_not_resolve`
now scans `src/` for chord literals outside a short allowlist of resolver
defaults, so the next one fails a test instead of needing a reviewer — the
same shape as §2's fixed-hue gate, and sharing its walker.

Then
`every_bound_chord_is_documented_in_the_keymap` can be restated as a real
sweep over the *effective* map (`default_keymap()` overlaid with the config's
overrides) rather than a hand-kept literal list — the assertion that was
always meant, and the one that stops a future chord going undocumented
without anyone noticing.

**Cost note.** Rows become owned strings rather than `&'static str`, so
`help_content_width` / `help_layout` / `hint_pair_cols` take a borrow instead
of a const. Real, but bounded to those three width functions.

---

## F2 · High · **RESOLVED 2026-08-19 (C33)** · `Alt+Shift+hjkl` does two different things on two terminals

**What.** Confirmed by running `default_chord_action` directly:

| delivery form | result |
|---|---|
| `('h', SHIFT)` | `Focus(Dir::Left)` — roost swallows it, focus moves |
| `('H', no shift)` | `None` → `unbound_alt` → meta-`H` forwarded to the pane |

Same for `j`/`J`, `k`/`K`, `l`/`L`. The cause is `input.rs:191–194`: the vim
arms (`KeyCode::Char('l')` etc.) carry no `shift` guard, so a shifted
lowercase delivery falls into the focus arm, while the uppercase delivery
matches nothing at all.

**Why the existing machinery misses it.** `twins` (`input.rs:742`) pairs two
codes exactly when they share a case-folded letter *and both already default
to the same action*. `'H'` defaults to nothing, so `h`/`H` are not twins — the
one mechanism built for this exact hazard is structurally blind to it. That
also means `{"keys": {"alt+shift+h": "resize_horizontal_shrink"}}` binds only
the `('h', SHIFT)` form and silently no-ops on terminals that send `'H'`,
which is precisely the failure `twins`' own doc comment says must never
happen.

**The sharper framing (raised in review).** On the terminals where
`Alt+Shift+l` *does* resolve, it resolves to `Focus(Dir::Right)` — which is
exactly what `Alt+l` already does. So the four shifted vim chords are not
merely inconsistent, they are **spent on nothing**: four chords duplicating
four chords, in a table whose free unshifted pool is empty (F7). That is the
strongest argument for the change, and it is independent of the delivery
split.

It also names the real defect in the vocabulary, which is visible with no
config at all: **shift + a direction key resizes with arrows and moves focus
with letters.** `KeyCode::Right if shift` (`input.rs:123`) is a resize arm and
sits above `KeyCode::Right | KeyCode::Char('l')` (`input.rs:191`), which has
no shift guard — so `Alt+Shift+→` shrinks a pane and `Alt+Shift+l` walks
focus. Same gesture, two verbs. §8 rows 3 and 4 read as though the arrows and
the letters are equal partners; for the shifted half they are not. zellij's
resize mode gives `hjkl` the resize verbs.

**Severity, honestly.** Because the resolving case is a duplicate, nobody has
been silently broken by it — this is not a user-facing bug of the U1 class,
and it should not be sold as one. What it is: a leak into the agent on one
terminal class, a config remap that half-applies on the other, and four dead
chords. The reason to do it early is the ordering dependency below, not
urgency.

**What shipped, and why not resize.** This report first proposed binding the
four letters to *resize*. Reviewed against F8 and rejected: resize already has
the arrows, so that trade spends four scarce chords on a second spelling of an
existing verb. They went to **moving a pane within its tab** instead — F8's
missing capability — as **DESIGN-ui.md C33**.

That is also the better-justified binding. C28 already established that a
shifted sibling *carries the pane* the way the unshifted one *carries you*
(`Alt+i`/`Alt+m` → `Alt+Shift+i`/`Alt+Shift+m`). `Alt+hjkl` moves you within a
tab; `Alt+Shift+hjkl` moving the pane within a tab is the same sentence on the
other axis, so shifted-letter-carries-the-pane becomes a rule holding on both
axes rather than a tab special case. It is C28's idiom generalized, not a new
one.

Both delivery forms are bound, which makes `h`/`H` twins under `twins`' own
existing rule — no change to `twins` — so a `config.json` remap of
`alt+shift+h` can no longer half-apply. Full rationale, including the
deliberate no-cross-tab-handoff-at-the-edge divergence from C31, is in C33.

---

## F3 · High · **RESOLVED 2026-08-19 (C36)** · Broadcast exists, but only outside the TUI

**What.** `roost send --all` is implemented end to end (`control.rs:70`,
`Method::Broadcast`) and is explicitly **CLI-only by design, no TUI key** per
the fleet-features roadmap entry.

**Why it should change.** "Ask all five agents the same question" is the verb
roost is uniquely positioned for, and it is the one thing you must leave roost
to do. zellij's sync-input-to-tab is one of its most-cited features and it
isn't even aimed at agents; roost has the better version of it already built
and unreachable from the keyboard.

**What shipped (C36), and the part the finding got half-right.** Not a sticky
sync mode — that stands, and for the reason given below. But this finding
proposed the guard be "a modal composer" and left it there; building it
surfaced the actual question, which is not *whether* the send is deliberate
but *whether you know who receives it*.

The answer is the title: `broadcast → N panes`, live, moving as `Tab` cycles
the status filter, `accent()` at zero. **The visible blast radius is the
guard**, not a confirm-twice. A second keypress tells you nothing a first
didn't; a count tells you *whom*. Confirm-twice is right for closing one busy
pane ("are you sure"); this question is "sure about whom".

For that to be a guard and not a decoration, the count and the send had to
come from one predicate — `broadcast_targets` — so they cannot disagree. A
guard that lies is worse than none.

Two other things the build settled: `Tab`'s filter is C27's, *extracted and
shared* rather than copied, because a filter meaning something subtly
different in each surface would make the count unreadable; and a keyboard
broadcast needed a third `Actor` variant (`Local`), because logging it as
`fleet` would have made a human's action indistinguishable from a token
holder's in a log whose whole value is attribution.

**The design audit caught five things, one serious.** Worth recording because
the pattern now holds across four consecutive contracts: the composer was not
in `App::modal_active`, which is U8(b) verbatim — a click moved focus *under*
the open dialog and a **paste landed in the pane behind it**. Compose a
fleet-wide message, paste a snippet, and it goes silently into whichever agent
happened to be focused. Also: C32's `Alt+Enter` carve-out was `PaneEdit`-only,
so on Terminal.app the chord that should break a line opened the picker and
discarded the message; the tier glyph was printed without its C5 colour; the
title's conditional `accent()` was unsanctioned by C12; and nothing capped the
line count on a dialog that grows a row per break and never scrolls.

**Original fix shape, for the record.** Not a sticky sync mode. A
persistent "every keystroke goes to five panes" state is exactly the
unguarded-destructive-action shape U1 exists to prevent, and it fails the same
way `Alt+q` did: one forgotten mode indicator and you've typed into a fleet.
Instead: a modal composer — enter, type once with the message visible, `Enter`
sends, `Esc` aborts — reusing C12/C13's dialog machinery. Timer-free and
modal, which the roadmap's own reliability finding names as roost's reliable
feature shape.

Scope it against the roster's existing status filter so "send to the three
panes that are `◆`" is one gesture, not five. That composes two surfaces
already built and is a thing none of the three comparison tools can do at all.

**Chord.** Genuinely contested — see F7. This is a strong candidate for the
roadmap's stated leader-key trigger ("a concrete feature needs a chord and
finds none free").

---

## F4 · High · There is no way to declare a fleet

**What.** `workspace.json` is *recovered state*, not *declared intent*: it
lives in one global state dir, is written by roost, and describes whatever
happened to be open last. There is no file you can check into a repo that
says "this project runs claude in `./api`, pi in `./web`, and a shell", and no
way for two projects to have two different fleets.

**What the others do.** zellij layouts are declarative KDL: tabs, panes,
commands, args, per-pane `cwd` that composes hierarchically with the tab's and
the layout's, plus templates and floating/stacked pane declarations —
`zellij --layout dev` and the workspace materializes. gh dash's config-defined
sections are the same idea aimed at a different domain: the tool's whole
content is a declaration you version and share.

**Why it matters here more than there.** Agent fleets are per-project by
nature — the adapter and the cwd *are* the fleet. roost already carries both
on every `PaneSpec` and already surfaces the cwd axis in C14's recent-cwd
column. And this subsumes the parked "persistent fleet rail" cleanly: a
per-project fleet file gives you the project → agent axis without spending a
single column of chrome, which was that entry's honest cost.

**Fix shape.** A discovered-upward `roost.toml` (or `.roost.json`) declaring
tabs → panes → `(adapter, cwd, name, initial input)`; adopt it when roost
starts in a directory that has one and has no live workspace for it;
resurrection stays `workspace.json`'s job, keyed per project root.

**The tension, stated honestly.** roost's stance is zero-config with exactly
one escape hatch, and that stance is load-bearing — it is why `config.json` is
keybindings-only and why theming was refused. A layout file is a second
config surface. The counter-argument is that a layout file is not
*configuration* (it changes no behavior) but *content* — the fleet itself,
the way a `Makefile` is not a config file. That distinction is the decision to
make; it should be made deliberately, not by default.

---

## F5 · Medium · `config.json` can only choose among roost's verbs, never add one

**What.** `NAMES` (`input.rs:627–675`) is a closed list of ~30 built-in
actions. A chord can be moved or disabled; nothing new can be bound.

**What the others do.** Both non-multiplexers converged on the same answer.
lazygit's `customCommands` bind a key in a given context to a shell command
templated over the selected object (`{{.SelectedLocalBranch}}`, …), with four
prompt kinds (input, confirm, menu, menuFromCommand), an output destination
(terminal / popup / log / none), and a description that shows up in the `?`
menu. gh dash binds keys per section to shell commands templated over the
selected row (`{{.RepoName}}`, `{{.PrNumber}}`, `{{.HeadRefName}}`, …).

**Why roost is unusually well set up for it.** The selected object is a pane,
and a pane already carries `(id, adapter, cwd, status, name, note)` — a richer
template context than either tool has. The verbs already exist and are already
audited: the control CLI speaks `send` / `read` / `spawn` / `close` / `wait`,
and every call is written to `control.log`. A `"commands"` block binding
`alt+x` → `roost send {pane} "run the tests" --enter`, or → a shell command
whose output lands in the C22 float, is a large capability increase over a
small, well-fenced config addition.

It also relieves the keyspace pressure that F3 and F7 run into, because the
user picks which free chord to spend rather than roost rationing them
centrally.

**Fences worth writing into the spec up front:** config.json is user-owned and
local, the same trust level as a shell rc — the audit log already records what
a bound command does. Commands should be spelled out in the help overlay via
their `description` (lazygit's rule), which F1's fix makes free.

---

## F6 · Medium · **RESOLVED 2026-08-19 (C35)** · Nothing takes you back

**What.** Every navigation chord is absolute or forward-directional:
`Alt+1..9`, `Alt+0`, `Alt+i`/`Alt+m`, `Alt+hjkl`, `Alt+a`. There is no
alternate-pane or alternate-tab toggle. `prev_focus` exists only inside
`Float` (`app.rs:4202`, `:4649`) — the float's return address, not a general
one — and U11's per-tab focus memory remembers where you were *in a tab*, not
which pane you were on before this one.

**Why it matters at fleet scale.** The dominant motion in a fleet is "check on
B, come back to A". `Alt+a` walks *forward* through the attention ring and
never home; on a two-pane hop it is the wrong tool, and on a tab switch there
is nothing at all. tmux has had `prefix ;` (last pane) and `prefix l` (last
window) forever; vim has `Ctrl-^`; lazygit gives every panel a digit *and*
`[`/`]`.

**What shipped (C35), and the premise that turned out wrong.** This finding
said "the expensive part is the chord, not the state", and F7 was queued ahead
of it to pay that cost. **The chord was free.** §8's pool accounting — the one
this report quoted to argue the keyspace was exhausted — enumerates *letters*.
Outside `Alt+/` and `Alt+?`, every punctuation chord was untouched
(`;` `'` `,` `.` `]` `-` `=` `` ` ``). `Alt+[` stays rejected (`ESC [` is the
CSI introducer), but nothing else had ever been counted.

So `Alt+;` — which is also tmux's own last-pane binding, so the muscle memory
exists, and collides with no readline op. **F7 is not a prerequisite for F6,
and the leader key's trigger has not fired here.**

The state was smaller than expected too: `set_focus` is already a documented
single chokepoint ("every focus move funnels through here"), so one field
maintained there gives a complete trail without the next focus-moving feature
having to remember it. It reuses `focus_attention_target` for the landing, so
cross-tab returns, stack expansion and the float all behave exactly as
`Alt+a`'s jump does.

One thing the build turned up: `focused` initializes to a `0` sentinel naming
no pane, so recording unconditionally filed a phantom and the first press
reported a pane that never existed. Existence is now checked at both ends by
one shared predicate — two ends that disagree about what "exists" means is how
the phantom gets back in.

---

## F7 · Medium · The free-chord pool is the real constraint, and it's nearly empty

**What.** §8's own accounting leaves `b d p v x y PgDn` unshifted, of which
`b`/`d` are protected readline word ops (and since U5 they genuinely reach the
shell, so taking them is a live regression, not a theoretical one), `p` is
deliberately kept free as `Alt+Shift+p`'s near-miss guard, and `v`/`x`/`y` are
reserved for copy-mode vocabulary. That is **zero** comfortably-available
unshifted chords, against three findings above that each want one (F3
broadcast, F6 back, F5 user commands).

**The relevant precedent.** The roadmap defers the single modal leader key
until "a concrete feature needs a chord and finds none free", with a full
design already assessed as reliable (`Mode::Leader`, flat config table, `Esc`
aborts, no wall-clock timer). **That trigger has now fired three times over.**
Note also that the `v`/`x`/`y` reservation is softer than it reads: those are
*mode-local* copy-mode keys, not Alt chords, so reserving their Alt forms
costs three chords to protect a namespace that was never actually shared.

**What the others do.** zellij's answer is modes — a `Ctrl+<mode>` prefix
opens a whole keyspace and the status bar reprints itself for it. That is not
the right answer for roost (roost's flat Alt layer is *better* for agent CLIs
precisely because there's no prefix to swallow the agent's own keys), but it
is why zellij never runs out of room. The leader-key design already on the
roadmap is roost's version of the same escape valve, scoped to one modal hop.

**Recommendation, amended 2026-08-19 — this finding overstated its case.**
The pool accounting it quotes covers **letters**. Punctuation was never
counted, and outside `Alt+/` and `Alt+?` all of it was free; C35 (`Alt+;`)
took one and the letter pool is untouched. So "zero comfortably-available
chords" was wrong, and two of the three findings said to be blocked on F7
were not: F6 shipped without it, and F5's whole point is that the *user*
picks a chord.

What survives: F3 (broadcast) still wants a chord, and the leader key remains
the right answer once punctuation runs thin or a feature needs a whole
keyspace rather than one key. Its trigger — "a concrete feature needs a chord
and finds none free" — has **not** fired. Build it when it does, with the
design the roadmap already assessed, and not before; that is what the roadmap
entry said, and this report was wrong to override it on a miscount.

---

## F8 · Medium · **RESOLVED 2026-08-19 (C33)** · You cannot rearrange panes inside a tab

**What.** `layout.rs` has `remove_pane`, split/stack ops, orientation flip and
the C25 canned cycle — but no swap or move-within-tab. `Action::MovePaneToTab`
(C28) moves a pane *between* tabs; nothing moves it *within* one. So in
main+stack you cannot promote the agent you care about into the main slot
except by closing and respawning it.

**What the others do.** zellij has a whole move mode (`Ctrl+h`) for exactly
this. lazygit has directional reordering where reordering makes sense
(commits).

**What shipped.** Swap-with-neighbour in all four directions, on
`Alt+Shift+hjkl` — the chords F2 freed. `layout::swap_panes` exchanges two
`PaneId`s and leaves the tree's shape bit-identical (same split directions,
same ratios, same stack arity), so it needs no ratio arithmetic and can reach
no layout a pair of splits couldn't already build. Direction comes from
`layout::neighbor` — the same call `focus_dir` makes — so "which pane is that
way?" keeps exactly one answer in roost.

Stacks fell out with no special case: every member already owns a `PaneRect`,
so `Down` inside a stack reorders it. The one thing that did need handling is
that `Stack`'s `expanded` is an index rather than an id, so the expanded slot
follows its pane — otherwise "move down" would collapse the pane you are
reading. Focus follows for free (the focused id never changes, it just
occupies a different slot).

Left for later, deliberately: drag-to-swap, and the cross-tab handoff at a
tab's edge. See C33 for why the edge is a no-op rather than inheriting C31's
continue-into-the-next-tab rule.

---

## F9 · Low · `Alt+?` neither filters nor knows where you are

**What.** The overlay is one global scrolling table. roost already has
type-ahead filtering in two other dialogs (C14's picker per U20, C27's roster)
and `/` search inside scroll mode (P21) — the overlay is the one list you
cannot narrow.

**What the others do.** lazygit's keybinding menu is filterable *and* scoped
to the current context, so "what can I do right here" is one key.

**The tension.** C15's contract is "any key closes it", which forbids free
typing. But that exact tension was already resolved once, for scrolling: the
carve-out is conditional, and the overlay says so in its own title when it
applies. The same resolution works here — `/` opens a filter (matching scroll
mode's own idiom rather than inventing one), the title carries the query the
way C14's does, `Esc` clears then closes, and every other key still dismisses.

**Cheaper alternative that keeps the contract untouched:** order the groups by
context. A dead-focused pane's overlay leads with the dead-pane verbs; copy
mode leads with READING. No new keys, no carve-out.

---

## F10 · Low · **RESOLVED 2026-08-19 (C37)** · `Alt+g` cycles one way only

Three layouts, forward-only, so the one you want can be two presses away.
Every other cycle in roost goes both ways — `Alt+i`/`Alt+m` for tabs,
`Tab`/`Shift+Tab` for the roster's status filter — and C28 established the
idiom that a shifted chord is its unshifted sibling's inverse. `Alt+Shift+g`
is free and means exactly one thing. zellij's swap-layout keys
(`Alt+[` / `Alt+]`) are bidirectional for the same reason.

**What shipped (C37).** The arithmetic is the interesting part: `layout_cycle`
holds the arrangement to try *next going forward*, so backward starts two
behind it — `lc+1, lc, lc-1`, the forward order reversed. Both directions share
C25's skip-what-doesn't-fit rule.

Two things the build corrected in my own first attempt. The inverse property
only holds **from inside the ring**: a tab's layout before the first `Alt+g`
isn't one of the three canned arrangements, so stepping back from the first
wraps to the last rather than restoring a custom layout — it is a ring, not an
undo stack. And arrangements compare by **shape, not tree**, because
`arrangement_for` re-derives from the live `pane_order`, which a previous step
can have reordered; my first test compared whole trees and failed on pane
placement rather than on arrangement.

Also: merging both directions onto one help row measured **83 columns** and
blew the 80-col floor that two prior audits found sitting at exactly 80. Two
rows instead — which is also the more correct presentation, since C28's
adjacency rule seats a row under its own unshifted form. C15's row cap was
retired in July precisely so rows need not be merged to fit; this is the first
change to take that at its word.

---

## F11 · Low · **RESOLVED 2026-08-19** · No `roost keys` / no config validation from the shell

`config.json` diagnostics surface only as a startup toast in the TUI
(`config.rs:load_keymap`), so a mistyped chord in a dotfile is discovered by
launching roost and reading a transient message. zellij has
`zellij setup --check` and `--dump-config`; lazygit has `lazygit --config`.

**What shipped.** `roost keys` prints the effective table as TSV
(`chord<TAB>action`, with a third column naming `config.json` for anything it
changed), and exits 2 with the diagnostics on stderr when an entry had to be
skipped. Disabled chords are listed as `disabled` rather than omitted — "why
doesn't `Alt+f` work any more" is the question the command gets asked, and a
table of live bindings structurally cannot answer it.

It is deliberately **not** a control verb: every one of those is a socket
request against a running roost, and this one reads `config.json` directly.
Routing it through the socket would make the only question you can ask about a
config require the program that config configures to already be running.

As predicted, F1 made it a printer rather than a feature — `effective_bindings`
was the whole of the hard part. Two things the build turned up: `action_name`
had to be promoted out of `#[cfg(test)]` (F1 promoted `default_keymap` for the
same reason), and the first output padded chords into columns, which quietly
broke the TSV contract the help text advertises — `cut -f1` would return
`"Alt+q     "`. Padding is what `column -t` is for; a caller who wants fields
cannot un-pad them.

---

## Simulation pass — 2026-08-20

Four agents drove the real binary in real PTYs against the shipped
contracts: every chord in the table, the CLI and `config.json` surface, the
new fleet features, and small terminals under resize.

**One report came back as a critical bug and was wrong.** The claim: `Alt+q`
does not quit while any modal is open — "100% reproducible across all three
modal types", the process hanging until force-killed. It reproduces on
nothing. The harness pressed the modal-opening chord **100 ms after spawn**,
before roost had drawn a first frame, so no modal was ever open; its own
screenshots would have shown `NORMAL`. Then it gave the quit a **1 s**
deadline, while `quit_and_wait` re-presses at 700 ms to answer U1's
busy-fleet confirm and a real quit here takes 0.9–1.9 s. Two independent
timing mistakes, compounding into a confident false positive.

Three things follow, and only one of them is about the report.

1. **Verify before acting.** The claim contradicted a contract on its face —
   C15 says *any* key dismisses the help overlay — which was the tell. A
   ten-minute reproduction in the main tree settled it. An agent's
   confidence ("VERY HIGH") is not evidence, and a failing test is evidence
   only about the test until you have read it.
2. **The rule it accused was genuinely untested.** "An Alt chord always
   leaves the mode" was enforced by one `if` in `handle_mode_key`, asserted
   nowhere, and already carved out once (C36's `Alt+Enter`). A false report
   found a real gap, which is worth more than the report costing nothing.
   Now contracted as an amendment to **C24b** — which turned out to own the
   rule already, in one under-specified bullet; the design audit caught the
   amendment landing in C12 and duplicating it — and pinned three ways: a
   1,111-probe sweep of every chord against every mode, an exhaustive
   `match` that breaks the build when a mode is added, and
   `tests/modal_quit.rs` driving five modals in a real PTY. The same audit
   found the amendment's own summary sentence false (`Alt+Enter` is
   consumed but does **not** leave the mode — it is a line break) and
   surfaced an open behaviour question underneath it: an Alt chord discards
   an editor's unsaved buffer, which U8 forbids the mouse from doing.
   Recorded in DESIGN-ui.md §7, not guessed at.
3. **A modal test must prove the modal opened.** `tests/modal_quit.rs`
   asserts its marker is *absent* before the chord and present after —
   so a test that reaches no overlay fails loudly instead of quietly
   reporting on Normal mode. The mode word (`HELP`, `ROSTER`, `BROADCAST`)
   is the marker precisely because overlay body text like "keys" or "shell"
   also sits in the always-visible hint bar.

Everything else the pass touched — 35 chords, rapid-fire sequences, orphan
cleanup, 50+ CLI and config cases — came back clean.

---

## What roost already does better — worth not regressing

Recorded so a future pass doesn't "improve" these toward the comparison tools:

- **No prefix key.** roost owns only `Alt`; everything else reaches the agent
  raw. zellij's `Ctrl`-prefixed modes would eat the agent's own bindings, and
  its own docs need a whole page on unbinding them. This is the correct
  trade for an agent multiplexer and should stay.
- **Theme inheritance.** All three comparison tools ship fixed palettes and
  ask you to re-theme them. roost's chrome is the terminal's own ink and
  paper, correct across a live theme switch with no detection.
- **The hint bar's fit/yield order.** A cheat row that drops whole pairs from
  the right, keeps the mode word last, and lets the fleet signal outrank
  static hints. zellij's status bar truncates; lazygit's footer is fixed.
- **`◆ N needs you` + `Alt+a`.** A cross-tab attention ring with a
  right-segment counter that teaches its own chord at the moment of need.
  Nothing in the three has an equivalent, because nothing in the three has
  panes that ask for you.
- **The audit trail.** Every chord's rationale, every rejection, and the test
  that pins it. It is why this review could be specific.

---

## Suggested order

**Done.** F10 as **C37** (`Alt+Shift+g` reverses the layout cycle). F3 as **C36** (`Alt+'` broadcast composer). F6 as **C35** (`Alt+;` goes back). F11 as `roost keys`. F2 + F8 as **C33** (`Alt+Shift+hjkl` moves a pane within its tab —
one change closed both, since the redundant chords F2 found were the chords
F8's missing verb needed). F1 as **C34** (the chrome reads the live keymap),
which also retired the const-vs-const documentation gate for a real sweep.

**Next.**

1. **F4** (declare a fleet) — largest, and the one that needs a stance
   decision before any code.
2. **F5** and **F9** as capacity allows. F5 (custom commands) got
   cheaper for the same reason F11 did: `effective_bindings` is the table it
   needs, and it exists now.

## Sources

- lazygit — [custom command keybindings](https://github.com/jesseduffield/lazygit/blob/master/docs/Custom_Command_Keybindings.md)
- zellij — [keybindings](https://zellij.dev/documentation/keybindings.html), [layouts](https://zellij.dev/documentation/creating-a-layout.html), [keybinding presets](https://zellij.dev/documentation/keybinding-presets.html)
- gh dash — [custom keybindings](https://www.gh-dash.dev/configuration/keybindings/), [configuration](https://www.gh-dash.dev/configuration/)
