//! Client mode: `roost <verb> ...` connects to a running roost's control socket,
//! issues one request, prints the reply, and exits. This is the tmux-style
//! actuation surface an LLM (or a human, or a script) drives — the same
//! `control::Request` the socket executes on the main loop.
//!
//! Targeting is daemonless: `$ROOST_SOCK` (set in every pane, so an in-pane
//! agent needs no config) else the default per-state-dir socket path.
//! Credential precedence: `$ROOST_CONTROL_TOKEN`, else `$ROOST_TOKEN` (an
//! in-pane agent's own pane token — authenticates it as its pane, scoped to
//! its spawned subtree and audited as that pane), else `<state>/control.token`
//! (the fleet token, for an external operator with no pane env).
//!
//! `__status` is a separate, undocumented internal verb (not in `VERBS`):
//! the Claude Code hooks `infra::extension::ensure_claude_hooks` installs
//! call back into this same binary instead of piping through `nc`/`socat` —
//! see `run_status_hook` below.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use crate::infra::sock::{socket_path, OVERSIZE_LINE_MSG};
use crate::infra::store::FsStore;

const VERBS: &[&str] = &["list", "status", "spawn", "fork", "send", "read", "close", "wait"];

/// If the first CLI arg is a control verb, run as a client and return the exit
/// code. Otherwise return None so `main` launches the TUI. Only a genuinely
/// empty argument list returns None, though — an unrecognized argument is a
/// hard error (below), not a fallthrough: a script or an LLM probing the
/// binary needs an answer it can act on, not a seized terminal.
pub fn maybe_run() -> Option<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let verb = args.first()?; // no args → launch the TUI
    if verb == "__status" {
        return Some(run_status_hook(&args[1..]));
    }
    if verb == "--help" || verb == "-h" {
        println!("{USAGE}"); // an explicit request, not an error: stdout, exit 0
        return Some(0);
    }
    if verb == "--version" || verb == "-V" {
        println!("roost {}", env!("CARGO_PKG_VERSION"));
        return Some(0);
    }
    // F11: `keys` is deliberately **not** in `VERBS`. Every verb there is a
    // control-socket request against a *running* roost; this one reads
    // config.json off disk and answers without one — you ask it precisely
    // when roost isn't up, or when you want to know what your dotfile did
    // before launching. Routing it through the socket would make the one
    // question you can ask about a config require the program the config
    // configures to already be running.
    if verb == "keys" {
        if args[1..].iter().any(|a| a == "--help" || a == "-h") {
            println!("{KEYS_HELP}");
            return Some(0);
        }
        if let Some(bad) = args[1..].first() {
            eprintln!("roost keys: unexpected argument: {bad}\n\n{KEYS_HELP}");
            return Some(2);
        }
        return Some(run_keys());
    }
    if !VERBS.contains(&verb.as_str()) {
        // Same convention as a bad flag inside a known verb: hard error,
        // instead of falling through to the TUI (which used to seize the
        // terminal on a typo, or panic off one).
        eprintln!("roost: unrecognized argument: {verb}\n\n{USAGE}");
        return Some(2);
    }
    // A verb's own `--help`/`-h`, honored before the verb ever runs for real
    // — but only ahead of a literal `--` (send/spawn's end-of-options
    // marker), so `send PANE -- --help` still sends that text as data
    // instead of showing help.
    let rest = &args[1..];
    let end = rest.iter().position(|a| a == "--").unwrap_or(rest.len());
    if rest[..end].iter().any(|a| a == "--help" || a == "-h") {
        println!("{}", verb_help(verb));
        return Some(0);
    }
    Some(run(&args))
}

/// `roost __status <working|waiting|needs_input>` — what the auto-installed
/// Claude Code hooks run. Reads the pane's own `ROOST_PANE`/`ROOST_TOKEN`/
/// `ROOST_SOCK` (set on every pane roost spawns, inherited by every
/// descendant process) and reports status the same way the pi extension
/// does, over the same socket, in the same one-way wire format `infra::sock`
/// already parses — no `nc`/`socat` involved, so it works identically on
/// macOS (whose system `/usr/bin/nc` lacks netcat-openbsd's `-q`) and Linux.
///
/// Not a public verb (kept out of `VERBS`/`USAGE`/`--help`): it's plumbing a
/// hook config invokes, not something a human is meant to type. Matches the
/// old hand-copied hooks' contract exactly: a no-op, not just non-fatal, the
/// instant `$ROOST_PANE` is unset (i.e. outside roost), and never writes to
/// stdout/stderr or returns nonzero — a broken or unreachable socket must
/// never surface as hook noise or an error in the user's Claude Code session.
fn run_status_hook(args: &[String]) -> i32 {
    let pane = std::env::var("ROOST_PANE").ok();
    // stdin is consulted only once the no-op guard has already passed:
    // outside roost ($ROOST_PANE unset) these hooks fire for every Claude
    // Code session on the machine, and the contract there is "exits 0
    // instantly without touching anything" — a stdin read that could block
    // on a pipe nobody closes would break exactly that.
    let message = match pane.as_deref() {
        Some(p) if !p.is_empty() => hook_stdin_message(args.first().map(String::as_str)),
        _ => None,
    };
    status_hook(
        pane,
        std::env::var("ROOST_TOKEN").unwrap_or_default(),
        std::env::var_os("ROOST_SOCK").map(Into::into),
        args.first().map(String::as_str),
        message,
    )
}

/// The `message` field of the hook-input JSON Claude Code writes to a hook's
/// stdin — for the Notification hook that's the human-readable reason
/// ("Claude needs your permission to use Bash", "Claude is waiting for your
/// input"), which roost surfaces with the ◆ exactly like a pi ask-tool's
/// question. Only consulted for `needs_input` (the one status whose hook
/// carries a reason worth showing); anything else skips the read entirely.
/// Never blocks on a human: a TTY stdin (someone typing the verb by hand) is
/// skipped, and a real hook's pipe is read to EOF (Claude writes the JSON
/// and closes) with a cap well past any legitimate hook input. Any read or
/// parse failure is just "no message" — same never-fail-loudly contract as
/// the rest of the hook path.
fn hook_stdin_message(status: Option<&str>) -> Option<String> {
    use std::io::{IsTerminal, Read};
    if status != Some("needs_input") || std::io::stdin().is_terminal() {
        return None;
    }
    let mut buf = String::new();
    std::io::stdin().take(64 * 1024).read_to_string(&mut buf).ok()?;
    let v: serde_json::Value = serde_json::from_str(&buf).ok()?;
    // Cap here, not just app-side: an oversized message would push the whole
    // status line past the socket's per-line limit and cost the report
    // itself, not just the text.
    v.get("message")?.as_str().map(|m| m.chars().take(512).collect())
}

/// The pure-ish core behind `run_status_hook`, taking its inputs as
/// parameters instead of reading them from the environment — so tests can
/// drive it against a real (but test-owned) socket without touching process
/// env at all.
fn status_hook(
    pane: Option<String>,
    token: String,
    sock: Option<std::path::PathBuf>,
    status: Option<&str>,
    message: Option<String>,
) -> i32 {
    let Some(pane) = pane.filter(|p| !p.is_empty()) else { return 0 };
    let Some(status) = status else { return 0 };
    let sock = sock.unwrap_or_else(socket_path);
    let mut msg = serde_json::json!({
        "pane": pane,
        "token": token,
        "event": "status",
        "status": status,
    });
    if let Some(m) = message {
        msg["message"] = serde_json::Value::String(m);
    }
    let _ = write_line(&sock, &msg); // best-effort: a hook must never fail loudly
    0
}

const USAGE: &str = "\
roost — control a running instance:
  roost list
  roost status [PANE]
  roost spawn ADAPTER [--cwd DIR] [--input TEXT]
  roost fork [PANE]
  roost send PANE TEXT... [--enter]
  roost send --all TEXT... [--enter]
  roost read PANE [--tail N | --full]
  roost close PANE [--force]
  roost wait PANE... [--until STATUS] [--timeout SEC]
  roost VERB --help              (a verb's own usage)
and locally, with no running instance:
  roost keys                     (the effective keymap, config.json applied)
  roost --help | -h
  roost --version | -V
(run `roost` with no args to launch the multiplexer)
`--` ends options for any verb — later words are taken verbatim, dashes and
all (e.g. `roost send 5 -- --not-a-flag`).
Exit codes: 0 ok / 1 runtime error / 2 usage error / 3 `wait` timed out.";

/// `roost VERB --help` / `-h` — this verb's own usage, checked (in
/// `maybe_run`) before the verb is ever allowed to run for real, so a help
/// request can't fall through into executing it (the `list --help` bug this
/// closes: it used to return live pane JSON, exit 0). `verb` is always one
/// of `VERBS` here — every caller checks membership first.
fn verb_help(verb: &str) -> &'static str {
    match verb {
        "list" => "roost list\nList every pane: id, adapter, cwd, status, …",
        "status" => "roost status [PANE]\nOne pane's status, or every pane's if PANE is omitted.",
        "spawn" => "roost spawn ADAPTER [--cwd DIR] [--input TEXT]\nLaunch a new pane running ADAPTER.",
        "fork" => "roost fork [PANE]\nA sibling pane in the same context; the focused pane's if PANE is omitted.",
        "send" => "roost send PANE TEXT... [--enter]\nroost send --all TEXT... [--enter]\nType TEXT into PANE (or, with --all, every reachable pane). --enter also submits it.",
        "read" => "roost read PANE [--tail N | --full]\nA pane's current screen (default), its last N lines, or its full scrollback.",
        "close" => "roost close PANE [--force]\nClose a pane; --force kills its process instead of asking it to exit.",
        "wait" => "roost wait PANE... [--until STATUS] [--timeout SEC]\nBlock until a pane reaches STATUS (default: waiting) or the timeout elapses.",
        _ => unreachable!("verb_help called with {verb:?}, not a member of VERBS"),
    }
}

/// `wait`'s timeout reply (`{"timed_out":true}`, see app.rs's `poll_waiters`)
/// is the one outcome a caller MUST branch on — printing it with the same
/// exit 0 as a resolved wait let `wait … && read` proceed as though the pane
/// had actually finished. A dedicated code rather than reusing 1: a caller
/// that only checks `$?` for "nonzero" still needs to tell a timeout apart
/// from every other kind of failure. Documented in `USAGE`.
const EXIT_WAIT_TIMEOUT: i32 = 3;

fn run(args: &[String]) -> i32 {
    let verb = args[0].as_str();
    let req = match build_request(args, resolve_token()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("roost: {e}\n\n{USAGE}");
            return 2;
        }
    };
    let sock = std::env::var_os("ROOST_SOCK").map(Into::into).unwrap_or_else(socket_path);
    match send_request(&sock, &req) {
        Ok(reply) => {
            if let Some(ok) = reply.get("ok") {
                println!("{}", serde_json::to_string_pretty(ok).unwrap_or_default());
                // The JSON is unchanged either way — only the exit status
                // tells a shell caller `wait` actually timed out rather than
                // resolved normally.
                let timed_out =
                    verb == "wait" && ok.get("timed_out") == Some(&serde_json::Value::Bool(true));
                if timed_out { EXIT_WAIT_TIMEOUT } else { 0 }
            } else {
                let err = reply.get("err").and_then(|e| e.as_str()).unwrap_or("unknown error");
                eprintln!("roost: {err}");
                // An oversized request is the caller's own invalid input —
                // the usage-error family (2, see USAGE), not a runtime
                // failure of the instance (1) every other `err` reply here
                // gets.
                if err == OVERSIZE_LINE_MSG { 2 } else { 1 }
            }
        }
        Err(e) => {
            eprintln!("roost: cannot reach a running roost ({e}). Is one open in this workspace?");
            1
        }
    }
}

/// The control credential: explicit env, else the pane's own token (an
/// in-pane agent authenticates as its pane — scoped to its spawned subtree
/// and audited as that pane), else the fleet token file (an external
/// operator, which has no `$ROOST_TOKEN` in its env).
fn resolve_token() -> String {
    if let Ok(t) = std::env::var("ROOST_CONTROL_TOKEN") {
        return t;
    }
    if let Ok(t) = std::env::var("ROOST_TOKEN") {
        return t;
    }
    let path = FsStore::default_path().with_file_name("control.token");
    std::fs::read_to_string(path).map(|t| t.trim().to_string()).unwrap_or_default()
}

fn build_request(args: &[String], token: String) -> Result<serde_json::Value, String> {
    let verb = args[0].as_str();
    let rest = &args[1..];
    let mut m = serde_json::Map::new();
    m.insert("token".into(), token.into());
    m.insert("method".into(), verb.into());
    match verb {
        "list" => {
            reject_unknown_flags(verb, rest, &[])?;
        }
        "status" => {
            reject_unknown_flags(verb, rest, &[])?;
            if let Some(p) = positional(rest).first() {
                m.insert("pane".into(), parse_pane(p)?.into());
            }
        }
        "spawn" => {
            reject_unknown_flags(verb, rest, &["--cwd", "--input"])?;
            let pos = positional(rest);
            let adapter = pos.first().ok_or("spawn needs an ADAPTER")?;
            m.insert("adapter".into(), adapter.as_str().into());
            if let Some(cwd) = flag_value(rest, "--cwd")? {
                m.insert("cwd".into(), cwd.into());
            }
            if let Some(input) = flag_value(rest, "--input")? {
                m.insert("initial_input".into(), input.into());
            }
        }
        "fork" => {
            reject_unknown_flags(verb, rest, &[])?;
            if let Some(p) = positional(rest).first() {
                m.insert("pane".into(), parse_pane(p)?.into());
            }
        }
        "send" => {
            reject_unknown_flags(verb, rest, &["--all", "--enter"])?;
            let pos = positional(rest);
            if has_flag(rest, "--all") {
                // --all replaces the PANE positional; a leading token that
                // still parses as one looks like a leftover `send PANE
                // TEXT...` (old muscle memory) rather than deliberate
                // broadcast text — reject instead of guessing, since this
                // fans out to every running pane the actor may target.
                if pos.first().is_some_and(|p| parse_pane(p).is_ok()) {
                    return Err("send --all takes TEXT only, no PANE (got both)".into());
                }
                m.insert("method".into(), "broadcast".into());
                m.insert("text".into(), pos.join(" ").into());
                m.insert("submit".into(), has_flag(rest, "--enter").into());
            } else {
                let pane = pos.first().ok_or("send needs a PANE")?;
                m.insert("pane".into(), parse_pane(pane)?.into());
                let text = pos[1..].join(" ");
                m.insert("text".into(), text.into());
                m.insert("submit".into(), has_flag(rest, "--enter").into());
            }
        }
        "read" => {
            reject_unknown_flags(verb, rest, &["--tail", "--full"])?;
            let pos = positional(rest);
            let pane = pos.first().ok_or("read needs a PANE")?;
            m.insert("pane".into(), parse_pane(pane)?.into());
            let mode = if let Some(n) = flag_value(rest, "--tail")? {
                let n: usize = n.parse().map_err(|_| "--tail needs a number")?;
                serde_json::json!({ "tail": n })
            } else if has_flag(rest, "--full") {
                serde_json::json!("full")
            } else {
                serde_json::json!("screen")
            };
            m.insert("mode".into(), mode);
        }
        "close" => {
            reject_unknown_flags(verb, rest, &["--force"])?;
            let pos = positional(rest);
            let pane = pos.first().ok_or("close needs a PANE")?;
            m.insert("pane".into(), parse_pane(pane)?.into());
            m.insert("force".into(), has_flag(rest, "--force").into());
        }
        "wait" => {
            reject_unknown_flags(verb, rest, &["--until", "--timeout"])?;
            let pos = positional(rest);
            if pos.is_empty() {
                return Err("wait needs at least one PANE".into());
            }
            let panes: Result<Vec<serde_json::Value>, String> =
                pos.iter().map(|p| parse_pane(p).map(Into::into)).collect();
            m.insert("panes".into(), serde_json::Value::Array(panes?));
            m.insert("until".into(), flag_value(rest, "--until")?.unwrap_or_else(|| "waiting".into()).into());
            if let Some(secs) = flag_value(rest, "--timeout")? {
                let secs: u64 = secs.parse().map_err(|_| "--timeout needs a number (seconds)")?;
                // Code review (exit UX audit 2026-08-07): `secs * 1000`
                // panics in a debug build and wraps in the shipped release
                // profile for a large-enough `--timeout` (e.g. u64::MAX).
                // Saturate instead — an absurd timeout should behave like
                // "wait (almost) forever", not undefined-by-profile.
                m.insert("timeout_ms".into(), secs.saturating_mul(1000).into());
            }
        }
        _ => return Err(format!("unknown verb: {verb}")),
    }
    Ok(serde_json::Value::Object(m))
}

fn parse_pane(s: &str) -> Result<u64, String> {
    s.parse().map_err(|_| format!("not a pane id: {s}"))
}

/// Index of the first literal `--` in `args`, or `args.len()` if there is
/// none — the end-of-options boundary every flag-parsing helper below
/// shares. Nothing at or past it is ever read as a flag, so text that
/// itself starts with `-`/`--` (e.g. `send 5 -- "--- plan ---"`) can only be
/// mistaken for an option before this point.
fn end_of_options(args: &[String]) -> usize {
    args.iter().position(|a| a == "--").unwrap_or(args.len())
}

/// Every `--flag` in `args` ahead of `--` must be in `allowed`, or this is a
/// usage error — a typo (`--entr` for `--enter`) or, worse, an arbitrary
/// `--`-prefixed piece of prompt text used to be silently swallowed (along
/// with whatever it introduced) while the reply still said ok. Mirrors the
/// wire side's own invalid-`--until` error: name what was wrong, then
/// enumerate what's actually valid for this verb.
fn reject_unknown_flags(verb: &str, args: &[String], allowed: &[&str]) -> Result<(), String> {
    for a in &args[..end_of_options(args)] {
        if a.starts_with("--") && !allowed.contains(&a.as_str()) {
            let opts = if allowed.is_empty() { "(none)".to_string() } else { allowed.join("|") };
            return Err(format!("{verb}: unknown flag {a} (valid: {opts})"));
        }
    }
    Ok(())
}

/// Args that aren't flags or flag values. A literal `--` ends option parsing
/// (`end_of_options`): everything after it is positional verbatim, dashes
/// and all — the escape hatch for text that would otherwise look like a
/// flag, since every `--`-prefixed token *before* that point must already
/// be one `reject_unknown_flags` has approved.
fn positional(args: &[String]) -> Vec<String> {
    let end = end_of_options(args);
    let mut out = Vec::new();
    let mut i = 0;
    while i < end {
        let a = &args[i];
        if a.starts_with("--") {
            // --cwd/--input/--tail take a value; skip it.
            if matches!(a.as_str(), "--cwd" | "--input" | "--tail" | "--until" | "--timeout") {
                i += 2;
            } else {
                i += 1;
            }
        } else {
            out.push(a.clone());
            i += 1;
        }
    }
    if end < args.len() {
        out.extend(args[end + 1..].iter().cloned()); // literal, past the `--`
    }
    out
}

/// The value following `flag` — only if `flag` appears before `--`, so a
/// flag *name* typed as literal data past the end-of-options marker (e.g.
/// `send 5 -- --cwd`) is never mistaken for the real flag.
///
/// F6 (exit UX audit 2026-08-07): `Err` when `flag` is present but nothing
/// valid follows it — the value slot is missing (`roost read 5 --tail`, the
/// flag as the last token) or belongs to the far side of `--`
/// (`roost read 5 --tail --`, where reading past `end` used to return the
/// `--` marker itself as the "value"). Previously this silently degraded to
/// `None`, indistinguishable from the flag never having been typed at all —
/// so `read 5 --tail` returned the *screen*, `wait 5 --timeout` silently
/// took the 5-minute default, `spawn pi --cwd` silently used the inherited
/// cwd, all replying ok. `--tail $N` with an unset shell variable is exactly
/// how a script hits this. Same silent-wrong-answer class `reject_unknown_flags`
/// already closed for unknown flags; a known flag with a missing value gets
/// the same treatment.
fn flag_value(args: &[String], flag: &str) -> Result<Option<String>, String> {
    let end = end_of_options(args);
    let Some(i) = args[..end].iter().position(|a| a == flag) else { return Ok(None) };
    if i + 1 < end {
        Ok(Some(args[i + 1].clone()))
    } else {
        Err(format!("{flag} needs a value"))
    }
}

/// Same `--`-boundary rule as `flag_value`, for a boolean flag.
fn has_flag(args: &[String], flag: &str) -> bool {
    args[..end_of_options(args)].iter().any(|a| a == flag)
}

/// Connect to `sock` and write `msg` as one newline-terminated JSON line —
/// the wire framing every message on this socket uses, control requests and
/// one-way status/session reports alike (see `infra::sock`). Returns the
/// connected stream so a caller that wants a reply (control verbs, via
/// `send_request`) can keep reading it; a fire-and-forget caller (the
/// `__status` hook, via `status_hook`) just drops it.
fn write_line(sock: &Path, msg: &serde_json::Value) -> std::io::Result<UnixStream> {
    let mut stream = UnixStream::connect(sock)?;
    let mut line = serde_json::to_string(msg).unwrap_or_default();
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    stream.flush()?;
    Ok(stream)
}

fn send_request(sock: &Path, req: &serde_json::Value) -> std::io::Result<serde_json::Value> {
    let stream = write_line(sock, req)?;
    let mut reader = BufReader::new(stream);
    let mut resp = String::new();
    reader.read_line(&mut resp)?;
    serde_json::from_str(resp.trim())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

const KEYS_HELP: &str = "\
roost keys
Print the keymap this machine actually runs: roost's defaults with
config.json's overrides applied, the same merge the TUI dispatches on.

Needs no running roost — it reads config.json directly, which is the state
you want to check *before* launching.

Output is `chord<TAB>action`, one per line, with a third column naming
config.json for anything it changed. The action names are config.json's own,
so a line can be pasted back into a \"keys\" block.

Exit codes: 0 ok / 2 config.json had entries roost had to skip (named on
stderr; the printed table is what roost would really run).";

/// F11: print the effective keymap and say what config.json changed.
///
/// The review that asked for this (`docs/engagements/2026-08-19-…`) named the
/// gap precisely: config.json's diagnostics surfaced *only* as a startup
/// toast inside the TUI, so a mistyped chord in a dotfile was discovered by
/// launching roost and catching a transient message. zellij has
/// `setup --check`, lazygit has `--config`; roost had no way to ask.
///
/// Cheap to build only because C34 exists: `effective_bindings` is the
/// default-plus-overrides merge, already written, already the same one
/// `translate_with` dispatches on. This function is a printer.
fn run_keys() -> i32 {
    use crate::ui::input::{action_name, effective_bindings, Keymap};

    let (keymap, diagnostics) = crate::infra::config::load_keymap();
    let defaults = effective_bindings(&Keymap::default());
    let live = effective_bindings(&keymap);

    let mut rows: Vec<(String, String, &str)> = Vec::new();
    for (chord, action) in &live {
        // Changed by config.json if this exact (chord, action) pair isn't
        // one of the defaults — covers both a remap onto a free chord and a
        // rebinding of a chord that already did something else.
        let from_config = !defaults.iter().any(|(c, a)| c == chord && a == action);
        let note = if from_config { "config.json" } else { "" };
        rows.push((chord.clone(), action_name(action), note));
    }
    // Disabled chords are the other half of the answer, and the half a table
    // of live bindings structurally cannot show: "why doesn't Alt+f work"
    // is exactly the question this command gets asked.
    for (chord, _) in &defaults {
        if !live.iter().any(|(c, _)| c == chord) {
            rows.push((chord.clone(), "disabled".to_string(), "config.json"));
        }
    }
    rows.sort();

    // Tab-separated and *not* column-padded. Padding before a tab looks
    // tidier in a terminal and quietly breaks the thing this format is for:
    // `cut -f1` would hand back `"Alt+q     "`. A caller who wants columns
    // has `column -t`; a caller who wants fields cannot un-pad them.
    for (chord, action, note) in &rows {
        if note.is_empty() {
            println!("{chord}\t{action}");
        } else {
            println!("{chord}\t{action}\t{note}");
        }
    }

    if diagnostics.is_empty() {
        return 0;
    }
    // Diagnostics to stderr so the table above still pipes cleanly, and a
    // non-zero exit so a dotfile test can gate on it — the whole point of
    // being able to ask this outside the TUI.
    for d in &diagnostics {
        eprintln!("roost keys: {d}");
    }
    2
}

#[cfg(test)]
mod tests {
    use super::build_request;
    use crate::core::control::{Method, Request};

    fn parse(args: &[&str]) -> Request {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let v = build_request(&owned, "T".into()).expect("build");
        // The request the socket would receive must deserialize into control::Request.
        serde_json::from_value(v).expect("deserialize")
    }

    #[test]
    fn cli_args_build_valid_requests() {
        assert!(matches!(parse(&["list"]).method, Method::List));
        match parse(&["spawn", "pi", "--cwd", "/x", "--input", "hi there"]).method {
            Method::Spawn { adapter, cwd, initial_input } => {
                assert_eq!(adapter, "pi");
                assert_eq!(cwd.as_deref(), Some("/x"));
                assert_eq!(initial_input.as_deref(), Some("hi there"));
            }
            _ => panic!(),
        }
        match parse(&["send", "3", "run", "the", "tests", "--enter"]).method {
            Method::Send { pane, text, submit } => {
                assert_eq!(pane, 3);
                assert_eq!(text, "run the tests");
                assert!(submit);
            }
            _ => panic!(),
        }
        match parse(&["read", "5", "--tail", "20"]).method {
            Method::Read { pane, mode } => {
                assert_eq!(pane, 5);
                assert_eq!(mode, crate::core::control::ReadMode::Tail(20));
            }
            _ => panic!(),
        }
        match parse(&["close", "4", "--force"]).method {
            Method::Close { pane, force } => {
                assert_eq!(pane, 4);
                assert!(force);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn cli_send_all_broadcasts_and_rejects_a_leftover_pane_id() {
        match parse(&["send", "--all", "hi", "there", "--enter"]).method {
            Method::Broadcast { text, submit } => {
                assert_eq!(text, "hi there");
                assert!(submit);
            }
            _ => panic!("expected a broadcast"),
        }
        // --all replaces the PANE positional; a leading token that still
        // parses as a pane id looks like `send PANE TEXT` with --all left
        // in by mistake — usage error, not a guess.
        let owned: Vec<String> = ["send", "--all", "3", "x"].iter().map(|s| s.to_string()).collect();
        assert!(build_request(&owned, "T".into()).is_err());
    }

    /// QA repro: `send 5 hi --entr` used to silently drop the typo'd flag
    /// and send without Enter anyway, replying ok. Now a usage error naming
    /// the flag this verb actually accepts.
    #[test]
    fn cli_rejects_an_unknown_flag_instead_of_dropping_it() {
        let owned: Vec<String> =
            ["send", "5", "hi", "--entr"].iter().map(|s| s.to_string()).collect();
        let err = build_request(&owned, "T".into()).unwrap_err();
        assert!(err.contains("--entr"), "must name what was wrong: {err}");
        assert!(err.contains("--enter"), "must name the valid flag: {err}");
    }

    /// QA repro: `send 5 "--- plan ---"` used to send an empty string and
    /// still reply ok — the worst failure mode for LLM-authored prompt text.
    /// `--` is the fix: an explicit end-of-options marker after which
    /// everything is literal, however many leading dashes it has.
    #[test]
    fn cli_dash_dash_ends_options_so_dashed_text_sends_verbatim() {
        match parse(&["send", "5", "--", "--- plan ---"]).method {
            Method::Send { pane, text, submit } => {
                assert_eq!(pane, 5);
                assert_eq!(text, "--- plan ---");
                assert!(!submit, "-- text must never be misread as --enter");
            }
            _ => panic!(),
        }
        // The same marker works for spawn's ADAPTER and wait's PANE list —
        // it's a property of `positional`, not a `send`-only special case.
        match parse(&["wait", "--", "5"]).method {
            Method::Wait { panes, .. } => assert_eq!(panes, vec![5]),
            _ => panic!(),
        }
    }

    /// F6 (exit UX audit 2026-08-07) repros: a known flag with no value
    /// after it used to be indistinguishable from the flag never being
    /// typed, so each of these silently fell back to a default and replied
    /// ok. `--tail $N` with an unset shell variable is exactly how a script
    /// hits this — the flag is there, its value just evaporated.
    #[test]
    fn cli_rejects_a_known_flag_missing_its_value() {
        let read = ["read", "5", "--tail"].iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let err = build_request(&read, "T".into()).unwrap_err();
        assert!(err.contains("--tail"), "must name the flag: {err}");

        let wait = ["wait", "5", "--timeout"].iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let err = build_request(&wait, "T".into()).unwrap_err();
        assert!(err.contains("--timeout"), "must name the flag: {err}");

        let spawn = ["spawn", "pi", "--cwd"].iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let err = build_request(&spawn, "T".into()).unwrap_err();
        assert!(err.contains("--cwd"), "must name the flag: {err}");
    }

    /// The value must come from strictly before `--`, not leak the `--`
    /// marker itself in as a "value" when the flag is its immediate
    /// predecessor.
    #[test]
    fn cli_flag_missing_its_value_is_still_an_error_right_before_the_end_of_options_marker() {
        let owned: Vec<String> =
            ["read", "5", "--tail", "--", "ignored"].iter().map(|s| s.to_string()).collect();
        let err = build_request(&owned, "T".into()).unwrap_err();
        assert!(err.contains("--tail"), "must name the flag: {err}");
    }

    /// Code review (exit UX audit 2026-08-07): `secs * 1000` panicked in a
    /// debug build (and wrapped in release) for a `--timeout` this large.
    #[test]
    fn cli_wait_timeout_saturates_instead_of_overflowing() {
        match parse(&["wait", "5", "--timeout", "18446744073709551615"]).method {
            Method::Wait { timeout_ms, .. } => assert_eq!(timeout_ms, Some(u64::MAX)),
            _ => panic!(),
        }
    }

    /// Every verb needs its own, real help text — not the generic usage
    /// (byte-identical to a bare invocation was the QA-caught bug) and not
    /// two verbs sharing text that would make the wrong one look right.
    #[test]
    fn cli_verb_help_is_distinct_and_leads_with_its_own_usage() {
        use std::collections::HashSet;
        let texts: HashSet<&str> = super::VERBS.iter().map(|v| super::verb_help(v)).collect();
        assert_eq!(texts.len(), super::VERBS.len(), "two verbs must not share identical help text");
        for v in super::VERBS {
            assert!(
                super::verb_help(v).starts_with(&format!("roost {v}")),
                "{v}'s help must lead with its own usage"
            );
        }
    }
}

#[cfg(test)]
mod status_hook_tests {
    use super::status_hook;
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;

    fn scratch_sock(name: &str) -> (PathBuf, UnixListener) {
        let dir = std::env::temp_dir().join(format!("roost-status-hook-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.sock");
        let listener = UnixListener::bind(&path).unwrap();
        (path, listener)
    }

    #[test]
    fn no_pane_or_no_status_is_a_true_noop() {
        // "True" no-op, not just a swallowed connection error: if the guard
        // didn't short-circuit before ever touching the socket, a listener
        // sitting right there would still see the connection land.
        let (sock, listener) = scratch_sock("noop");
        listener.set_nonblocking(true).unwrap();

        assert_eq!(status_hook(None, String::new(), Some(sock.clone()), Some("working"), None), 0);
        assert_eq!(
            status_hook(Some(String::new()), String::new(), Some(sock.clone()), Some("working"), None),
            0
        );
        assert_eq!(status_hook(Some("3".into()), "t".into(), Some(sock.clone()), None, None), 0);

        assert!(
            listener.accept().is_err(),
            "status_hook connected to the socket despite no pane/status"
        );
        let _ = std::fs::remove_dir_all(sock.parent().unwrap());
    }

    #[test]
    fn writes_exactly_the_status_line_the_socket_expects() {
        let (sock, listener) = scratch_sock("wire");

        let code = status_hook(
            Some("7".into()),
            "tok".into(),
            Some(sock.clone()),
            Some("needs_input"),
            Some("Claude needs your permission to use Bash".into()),
        );
        assert_eq!(code, 0);

        let (stream, _) = listener.accept().expect("status_hook never connected");
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read the status line");
        let v: serde_json::Value = serde_json::from_str(line.trim()).expect("valid JSON");
        assert_eq!(v["pane"], "7");
        assert_eq!(v["token"], "tok");
        assert_eq!(v["event"], "status");
        assert_eq!(v["status"], "needs_input");
        assert_eq!(v["message"], "Claude needs your permission to use Bash");

        let _ = std::fs::remove_dir_all(sock.parent().unwrap());
    }

    /// The hook-input reason rides only when present — a plain report stays
    /// byte-identical to the old wire shape (no `message` key at all).
    #[test]
    fn no_message_means_no_message_key_on_the_wire() {
        let (sock, listener) = scratch_sock("bare");
        assert_eq!(status_hook(Some("7".into()), "tok".into(), Some(sock.clone()), Some("working"), None), 0);
        let (stream, _) = listener.accept().expect("status_hook never connected");
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read the status line");
        let v: serde_json::Value = serde_json::from_str(line.trim()).expect("valid JSON");
        assert!(v.get("message").is_none());
        let _ = std::fs::remove_dir_all(sock.parent().unwrap());
    }
}
