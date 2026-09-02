//! Keep agent-CLI status integrations in sync with this build.
//!
//! roost's status/session reporting depends on each agent CLI being wired to
//! roost's socket. Because roost doesn't ship a package for either, users
//! would otherwise wire it up by hand — and it then silently rots when
//! roost's socket protocol changes (e.g. the per-pane token, which roost now
//! *requires*: a stale integration's messages are dropped).
//!
//! On startup we install/update both automatically, but only when the tool
//! is actually set up (we never create `~/.pi` or `~/.claude` ourselves), and
//! only when the on-disk copy is missing or differs. Two independent knobs
//! suppress this entirely — see `ext_install_disabled`:
//! - `ROOST_NO_EXT_INSTALL`: user-facing — "I manage this myself".
//! - `ROOST_TEST_NO_HOST_IO` (`infra::host_io_disabled`): machine-facing —
//!   "this run must not touch anything outside itself", already set on every
//!   roost `tests/harness/mod.rs` spawns.
//!
//! All three write into the operator's real, often dotfiles-managed
//! `~/.pi`/`~/.claude`/`~/.config/opencode` regardless of `ROOST_STATE` — an
//! isolated *workspace*
//! is not an isolated *machine*, so `ROOST_STATE` alone does not suppress
//! this (deliberate: someone running two concurrent, equally-real roost
//! fleets on one box, each with its own `ROOST_STATE`, still wants both
//! wired to the one real Claude Code config on that machine; overloading
//! `ROOST_STATE` to also mean "and don't touch my global config" would
//! silently break that, with no opt-in back on). `ROOST_TEST_NO_HOST_IO`
//! already exists to mean exactly "don't touch anything outside this
//! process" — that's the knob for a harness/orchestrator that wants
//! isolation, not a second meaning grafted onto `ROOST_STATE`.
//!
//! - pi: a single file we own outright (`~/.pi/agent/extensions/roost.ts`) —
//!   compare-and-overwrite, see `ensure_pi_extension`.
//! - Claude Code: three hook entries inside `~/.claude/settings.json`, a file
//!   the *user* owns and may already have their own content in — merge, not
//!   overwrite, see `ensure_claude_hooks`.
//! - opencode: a single file we own outright
//!   (`~/.config/opencode/plugin/opencode-plugin.ts`) — compare-and-overwrite,
//!   see `ensure_opencode_plugin`. Unlike the two above, creating the
//!   `plugin/` subdir *is* part of the install: opencode auto-globs that dir,
//!   so there is no existing file to merge into. The config dir itself still
//!   must exist — a user who never ran opencode has no `~/.config/opencode`,
//!   and we never create that.

use std::path::{Path, PathBuf};

/// Whether any install path (`ensure_pi_extension`, `ensure_claude_hooks`,
/// `ensure_opencode_plugin`) must no-op — see the module doc for what each of
/// the two knobs means.
/// Checked before either function even resolves `dirs::home_dir()`, not just
/// before it writes: the bug this exists to fix was a real, global
/// `~/.claude/settings.json` getting mutated by a run that believed
/// `ROOST_STATE`+`ROOST_TEST_NO_HOST_IO` made it isolated.
fn ext_install_disabled() -> bool {
    std::env::var_os("ROOST_NO_EXT_INSTALL").is_some() || super::host_io_disabled()
}

/// The extension source, embedded at build time so the binary is self-contained.
const BUNDLED: &str = include_str!("../../extensions/roost.ts");

/// Ensure `~/.pi/agent/extensions/roost.ts` matches this build. Returns a short
/// message to surface when it installed or updated the file, else None.
pub fn ensure_pi_extension() -> Option<String> {
    if ext_install_disabled() {
        return None;
    }
    let agent_dir = dirs::home_dir()?.join(".pi").join("agent");
    // Only touch things when pi is present — never create ~/.pi ourselves.
    if !agent_dir.is_dir() {
        return None;
    }
    let ext_dir = agent_dir.join("extensions");
    let target = ext_dir.join("roost.ts");

    let existing = std::fs::read_to_string(&target).ok();
    if existing.as_deref() == Some(BUNDLED) {
        return None; // already current
    }
    let updating = existing.is_some();

    std::fs::create_dir_all(&ext_dir).ok()?;
    write_atomic(&target, BUNDLED)?;

    Some(if updating {
        "updated the roost pi extension to match this build".into()
    } else {
        "installed the roost pi extension (~/.pi/agent/extensions/roost.ts)".into()
    })
}

/// Ensure `~/.claude/settings.json` has roost's three status hooks (see
/// `extensions/claude-code-hooks.md`), merged into whatever's already there.
/// Returns a short message when it installed, updated, or refused to touch
/// the file; None when nothing changed. Mirrors `ensure_pi_extension`'s
/// contract (same opt-out, same "only when the tool is set up" rule, same
/// "message only on an actual change" shape) — the difference is `settings.
/// json` is user-owned, so this merges instead of overwriting; see
/// `merge_claude_hooks`.
pub fn ensure_claude_hooks() -> Option<String> {
    if ext_install_disabled() {
        return None;
    }
    let claude_dir = dirs::home_dir()?.join(".claude");
    // Only touch things when Claude Code is present — never create ~/.claude.
    if !claude_dir.is_dir() {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    // A non-UTF-8 exe path can't be embedded correctly in a shell command —
    // to_string_lossy would silently mangle it into a path that can never
    // execute. Bail rather than install something broken.
    let exe = exe.to_str()?;
    merge_claude_hooks(&claude_dir.join("settings.json"), exe)
}

/// The opencode plugin source, embedded at build time like `BUNDLED`.
const BUNDLED_OPENCODE: &str = include_str!("../../extensions/opencode-plugin.ts");

/// Ensure `~/.config/opencode/plugin/opencode-plugin.ts` matches this build —
/// the plugin that reports opencode's session id over roost's socket (see
/// `extensions/opencode-plugin.ts`; without it roost has no way to learn the
/// id, since opencode keeps sessions in one global SQLite database with no
/// directory to scan). Returns a short message to surface when it installed
/// or updated the file, else None.
///
/// Like the pi extension, this is a file roost owns outright — no merge —
/// with one difference: the `plugin/` subdir is created on demand, because
/// opencode auto-globs it and a working install needs nothing else in it.
/// Only when opencode is set up: `~/.config/opencode` itself must already
/// exist (we never create it — a user who never ran opencode would get a
/// config dir that exists for nothing).
pub fn ensure_opencode_plugin() -> Option<String> {
    if ext_install_disabled() {
        return None;
    }
    let config_dir = dirs::home_dir()?.join(".config").join("opencode");
    // Only touch things when opencode is present — never create
    // ~/.config/opencode ourselves.
    if !config_dir.is_dir() {
        return None;
    }
    install_opencode_plugin(&config_dir)
}

/// The real work of `ensure_opencode_plugin`, split out (like
/// `merge_claude_hooks`) so it's independently testable against a tempdir.
/// `config_dir` is opencode's config directory (the one containing
/// `opencode.json`), not the `plugin/` subdir itself.
fn install_opencode_plugin(config_dir: &Path) -> Option<String> {
    let plugin_dir = config_dir.join("plugin");
    let target = plugin_dir.join("opencode-plugin.ts");

    let existing = std::fs::read_to_string(&target).ok();
    if existing.as_deref() == Some(BUNDLED_OPENCODE) {
        return None; // already current
    }
    let updating = existing.is_some();

    std::fs::create_dir_all(&plugin_dir).ok()?;
    write_atomic(&target, BUNDLED_OPENCODE)?;

    Some(if updating {
        "updated the roost opencode plugin to match this build".into()
    } else {
        "installed the roost opencode plugin (~/.config/opencode/plugin/opencode-plugin.ts)".into()
    })
}

/// The Claude Code hook events roost wires up, and the status each reports.
/// PreToolUse fires at the start of a turn (working), Stop at the end
/// (waiting), Notification on Claude Code's own permission/idle prompts
/// (needs_input) — see `extensions/claude-code-hooks.md`.
const HOOK_EVENTS: &[(&str, &str)] =
    &[("PreToolUse", "working"), ("Stop", "waiting"), ("Notification", "needs_input")];

/// Appended as a trailing shell comment on every hook command roost installs
/// (`hook_command`), so `is_roost_hook` can recognize its own entries
/// without matching on the internal `__status` verb name alone — a user's
/// own unrelated hook could plausibly invoke a differently-owned script that
/// also happens to be named `__status`. `command` always runs through a
/// shell, and `#` starts a comment to end of line wherever it appears
/// outside quotes, so this is inert as far as execution goes.
const HOOK_SENTINEL: &str = "roost-status-hook";

/// Identifies a hook command as roost's own, independent of the (variable)
/// binary path — so a build whose binary moved, or an older hand-copied
/// hook from before this build, is recognized and replaced in place rather
/// than duplicated:
/// - `HOOK_SENTINEL`: this build's command, tagged explicitly (see above).
/// - `ROOST_SOCK`: any earlier hand-copied `nc`/`socat` hook from
///   `extensions/claude-code-hooks.md`, which referenced it directly.
///
/// ponytail: substring match, not a real parser — fine because a false
/// match now requires a user's own hook to literally contain roost's
/// sentinel or its env var name, not just an internal verb name.
fn is_roost_hook(command: &str) -> bool {
    command.contains(HOOK_SENTINEL) || command.contains("ROOST_SOCK")
}

/// Merge roost's status hooks into the settings file at `settings_path`,
/// pointing each hook's command at `exe`. Returns a short message on any
/// actual change (installed, updated, or refused-to-touch), None when
/// nothing changed. Takes an explicit path (rather than resolving `~/.claude`
/// itself) so it's independently testable against a tempdir.
///
/// Safety contract, in order:
/// - Missing file → treated as `{}` (first install).
/// - A read failure (permissions, IO error), malformed JSON, or valid JSON
///   that isn't an object are refused and reported distinctly — never
///   guessed at or overwritten. A malformed file keeps failing (and keeps
///   saying so) on every future start until it parses; the message says
///   that plainly rather than nagging with no explanation.
/// - A `.bak` of the pristine (pre-roost) file is written once, before the
///   first modification ever made to it.
/// - Every unrelated key, and every non-roost hook entry, round-trips with
///   its value unchanged (`preserve_order` on serde_json keeps key order
///   too) — though the file is reformatted through serde_json's own
///   pretty-printer, so a hand-tab-indented file will show a whitespace-only
///   diff even when nothing semantic changed.
/// - A failure to serialize or write is reported, not swallowed — silence
///   here would read as "already installed" when it might mean "disk full".
/// - Written atomically via `write_atomic` (temp file + rename, symlink- and
///   concurrency-safe, preserves the original file's permissions).
fn merge_claude_hooks(settings_path: &Path, exe: &str) -> Option<String> {
    let raw = match std::fs::read_to_string(settings_path) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Some(format!(
                "roost: couldn't read {} ({e}) — leaving Claude Code hooks untouched.",
                settings_path.display()
            ));
        }
    };
    let mut root: serde_json::Value = match &raw {
        None => serde_json::json!({}),
        Some(s) => match serde_json::from_str(s) {
            Ok(v) => v,
            Err(e) => {
                return Some(format!(
                    "roost: {} isn't valid JSON ({e}) — leaving it untouched. Claude Code hooks \
                     won't auto-install until this parses (roost rechecks on every start); fix \
                     it, or add the hooks by hand (see extensions/claude-code-hooks.md).",
                    settings_path.display()
                ));
            }
        },
    };
    let Some(obj) = root.as_object_mut() else {
        return Some(format!(
            "roost: {} is valid JSON but not an object at the top level — leaving it untouched.",
            settings_path.display()
        ));
    };

    let mut inserted = false;
    let mut updated = false;
    for (event, status) in HOOK_EVENTS {
        match upsert_hook(obj, event, &hook_command(exe, status)) {
            HookChange::Inserted => inserted = true,
            HookChange::Updated => updated = true,
            HookChange::Unchanged => {}
        }
    }
    if !inserted && !updated {
        return None;
    }

    // Snapshot the file exactly as the user left it, once, before roost ever
    // writes to it — best effort, never blocks the install. The snapshot
    // inherits the original's mode rather than the umask default: settings.json
    // can carry an `env` block or `apiKeyHelper`, so a 0644 copy of a 0600 file
    // would leak exactly what the original was locked down to protect.
    if let Some(raw) = &raw {
        let bak = settings_path.with_extension("json.bak");
        // Durable, not just written: this snapshot is the entire recovery
        // story for the merge below, and a copy that never reached the disk
        // is worth nothing on exactly the crash it exists for.
        if !bak.exists() && write_durable(&bak, raw.as_bytes()).is_ok() {
            if let Ok(meta) = std::fs::metadata(settings_path) {
                let _ = std::fs::set_permissions(&bak, meta.permissions());
            }
        }
    }

    let out = match serde_json::to_string_pretty(&root) {
        Ok(s) => s,
        Err(e) => {
            return Some(format!(
                "roost: couldn't serialize the merged Claude Code hooks ({e}) — {} left untouched.",
                settings_path.display()
            ));
        }
    };
    if write_atomic(settings_path, &out).is_none() {
        return Some(format!(
            "roost: couldn't write the Claude Code hooks to {} (disk full? read-only?) — left \
             untouched.",
            settings_path.display()
        ));
    }
    Some(if inserted {
        format!("installed the roost Claude Code hooks ({})", settings_path.display())
    } else {
        "updated the roost Claude Code hooks to match this build".into()
    })
}

enum HookChange {
    Inserted,
    Updated,
    Unchanged,
}

/// Insert or update roost's hook for `event` within `root["hooks"][event]`
/// (an array of `{"hooks": [{"type": "command", "command": ...}]}` groups —
/// Claude Code's hook-config shape). Finds roost's existing entry anywhere in
/// that array via `is_roost_hook` and replaces its command in place; appends
/// a new group only if none is found. Never touches any other entry, group,
/// or key.
///
/// The equality check below compares the *whole* command, including the exe
/// path — so two different builds of roost alternating on one machine (e.g.
/// a `cargo run` dev binary and an installed one) will each see the other's
/// entry as stale and rewrite it, every launch, forever. That's a deliberate
/// tradeoff, not an oversight: the alternative is a hook that never notices
/// a genuinely moved/reinstalled binary. Harmless on its own (the write
/// underneath is symlink-safe, pid-safe, and mode-preserving — see
/// `write_atomic`), just a repeated flash + file touch in that one setup.
fn upsert_hook(
    root: &mut serde_json::Map<String, serde_json::Value>,
    event: &str,
    command: &str,
) -> HookChange {
    let hooks = root.entry("hooks").or_insert_with(|| serde_json::json!({}));
    let Some(hooks) = hooks.as_object_mut() else { return HookChange::Unchanged };
    let arr = hooks.entry(event).or_insert_with(|| serde_json::json!([]));
    let Some(arr) = arr.as_array_mut() else { return HookChange::Unchanged };

    for group in arr.iter_mut() {
        let Some(entries) = group.get_mut("hooks").and_then(|h| h.as_array_mut()) else {
            continue;
        };
        for entry in entries.iter_mut() {
            let is_ours = entry.get("command").and_then(|c| c.as_str()).is_some_and(is_roost_hook);
            if !is_ours {
                continue;
            }
            if entry.get("command").and_then(|c| c.as_str()) == Some(command) {
                return HookChange::Unchanged;
            }
            if let Some(o) = entry.as_object_mut() {
                o.insert("command".into(), serde_json::json!(command));
            }
            return HookChange::Updated;
        }
    }
    arr.push(serde_json::json!({ "hooks": [{ "type": "command", "command": command }] }));
    HookChange::Inserted
}

/// The shell command a hook entry runs: roost's own binary, invoked with the
/// internal `__status` subcommand (see `cli.rs`) instead of piping through
/// `nc`/`socat` — no netcat dependency, identical on macOS and Linux.
/// Claude Code runs `command` through a shell, so the path is quoted; the
/// trailing comment is `is_roost_hook`'s marker, not read by anything.
fn hook_command(exe: &str, status: &str) -> String {
    format!("{} __status {status} # {HOOK_SENTINEL}", shell_quote(exe))
}

/// POSIX-safe single-quoting: wrap in `'...'`, escaping embedded `'` as
/// `'\''`. Handles spaces in the exe path (common on macOS, e.g. under `~/Library`).
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Write via a temp file + rename so a crash can't leave a half-written file
/// an agent or Claude Code would try to load.
///
/// `target` may be a symlink — dotfiles-managed configs commonly are (e.g.
/// `~/.claude/settings.json -> ~/repos/dotfiles/claude/settings.json`).
/// `fs::rename` replaces whatever name it's given: renaming onto a symlink
/// replaces the *link itself* with a plain file, silently orphaning whatever
/// it pointed at (the dotfiles copy stops being live, with nothing to show
/// for it — `git status` there stays clean). So: resolve the link first —
/// if `target` doesn't exist yet, `canonicalize` fails and we fall back to
/// `target` itself, since there's nothing to resolve — and read/write/rename
/// against the resolved path. The tmp file is named from that same resolved
/// path, so it always lives in the same directory as the real target: the
/// rename can never cross a filesystem (no EXDEV), by construction rather
/// than by handling the error after the fact.
///
/// The tmp name also carries this process's pid, so two roosts racing each
/// other never share a tmp file — each truncates/writes/renames its own
/// instead of one clobbering bytes mid-write into the other's fd.
///
/// A fresh tmp file doesn't inherit the target's mode, so its permissions
/// are copied over explicitly before the rename — otherwise an existing
/// 0600 file (settings.json can hold `env`/`apiKeyHelper` secrets) would be
/// silently downgraded to whatever the process umask produces.
///
/// And the bytes are fsynced before the rename, for the reason
/// `infra::store::FsStore::save` spells out for `workspace.json`: without a
/// real durability barrier the rename can reach the disk before the data
/// does, so a power cut or kernel panic leaves the target truncated or
/// zero-length. That file is `~/.claude/settings.json` — the user's own
/// config, hooks, `env` and `apiKeyHelper`, which roost is a guest in — so
/// the outcome is worse here than it was for roost's own state. The
/// directory's own fsync stays best-effort on the same reasoning as there:
/// losing it means the *previous* contents come back, which is intact.
fn write_atomic(target: &Path, contents: &str) -> Option<()> {
    let real = std::fs::canonicalize(target).unwrap_or_else(|_| target.to_path_buf());
    let mut tmp = real.as_os_str().to_os_string();
    tmp.push(format!(".{}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp);
    write_durable(&tmp, contents.as_bytes()).ok()?;
    if let Ok(meta) = std::fs::metadata(&real) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    let renamed = std::fs::rename(&tmp, &real);
    if renamed.is_err() {
        let _ = std::fs::remove_file(&tmp); // don't litter a pid-tagged tmp file behind on failure
        return None;
    }
    if let Some(dir) = real.parent() {
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Some(())
}

/// Write `bytes` to `path` and fsync them. **Not** `Write::flush`, which on
/// a `std::fs::File` is a no-op that reads like a durability barrier and is
/// not one — the same trap `infra::store` documents.
fn write_durable(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut f = std::fs::File::create(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("roost-claude-hooks-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn hook_commands(root: &serde_json::Value, event: &str) -> Vec<String> {
        root["hooks"][event]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|g| g["hooks"].as_array().into_iter().flatten())
            .map(|h| h["command"].as_str().unwrap().to_string())
            .collect()
    }

    fn command_for(exe: &str, status: &str) -> String {
        format!("'{exe}' __status {status} # {HOOK_SENTINEL}")
    }

    /// No stray `*.tmp` file (any naming scheme) left behind in `dir`.
    fn no_tmp_litter(dir: &Path) -> bool {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .all(|e| !e.file_name().to_string_lossy().contains(".tmp"))
    }

    #[test]
    fn installs_into_an_absent_file() {
        let dir = scratch_dir("absent");
        let path = dir.join("settings.json");
        let msg = merge_claude_hooks(&path, "/opt/roost/roost").expect("should install");
        assert!(msg.contains("installed"), "{msg}");
        let root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for (event, status) in HOOK_EVENTS {
            let cmds = hook_commands(&root, event);
            assert_eq!(cmds, vec![command_for("/opt/roost/roost", status)], "{event}");
        }
        assert!(no_tmp_litter(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn installs_into_an_empty_object() {
        let dir = scratch_dir("empty");
        let path = dir.join("settings.json");
        std::fs::write(&path, "{}").unwrap();
        let msg = merge_claude_hooks(&path, "/opt/roost/roost").expect("should install");
        assert!(msg.contains("installed"), "{msg}");
        let root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(hook_commands(&root, "Stop").len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unrelated_settings_and_key_order_survive() {
        let dir = scratch_dir("unrelated");
        let path = dir.join("settings.json");
        // zzz_last before aaa_first on purpose: proves order is preserved
        // (not alphabetized) through the round-trip.
        std::fs::write(
            &path,
            r#"{"zzz_last": 1, "model": "sonnet", "permissions": {"allow": ["Bash(ls:*)"]}, "aaa_first": 2, "hooks": {"PostToolUse": [{"hooks": [{"type": "command", "command": "echo unrelated"}]}]}}"#,
        )
        .unwrap();
        merge_claude_hooks(&path, "/opt/roost/roost").expect("should install");
        let out = std::fs::read_to_string(&path).unwrap();
        let root: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(root["model"], "sonnet");
        assert_eq!(root["permissions"]["allow"][0], "Bash(ls:*)");
        assert_eq!(root["hooks"]["PostToolUse"][0]["hooks"][0]["command"], "echo unrelated");
        assert!(
            out.find("zzz_last").unwrap() < out.find("aaa_first").unwrap(),
            "keys were reordered:\n{out}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_users_own_hooks_in_the_same_events_survive() {
        let dir = scratch_dir("user-hooks");
        let path = dir.join("settings.json");
        std::fs::write(
            &path,
            r#"{"hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "echo mine"}]}]}}"#,
        )
        .unwrap();
        merge_claude_hooks(&path, "/opt/roost/roost").expect("should install");
        let root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let groups = root["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(groups.len(), 2, "user's group must survive alongside roost's own: {groups:?}");
        assert_eq!(groups[0]["hooks"][0]["command"], "echo mine");
        assert_eq!(groups[0]["matcher"], "Bash");
        assert_eq!(groups[1]["hooks"][0]["command"], command_for("/opt/roost/roost", "working"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn running_twice_does_not_duplicate() {
        let dir = scratch_dir("idempotent");
        let path = dir.join("settings.json");
        merge_claude_hooks(&path, "/opt/roost/roost").expect("first run installs");
        let second = merge_claude_hooks(&path, "/opt/roost/roost");
        assert!(second.is_none(), "unchanged second run must be silent: {second:?}");
        let root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for (event, _) in HOOK_EVENTS {
            assert_eq!(hook_commands(&root, event).len(), 1, "{event} duplicated");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stale_roost_entry_is_replaced_not_appended() {
        let dir = scratch_dir("stale");
        let path = dir.join("settings.json");
        merge_claude_hooks(&path, "/old/path/roost").expect("first run installs");
        let msg = merge_claude_hooks(&path, "/new/path/roost").expect("moved binary must update");
        assert!(msg.contains("updated"), "{msg}");
        let root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for (event, status) in HOOK_EVENTS {
            let cmds = hook_commands(&root, event);
            assert_eq!(cmds, vec![command_for("/new/path/roost", status)], "{event}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_hand_copied_legacy_nc_hook_is_recognized_and_replaced() {
        // The pre-this-change docs told users to hand-copy an `nc`-piping
        // command referencing $ROOST_SOCK directly, for all three events.
        // That's exactly the "rot" this feature exists to fix — every one
        // must be upgraded in place, not left running (broken, on macOS)
        // alongside a second, working entry.
        let dir = scratch_dir("legacy-nc");
        let path = dir.join("settings.json");
        let legacy_group = |status: &str| {
            let command = format!(
                "[ -n \"$ROOST_PANE\" ] && printf '{{\"status\":\"{status}\"}}' | nc -U -q0 \"$ROOST_SOCK\" 2>/dev/null; true"
            );
            serde_json::json!({ "hooks": [{ "type": "command", "command": command }] })
        };
        let seed = serde_json::json!({ "hooks": {
            "PreToolUse": [legacy_group("working")],
            "Stop": [legacy_group("waiting")],
            "Notification": [legacy_group("needs_input")],
        }});
        std::fs::write(&path, seed.to_string()).unwrap();

        let msg =
            merge_claude_hooks(&path, "/opt/roost/roost").expect("legacy hooks must be updated");
        assert!(msg.contains("updated"), "{msg}");
        let root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for (event, status) in HOOK_EVENTS {
            let cmds = hook_commands(&root, event);
            assert_eq!(cmds, vec![command_for("/opt/roost/roost", status)], "{event}: {cmds:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_json_is_refused_not_overwritten() {
        let dir = scratch_dir("malformed");
        let path = dir.join("settings.json");
        let original = "{ this is not valid json";
        std::fs::write(&path, original).unwrap();
        let msg = merge_claude_hooks(&path, "/opt/roost/roost").expect("must explain the refusal");
        assert!(msg.contains("valid JSON"), "{msg}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original,
            "malformed file must be left untouched"
        );
        assert!(!path.with_extension("json.bak").exists());
        assert!(no_tmp_litter(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_read_error_is_reported_distinctly_from_a_parse_error() {
        let dir = scratch_dir("read-error");
        // A directory where a file is expected forces a read error that has
        // nothing to do with JSON content — must not be reported as one.
        let path = dir.join("settings.json");
        std::fs::create_dir_all(&path).unwrap();
        let msg = merge_claude_hooks(&path, "/opt/roost/roost").expect("must explain the refusal");
        assert!(
            !msg.contains("valid JSON"),
            "a read error must not claim to be a parse error: {msg}"
        );
        assert!(msg.contains("couldn't read"), "{msg}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_object_json_is_refused_with_its_own_message() {
        let dir = scratch_dir("non-object");
        let path = dir.join("settings.json");
        std::fs::write(&path, "[1, 2, 3]").unwrap();
        let msg = merge_claude_hooks(&path, "/opt/roost/roost").expect("must explain the refusal");
        assert!(msg.contains("not an object"), "{msg}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[1, 2, 3]");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_write_failure_is_reported_not_swallowed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch_dir("write-fail");
        let path = dir.join("settings.json");
        std::fs::write(&path, "{}").unwrap();
        // A read-only directory: write_atomic can create no tmp file here,
        // so the final fs::write step of the merge fails.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let msg = merge_claude_hooks(&path, "/opt/roost/roost");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap(); // restore for cleanup
        let msg = msg.expect("a write failure must still say something, not silently no-op");
        assert!(msg.contains("couldn't write"), "{msg}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn backs_up_the_pristine_file_once_before_the_first_write() {
        let dir = scratch_dir("backup");
        let path = dir.join("settings.json");
        let original = r#"{"model": "sonnet"}"#;
        std::fs::write(&path, original).unwrap();
        merge_claude_hooks(&path, "/old/roost").expect("install");
        let bak = path.with_extension("json.bak");
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), original);

        // A later change (stale entry) must not clobber that first snapshot
        // with an already-roost-modified version.
        merge_claude_hooks(&path, "/new/roost").expect("update");
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), original);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Blocking #1: `~/.claude/settings.json` is commonly a symlink into a
    /// dotfiles repo. Writing through it must update the *target* the link
    /// points at and leave the link itself alone — replacing the link with a
    /// plain file orphans the dotfiles copy silently (no error, no dirty
    /// `git status`, just stops applying from then on).
    #[test]
    fn write_atomic_follows_a_symlink_instead_of_replacing_it() {
        let dir = scratch_dir("symlink");
        let real_dir = dir.join("dotfiles-repo");
        std::fs::create_dir_all(&real_dir).unwrap();
        let real_target = real_dir.join("settings.json");
        std::fs::write(&real_target, "{}").unwrap();

        let link = dir.join("settings.json"); // stand-in for ~/.claude/settings.json
        std::os::unix::fs::symlink(&real_target, &link).unwrap();

        let msg = merge_claude_hooks(&link, "/opt/roost/roost").expect("should install");
        assert!(msg.contains("installed"), "{msg}");

        // The link itself must survive, unchanged, pointing at the same file.
        let meta = std::fs::symlink_metadata(&link).unwrap();
        assert!(meta.file_type().is_symlink(), "the symlink was replaced with a regular file");
        assert_eq!(std::fs::read_link(&link).unwrap(), real_target);

        // The dotfiles-managed file itself must carry the new content.
        let root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&real_target).unwrap()).unwrap();
        assert_eq!(hook_commands(&root, "Stop").len(), 1);

        assert!(no_tmp_litter(&real_dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Blocking #2: two roosts racing each other under the old `<path>.tmp`
    /// scheme could interleave writes into the same tmp file. Naming it from
    /// this process's own pid means a foreign writer's in-flight tmp file —
    /// simulated here — is never touched or collided with.
    #[test]
    fn write_atomic_tmp_path_is_pid_qualified_so_concurrent_writers_cannot_collide() {
        let dir = scratch_dir("concurrent-tmp");
        let target = dir.join("settings.json");
        std::fs::write(&target, "{}").unwrap();
        let foreign_pid = std::process::id() as u64 + 1;
        let foreign_tmp = dir.join(format!("settings.json.{foreign_pid}.tmp"));
        std::fs::write(&foreign_tmp, "mid-write-from-another-roost").unwrap();

        write_atomic(&target, "{\"hooks\":{}}").expect("write");

        assert_eq!(std::fs::read_to_string(&target).unwrap(), "{\"hooks\":{}}");
        // The foreign writer's own in-flight file must be untouched...
        assert_eq!(std::fs::read_to_string(&foreign_tmp).unwrap(), "mid-write-from-another-roost");
        // ...and nothing is left under the old, collision-prone `<path>.tmp` name.
        assert!(!dir.join("settings.json.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The `.bak` is a verbatim copy of a file that can hold `env` secrets or
    /// `apiKeyHelper`. Writing it at the umask default would publish exactly
    /// what a 0600 settings.json was locked down to protect.
    #[test]
    fn the_backup_inherits_the_original_mode_rather_than_the_umask() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch_dir("bak-mode");
        let path = dir.join("settings.json");
        std::fs::write(&path, "{\"env\":{\"ANTHROPIC_API_KEY\":\"sk-secret\"}}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let msg = merge_claude_hooks(&path, "/opt/roost/roost").expect("should install");
        assert!(msg.contains("installed"), "{msg}");

        let bak = path.with_extension("json.bak");
        assert!(bak.exists(), "no backup was written at {}", bak.display());
        let mode = std::fs::metadata(&bak).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the backup published a locked-down file at the umask default");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Blocking #3: settings.json can carry `env`/`apiKeyHelper` secrets, so
    /// an existing restrictive mode must survive the rewrite — a fresh tmp
    /// file otherwise picks up the process umask instead.
    #[test]
    fn write_atomic_preserves_the_original_file_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch_dir("mode");
        let target = dir.join("settings.json");
        std::fs::write(&target, "{}").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();

        write_atomic(&target, "{\"hooks\":{}}").expect("write");

        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "file mode was not preserved through the atomic write");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Fixture `$HOME` with `.pi/agent`, `.claude` and `.config/opencode`
    /// present — ready to receive an install — so a test run against it
    /// proves whichever gate is under test is what stopped the write, not
    /// "tool not present" (the other, unrelated reason the `ensure_*`
    /// functions no-op).
    fn ready_fixture_home(name: &str) -> PathBuf {
        let dir = scratch_dir(name);
        std::fs::create_dir_all(dir.join(".pi").join("agent")).unwrap();
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        std::fs::create_dir_all(dir.join(".config").join("opencode")).unwrap();
        dir
    }

    /// Run `f` with `$HOME` pointed at `home`, restoring the real value (or
    /// absence) before returning. Same bare set/restore shape as
    /// `core::app`'s `stale_session_falls_back_to_fresh_launch` fixture —
    /// no crate-wide lock serializes `$HOME` mutation against other tests in
    /// this binary there, and none is added here either; see that test's own
    /// note. The `ensure_*` functions read `$HOME` once
    /// per call, uncached, exactly like `host_io_disabled` reads its env var
    /// — so this is trusted regardless of how many times it runs.
    fn with_home<T>(home: &Path, f: impl FnOnce() -> T) -> T {
        let real_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home);
        let result = f();
        match real_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        result
    }

    /// The isolation gap this fix closes: `ROOST_STATE` + the old
    /// `ROOST_NO_EXT_INSTALL`-only gate let a run that followed the
    /// documented `ROOST_STATE=... ROOST_TEST_NO_HOST_IO=1 roost` isolation
    /// recipe silently write into the operator's real `~/.pi`/`~/.claude`.
    /// Both phases run in one test (rather than two) so this file's own
    /// `$HOME` mutation can't race itself; it can still race `core::app`'s
    /// or `clipboard`'s own env-mutating unit tests elsewhere in this same
    /// binary — a pre-existing, accepted risk, not one this test adds.
    #[test]
    fn host_io_gate_blocks_all_three_installs_even_when_all_three_tools_are_present() {
        let home = ready_fixture_home("host-io-gate");
        std::env::remove_var("ROOST_NO_EXT_INSTALL");

        // Phase 1: gate on, ROOST_NO_EXT_INSTALL deliberately left unset —
        // a pass here proves ROOST_TEST_NO_HOST_IO alone is sufficient.
        std::env::set_var("ROOST_TEST_NO_HOST_IO", "1");
        let (pi_gated, claude_gated, oc_gated) = with_home(&home, || {
            (ensure_pi_extension(), ensure_claude_hooks(), ensure_opencode_plugin())
        });
        assert!(pi_gated.is_none(), "pi extension installed despite the gate: {pi_gated:?}");
        assert!(
            claude_gated.is_none(),
            "Claude Code hooks installed despite the gate: {claude_gated:?}"
        );
        assert!(oc_gated.is_none(), "opencode plugin installed despite the gate: {oc_gated:?}");
        assert!(!home.join(".pi/agent/extensions/roost.ts").exists(), "pi extension file written");
        assert!(!home.join(".claude/settings.json").exists(), "settings.json written");
        // The gate must stop the opencode install before even the mkdir —
        // a stray (empty) plugin dir would be litter in a gated run.
        assert!(!home.join(".config/opencode/plugin").exists(), "opencode plugin dir created");

        // Phase 2 (positive control): identical fixture, gate off, must
        // actually install — proves phase 1 passed *because of* the gate,
        // not because the fixture was never "ready" (e.g. a typo'd path).
        std::env::remove_var("ROOST_TEST_NO_HOST_IO");
        let (pi_msg, claude_msg, oc_msg) = with_home(&home, || {
            (ensure_pi_extension(), ensure_claude_hooks(), ensure_opencode_plugin())
        });
        let pi_msg = pi_msg.expect("pi extension should install once the gate is off");
        assert!(pi_msg.contains("installed"), "{pi_msg}");
        let claude_msg = claude_msg.expect("claude hooks should install once the gate is off");
        assert!(claude_msg.contains("installed"), "{claude_msg}");
        let oc_msg = oc_msg.expect("opencode plugin should install once the gate is off");
        assert!(oc_msg.contains("installed"), "{oc_msg}");
        assert!(home.join(".pi/agent/extensions/roost.ts").exists());
        assert!(home.join(".claude/settings.json").exists());
        assert!(home.join(".config/opencode/plugin/opencode-plugin.ts").exists());

        let _ = std::fs::remove_dir_all(&home);
    }

    // ---- opencode plugin installer ----
    // (env-mutating tests below share the gate test's accepted-race note:
    // no crate-wide lock serializes env against this binary's other
    // env-touching tests.)

    #[test]
    fn opencode_installs_into_an_absent_plugin_dir() {
        // The fresh-machine shape: ~/.config/opencode exists but has no
        // plugin/ yet — the installer must create it (opencode auto-globs
        // the dir; nothing else can create it for us).
        let dir = scratch_dir("opencode-absent");
        let msg = install_opencode_plugin(&dir).expect("should install");
        assert!(msg.contains("installed"), "{msg}");
        assert!(msg.contains("opencode"), "{msg}");
        let target = dir.join("plugin").join("opencode-plugin.ts");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), BUNDLED_OPENCODE);
        assert!(no_tmp_litter(&dir.join("plugin")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn opencode_running_twice_does_not_duplicate() {
        let dir = scratch_dir("opencode-idempotent");
        let first = install_opencode_plugin(&dir).expect("first run installs");
        assert!(first.contains("installed"), "{first}");
        let second = install_opencode_plugin(&dir);
        assert!(second.is_none(), "unchanged second run must be silent: {second:?}");
        assert_eq!(
            std::fs::read_to_string(dir.join("plugin").join("opencode-plugin.ts")).unwrap(),
            BUNDLED_OPENCODE
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn opencode_a_stale_file_is_updated_not_left_stale() {
        // An older build's copy (or a hand-copied one) must be replaced in
        // place — the same rot the pi/claude paths exist to fix.
        let dir = scratch_dir("opencode-stale");
        let target = dir.join("plugin").join("opencode-plugin.ts");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, "// an older build's plugin").unwrap();
        let msg = install_opencode_plugin(&dir).expect("stale file must be updated");
        assert!(msg.contains("updated"), "{msg}");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), BUNDLED_OPENCODE);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn opencode_is_a_no_op_when_the_config_dir_is_absent() {
        // "Only when the tool is set up": no ~/.config/opencode means a
        // silent no-op — and critically, no ~/.config created either.
        let home = scratch_dir("opencode-no-tool");
        std::fs::create_dir_all(home.join(".pi").join("agent")).unwrap();
        std::env::remove_var("ROOST_NO_EXT_INSTALL");
        std::env::remove_var("ROOST_TEST_NO_HOST_IO");
        let msg = with_home(&home, ensure_opencode_plugin);
        assert!(msg.is_none(), "absent opencode must be a silent no-op: {msg:?}");
        assert!(!home.join(".config").exists(), "must never create ~/.config/opencode");
        let _ = std::fs::remove_dir_all(&home);
    }
}
