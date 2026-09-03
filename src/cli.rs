//! Client mode: `roost <verb> ...` connects to a running roost's control socket,
//! issues one request, prints the reply, and exits. This is the tmux-style
//! actuation surface an LLM (or a human, or a script) drives — the same
//! `control::Request` the socket executes on the main loop.
//!
//! Targeting is daemonless. A control verb resolves its target instance in
//! this order: an explicit `-w <name>` / `--workspace <name>` flag (the
//! pre-pass below strips it from argv wherever it appears), then
//! `$ROOST_SOCK` (set in every pane, so an in-pane agent needs no config),
//! then the workspace named by `$ROOST_WORKSPACE`, then the default
//! per-state-dir socket path. Credential precedence: `$ROOST_CONTROL_TOKEN`,
//! else `$ROOST_TOKEN` (an in-pane agent's own pane token — authenticates it
//! as its pane, scoped to its spawned subtree and audited as that pane),
//! else `<workspace>/control.token` (the fleet token, for an external
//! operator with no pane env).
//!
//! `__status` is a separate, undocumented internal verb (not in `VERBS`):
//! the Claude Code hooks `infra::extension::ensure_claude_hooks` installs
//! call back into this same binary instead of piping through `nc`/`socat` —
//! see `run_status_hook` below.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::core::workspace::Workspace;
use crate::infra::sock::{socket_path, OVERSIZE_LINE_MSG};
use crate::infra::store::{check_creatable_name, check_workspace_name, FsStore, DEFAULT_WORKSPACE};

const VERBS: &[&str] =
    &["list", "status", "spawn", "fork", "send", "read", "close", "focus", "wait"];

/// If the first CLI arg is a control verb, run as a client and return the exit
/// code. Otherwise return None so `main` launches the TUI. Only a genuinely
/// empty argument list returns None, though — an unrecognized argument is a
/// hard error (below), not a fallthrough: a script or an LLM probing the
/// binary needs an answer it can act on, not a seized terminal.
pub fn maybe_run() -> Option<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // The workspace pre-pass runs before verb dispatch, so `-w` works
    // before or after any verb — and before this function decides there is
    // no verb at all (a bare `roost -w x` must launch the TUI *in x*).
    // Invalid names and missing values exit 2 here, before any state is
    // touched; the resolution itself is idempotent, so main's TUI-path
    // re-init below cannot override it.
    let (workspace_flag, args) = match strip_workspace_flag(&args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("roost: {e}\n\n{USAGE}");
            return Some(2);
        }
    };
    let workspace_env =
        std::env::var_os("ROOST_WORKSPACE").map(|v| v.to_string_lossy().into_owned());
    if let Err(e) = FsStore::init_workspace(workspace_flag.as_deref(), workspace_env.as_deref()) {
        eprintln!("roost: {e}\n\n{USAGE}");
        return Some(2);
    }
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
    // The `ws` verb family is the other local surface (beside `keys`): it
    // answers from the state root's files — workspace directories, their lock
    // sentinels and `workspace.json` — and never opens a socket, which is the
    // whole point: it exists to tell you what is on disk *before* you aim a
    // control verb anywhere.
    if verb == "ws" {
        if args[1..].iter().any(|a| a == "--help" || a == "-h") {
            println!("{}", verb_help("ws"));
            return Some(0);
        }
        return Some(run_ws(&args[1..]));
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
    Some(run(&args, workspace_flag.as_deref()))
}

/// The workspace pre-pass (D2): pull `-w <name>`, `--workspace <name>` and
/// `--workspace=<name>` out of argv wherever they appear — before or after
/// the verb — and return the name plus the remaining argv, so the verb
/// parser below never sees the flag. Stripping stops at the first literal
/// `--` (the end-of-options convention every verb shares), so flag-shaped
/// text sent as data (`roost send 5 -- -w x`) survives verbatim. Pure: no
/// env, no dispatch — errors here are the caller's usage-error exit 2.
fn strip_workspace_flag(args: &[String]) -> Result<(Option<String>, Vec<String>), String> {
    let mut flag: Option<String> = None;
    let mut rest = Vec::with_capacity(args.len());
    let mut end_of_options = false;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if !end_of_options {
            if a == "-w" || a == "--workspace" {
                if flag.is_some() {
                    return Err("workspace selected more than once".into());
                }
                let value = args
                    .get(i + 1)
                    .ok_or_else(|| format!("{a} needs a workspace name (roost -w <name>)"))?;
                flag = Some(value.clone());
                i += 2;
                continue;
            }
            if let Some(value) = a.strip_prefix("--workspace=") {
                if flag.is_some() {
                    return Err("workspace selected more than once".into());
                }
                flag = Some(value.to_string());
                i += 1;
                continue;
            }
            if a == "--" {
                end_of_options = true;
            }
        }
        rest.push(a.clone());
        i += 1;
    }
    Ok((flag, rest))
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
    // Best-effort: a hook must never fail loudly — and never hang either.
    // This runs inside the agent's own process on every status transition,
    // so a roost that stopped reading its socket must not stall the agent.
    let _ = write_line(&sock, &msg, HOOK_WRITE_TIMEOUT);
    0
}

/// A status hook runs inside the agent's own process, on its own critical
/// path, and its whole contract is "cheap and silent". A roost that has
/// stopped draining its socket must cost it a couple of seconds at most,
/// never a stall.
const HOOK_WRITE_TIMEOUT: Duration = Duration::from_secs(2);

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
  roost focus PANE
  roost wait PANE... [--until STATUS] [--timeout SEC]
  roost VERB --help              (a verb's own usage)
and locally, with no running instance:
  roost ws ls [--json]           (every workspace: running/idle, tabs, panes, adapters, saved)
  roost ws rm NAME               (delete an idle workspace's directory)
  roost ws mv OLD NEW            (rename an idle workspace)
  roost keys                     (the effective keymap, config.json applied)
  roost --help | -h
  roost --version | -V
(run `roost` with no args to launch the multiplexer)
`-w NAME` / `--workspace NAME` targets another workspace, before or after
the verb (names: 1-32 of a-z, 0-9, '.', '_', '-'; `default` is the one roost
opens with no flag and no $ROOST_WORKSPACE).
`--` ends options for any verb — later words are taken verbatim, dashes and
all (e.g. `roost send 5 -- --not-a-flag`).
Exit codes: 0 ok / 1 runtime error / 2 usage error / 3 `wait` timed out.";

/// `roost VERB --help` / `-h` — this verb's own usage, checked (in
/// `maybe_run`) before the verb is ever allowed to run for real, so a help
/// request can't fall through into executing it (the `list --help` bug this
/// closes: it used to return live pane JSON, exit 0). `verb` is always one
/// of `VERBS` here — every caller checks membership first — plus the local
/// `ws` family, whose own dispatch hands `"ws"` straight through.
fn verb_help(verb: &str) -> &'static str {
    match verb {
        "list" => "roost list\nList every pane: id, adapter, cwd, status, …",
        "status" => "roost status [PANE]\nOne pane's status, or every pane's if PANE is omitted.",
        "spawn" => "roost spawn ADAPTER [--cwd DIR] [--input TEXT]\nLaunch a new pane running ADAPTER.",
        "fork" => "roost fork [PANE]\nA sibling pane in the same context; the focused pane's if PANE is omitted.",
        "send" => "roost send PANE TEXT... [--enter]\nroost send --all TEXT... [--enter]\nType TEXT into PANE (or, with --all, every reachable pane). --enter also submits it.",
        "read" => "roost read PANE [--tail N | --full]\nA pane's current screen (default), its last N lines, or its full scrollback.",
        "close" => "roost close PANE [--force]\nClose a pane; --force kills its process instead of asking it to exit.",
        "focus" => "roost focus PANE\nFocus a pane: switch to its tab and land focus on it (a no-op if it already is).",
        "wait" => "roost wait PANE... [--until STATUS] [--timeout SEC]\nBlock until a pane reaches STATUS (default: waiting) or the timeout elapses.",
        "ws" => "roost ws [ls [--json] | rm NAME | mv OLD NEW]\n\
                 List every workspace (name, running/idle, tabs, panes, adapters, last saved;\n\
                 --json as a JSON array), delete an idle workspace's directory (rm), or rename\n\
                 one (mv). Read from the state root's files: no running roost is contacted,\n\
                 and `default` can be listed but never renamed or deleted.",
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

fn run(args: &[String], workspace_flag: Option<&str>) -> i32 {
    let verb = args[0].as_str();
    let req = match build_request(args, resolve_token()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("roost: {e}\n\n{USAGE}");
            return 2;
        }
    };
    // Target precedence (control-targeting spec): an explicit `-w` flag
    // beats even `ROOST_SOCK` — the flag is the one way to reach *another*
    // workspace from inside a pane, and `socket_path` already names the
    // selected workspace's socket. Without a flag, `ROOST_SOCK` keeps
    // winning exactly where it does today (in-pane), and `socket_path`
    // falls back to the workspace `ROOST_WORKSPACE` named, else default.
    let sock = match workspace_flag {
        Some(_) => socket_path(),
        None => std::env::var_os("ROOST_SOCK").map(Into::into).unwrap_or_else(socket_path),
    };
    let deadline = reply_timeout(verb, &req);
    match send_request(&sock, &req, deadline) {
        Ok(reply) => {
            if let Some(ok) = reply.get("ok") {
                println!("{}", serde_json::to_string_pretty(ok).unwrap_or_default());
                // The JSON is unchanged either way — only the exit status
                // tells a shell caller `wait` actually timed out rather than
                // resolved normally.
                let timed_out =
                    verb == "wait" && ok.get("timed_out") == Some(&serde_json::Value::Bool(true));
                if timed_out {
                    EXIT_WAIT_TIMEOUT
                } else {
                    0
                }
            } else {
                let err = reply.get("err").and_then(|e| e.as_str()).unwrap_or("unknown error");
                eprintln!("roost: {err}");
                // An oversized request is the caller's own invalid input —
                // the usage-error family (2, see USAGE), not a runtime
                // failure of the instance (1) every other `err` reply here
                // gets.
                if err == OVERSIZE_LINE_MSG {
                    2
                } else {
                    1
                }
            }
        }
        // A timeout is not "cannot reach": the connection was made and the
        // request went out. Saying so is the difference between a script
        // author looking for a running roost and looking at a wedged one.
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) =>
        {
            eprintln!(
                "roost: connected, but no reply within {}s — the running roost is wedged or \
                 saturated.",
                deadline.as_secs()
            );
            1
        }
        Err(e) => {
            // With several workspaces possible, "cannot reach" alone leaves
            // the caller guessing which roost they meant. Name the workspace
            // that was tried, probe the instance locks for ones actually
            // running, and hand over the flag that targets them.
            let running = FsStore::running_workspaces();
            eprintln!("{}", unreachable_message(FsStore::workspace_name(), &running, &e));
            1
        }
    }
}

/// The control client's unreachable error (D9): the workspace tried, the
/// workspaces with a live instance (probed via their locks), and the `-w`
/// suggestion — or the plain "nothing is running anywhere" when the probe
/// comes back empty. `tried` and `running` are injected so the wording is
/// unit-testable without touching process env.
fn unreachable_message(tried: &str, running: &[String], e: &std::io::Error) -> String {
    let place = if tried == DEFAULT_WORKSPACE {
        "the default workspace".to_string()
    } else {
        format!("workspace '{tried}'")
    };
    let mut msg = format!("roost: cannot reach a running roost in {place} ({e}).");
    if running.is_empty() {
        msg.push_str("\nNo roost is running in any workspace.");
    } else {
        // Suggest the first one literally: the whole point is a copy-pasteable
        // next command, not a description.
        msg.push_str(&format!(
            "\nRunning in: {} — target one with `roost -w {}` (`roost ws ls` lists workspaces).",
            running.join(", "),
            running[0]
        ));
    }
    msg
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
        "focus" => {
            reject_unknown_flags(verb, rest, &[])?;
            let pos = positional(rest);
            let pane = pos.first().ok_or("focus needs a PANE")?;
            m.insert("pane".into(), parse_pane(pane)?.into());
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
            m.insert(
                "until".into(),
                flag_value(rest, "--until")?.unwrap_or_else(|| "waiting".into()).into(),
            );
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
fn write_line(
    sock: &Path,
    msg: &serde_json::Value,
    timeout: Duration,
) -> std::io::Result<UnixStream> {
    let mut stream = UnixStream::connect(sock)?;
    // A server that accepts and then never reads would otherwise block this
    // write forever, the same way a server that never replies used to block
    // the read below.
    stream.set_write_timeout(Some(timeout))?;
    let mut line = serde_json::to_string(msg).unwrap_or_default();
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    stream.flush()?;
    Ok(stream)
}

/// How long the client waits for a reply to any verb but `wait`.
///
/// roost answers a control request on its event loop, which turns over at
/// ~30 Hz, so a normal reply is a frame away. This is the server's own
/// `READ_TIMEOUT` — the longest the far end is ever willing to wait on
/// *us* — reused as the longest we wait on it.
const REPLY_TIMEOUT: Duration = Duration::from_secs(30);

/// Slack on top of `wait`'s own deadline, so the client's timer can never
/// fire before the reply it is waiting for is even due.
const WAIT_REPLY_SLACK: Duration = Duration::from_secs(10);

/// How long the client should wait for a reply to `req`.
///
/// `wait` is the one verb that legitimately blocks — it parks until its
/// panes reach a status or its own deadline fires (`App::register_waiter`,
/// whose default and ceiling are `control::WAIT_*_TIMEOUT_MS`). Every other
/// verb is answered in a frame.
fn reply_timeout(verb: &str, req: &serde_json::Value) -> Duration {
    if verb != "wait" {
        return REPLY_TIMEOUT;
    }
    let ms = req
        .get("timeout_ms")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(crate::core::control::WAIT_DEFAULT_TIMEOUT_MS)
        .min(crate::core::control::WAIT_MAX_TIMEOUT_MS);
    Duration::from_millis(ms).saturating_add(WAIT_REPLY_SLACK)
}

/// Send `req` and read the one-line reply, giving up after `timeout`.
///
/// The deadline is the point. `infra::sock` is hardened at every step — a 2 s
/// pre-auth read, a 30 s read, a 5 s write — and the client had none at all,
/// so a roost whose event loop was wedged left `roost list` blocked forever:
/// no output, no exit status, nothing for a script to recover from. The
/// timeouts are per-syscall rather than a wall-clock budget, which is exact
/// enough here: the request is one small line and the reply is one line.
fn send_request(
    sock: &Path,
    req: &serde_json::Value,
    timeout: Duration,
) -> std::io::Result<serde_json::Value> {
    let stream = write_line(sock, req, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
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
    // of live bindings structurally cannot show: "why doesn't Alt+Shift+z
    // work" is exactly the question this command gets asked.
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

    // Where the file is — the question this command was getting asked and
    // could not answer. stderr, so the table above still pipes cleanly.
    //
    // Not under `$ROOST_STATE`: setting it *is* naming the directory, so
    // there is nothing to discover, and "a clean config says nothing on
    // stderr" is a contract worth keeping for the scripted case (tests/cli.rs
    // pins it). The line is for the user who never chose a directory and has
    // no way to know which of two roost would read.
    let resolved = crate::infra::config::resolve_config();
    if std::env::var_os("ROOST_STATE").is_none() {
        if resolved.exists {
            eprintln!("roost keys: reading {}", resolved.path.display());
        } else {
            eprintln!(
                "roost keys: no config.json — create {} to change these bindings",
                resolved.path.display()
            );
        }
    }

    if diagnostics.is_empty() {
        return 0;
    }
    // Diagnostics to stderr so the table above still pipes cleanly. Both
    // channels print — a notice is worth reading — but only a **problem**
    // sets the exit code. README's contract is precise about this: a
    // non-zero exit means "an entry roost had to skip", so a dotfile test
    // can gate on it. A config that displaced a default did nothing wrong
    // and roost did exactly what it asked; failing the gate for that would
    // break every dotfile that remaps a bound chord, which is the ordinary
    // case the escape hatch exists for.
    for d in diagnostics.all() {
        eprintln!("roost keys: {d}");
    }
    if diagnostics.problems.is_empty() {
        0
    } else {
        2
    }
}

const WS_HELP: &str = "\
roost ws [ls [--json] | rm NAME | mv OLD NEW]
Workspaces, answered from the state root's files with nothing running.

  roost ws            same as `ws ls`: one line per workspace (default plus
                      every directory under <root>/workspaces), tab-separated:
                      name, running or idle, tabs=, panes=, adapters=, saved=
  roost ws ls --json  the same fields as a JSON array on stdout
  roost ws rm NAME    delete an idle workspace's directory and everything in
                      it (claims under <root>/claims/ belong to sessions, not
                      workspaces, and are never touched)
  roost ws mv OLD NEW rename an idle workspace's directory

`running`/`idle` comes from the workspace's instance lock, probed without
holding it; tabs, panes and adapters come from reading the workspace's
workspace.json read-only, and `saved` is that file's last-modified time
(`-` or null when there is none). No socket is ever opened.

Exit codes: 0 ok / 1 refused (running, unknown, reserved `default`) / 2
usage error (bad name, wrong arguments).";

/// A `ws` sub-command, parsed off argv before anything touches the
/// filesystem — the pure half of the dispatch, so the argument grammar is
/// unit-testable and the execution half below stays a straight line.
#[derive(Debug, PartialEq)]
enum WsCmd {
    List { json: bool },
    Rm(String),
    Mv(String, String),
}

fn parse_ws_args(args: &[String]) -> Result<WsCmd, String> {
    let Some(sub) = args.first() else {
        // Bare `roost ws` is the listing — the one thing you want from it
        // most of the time, and the cheapest thing to type.
        return Ok(WsCmd::List { json: false });
    };
    let rest = &args[1..];
    match sub.as_str() {
        "ls" => match rest {
            [] => Ok(WsCmd::List { json: false }),
            [f] if f.as_str() == "--json" => Ok(WsCmd::List { json: true }),
            _ => Err("ws ls takes at most --json and nothing else".into()),
        },
        "rm" => match rest {
            [name] => Ok(WsCmd::Rm(name.clone())),
            _ => Err("ws rm takes exactly one NAME (roost ws rm <name>)".into()),
        },
        "mv" => match rest {
            [old, new] => Ok(WsCmd::Mv(old.clone(), new.clone())),
            _ => Err("ws mv takes exactly OLD and NEW (roost ws mv <old> <new>)".into()),
        },
        other => Err(format!("unknown ws verb: {other} (ls, rm, mv)")),
    }
}

/// The `ws` verb family: list, remove and rename workspaces straight off the
/// state root. Like `keys`, it is a *local* verb — nothing here connects to
/// a control socket or spawns a client — because its whole job is to answer
/// "what exists and what is running" before any control verb is aimed
/// anywhere. All state lives in one directory per workspace, so every
/// operation below is a plain directory operation plus the same lock probe
/// the unreachable-error path already uses.
fn run_ws(args: &[String]) -> i32 {
    match parse_ws_args(args) {
        Ok(WsCmd::List { json }) => ws_list(json),
        Ok(WsCmd::Rm(name)) => ws_rm(&name),
        Ok(WsCmd::Mv(old, new)) => ws_mv(&old, &new),
        Err(e) => {
            eprintln!("roost ws: {e}\n\n{WS_HELP}");
            2
        }
    }
}

/// `ws ls` / bare `ws`: enumerate every workspace and print one row each.
/// Files only — the running flag is the lock probe (D6), never a socket
/// connect. The list is never actually empty (`default` is always listed),
/// but the spec pins exit 0 for the empty case, so it is handled, not
/// assumed.
fn ws_list(json: bool) -> i32 {
    let rows: Vec<WsRow> = FsStore::workspace_names().iter().map(|n| ws_row(n)).collect();
    if json {
        let arr: Vec<serde_json::Value> = rows.iter().map(ws_row_json).collect();
        println!("{}", serde_json::to_string_pretty(&arr).unwrap_or_else(|_| "[]".into()));
        return 0;
    }
    if rows.is_empty() {
        println!("no workspaces");
        return 0;
    }
    for row in &rows {
        println!("{}", ws_row_text(row));
    }
    0
}

/// One workspace's row for `ws ls`: everything the listing shows, gathered
/// without holding any lock and without opening any socket. `last_saved` is
/// the workspace.json mtime as RFC3339 UTC — the timestamp the *save* path
/// last touched, which is the honest answer to "when did I last work here".
struct WsRow {
    name: String,
    running: bool,
    tabs: usize,
    panes: usize,
    adapters: Vec<String>,
    last_saved: Option<String>,
}

fn ws_row(name: &str) -> WsRow {
    let dir = FsStore::workspace_dir(name);
    let running = FsStore::instance_running(&dir);
    let path = dir.join("workspace.json");
    // Missing or unparseable state shows zeros, never an error: a workspace
    // that has never been saved (or was hand-mangled) is still a workspace
    // someone may be running, and the listing must not die on it.
    let (tabs, panes, adapters) =
        std::fs::read_to_string(&path).map(|raw| ws_counts(&raw)).unwrap_or((0, 0, Vec::new()));
    let last_saved = std::fs::metadata(&path).and_then(|m| m.modified()).ok().map(rfc3339);
    WsRow { name: name.to_string(), running, tabs, panes, adapters, last_saved }
}

/// Tab count, pane count and the distinct adapters in use, from the text of
/// a workspace.json read read-only. This deliberately parses *past* a future
/// `version` — the SCHEMA_VERSION guard exists to protect write-back, and
/// nothing here ever writes — and returns zeros for anything it cannot
/// parse, so a corrupt or newer file can never crash the listing.
fn ws_counts(raw: &str) -> (usize, usize, Vec<String>) {
    let Ok(ws) = serde_json::from_str::<Workspace>(raw) else { return (0, 0, Vec::new()) };
    let panes: usize = ws.tabs.iter().map(|t| t.panes.len()).sum();
    let adapters: BTreeSet<String> =
        ws.tabs.iter().flat_map(|t| t.panes.values()).map(|p| p.adapter.clone()).collect();
    (ws.tabs.len(), panes, adapters.into_iter().collect())
}

/// The text row: tab-separated so `cut`/`awk` keep working (the same
/// convention `roost keys` prints under — a caller who wants columns has
/// `column -t`), with self-describing `key=value` fields and `-` for "none",
/// so an empty adapters list or a never-saved workspace is unambiguous at a
/// glance and never an empty field a parser silently drops.
fn ws_row_text(row: &WsRow) -> String {
    let state = if row.running { "running" } else { "idle" };
    let adapters = if row.adapters.is_empty() { "-".to_string() } else { row.adapters.join(",") };
    let saved = row.last_saved.clone().unwrap_or_else(|| "-".into());
    format!(
        "{}\t{}\ttabs={}\tpanes={}\tadapters={}\tsaved={}",
        row.name, state, row.tabs, row.panes, adapters, saved
    )
}

/// The JSON row for `ws ls --json`: the same fields, `last_saved` null when
/// there is no workspace.json. Stdout carries nothing but the array.
fn ws_row_json(row: &WsRow) -> serde_json::Value {
    serde_json::json!({
        "name": row.name,
        "running": row.running,
        "tabs": row.tabs,
        "panes": row.panes,
        "adapters": row.adapters,
        "last_saved": row.last_saved,
    })
}

/// `ws rm <name>`: delete one workspace's directory, nothing else. The
/// guard order is the safety story: a reserved or invalid name is refused
/// before any path is computed, a missing or running workspace before
/// anything is removed — and what is removed is exactly
/// `<root>/workspaces/<name>`, so the root and the sessions' claims beside
/// it (D7) are unreachable by construction.
fn ws_rm(name: &str) -> i32 {
    if let Err(e) = check_workspace_name(name) {
        eprintln!("roost ws rm: {e}\n\n{WS_HELP}");
        return 2;
    }
    // The reserved half of the creatable check is the only way here (the
    // grammar half already exited above): `default` cannot be deleted.
    if let Err(e) = check_creatable_name(name) {
        eprintln!("roost ws rm: {e}");
        return 1;
    }
    let dir = FsStore::workspace_dir(name);
    if !dir.is_dir() {
        eprintln!("roost ws rm: no workspace named '{name}' (`roost ws ls` lists workspaces)");
        return 1;
    }
    if FsStore::instance_running(&dir) {
        eprintln!(
            "roost ws rm: workspace '{name}' is running — close it before removing \
             (`roost ws ls` lists workspaces)"
        );
        return 1;
    }
    if let Err(e) = std::fs::remove_dir_all(&dir) {
        eprintln!("roost ws rm: could not remove '{name}' ({e})");
        return 1;
    }
    0
}

/// `ws mv <old> <new>`: rename one workspace's directory. Same guard order
/// as `ws rm`, plus the new name's own validation — a bad NEW is a usage
/// error (2, like every other name error), while refusals about OLD
/// (`default`, unknown, running) and a taken destination are runtime
/// refusals (1). The rename is one `fs::rename` inside `workspaces/`, same
/// filesystem, atomic where the platform allows it to be.
fn ws_mv(old: &str, new: &str) -> i32 {
    if let Err(e) = check_creatable_name(new) {
        eprintln!("roost ws mv: {e}\n\n{WS_HELP}");
        return 2;
    }
    if old == DEFAULT_WORKSPACE {
        // check_creatable_name's own message, verbatim: `default` is
        // reserved for the default workspace, which is lived in, not moved.
        eprintln!("roost ws mv: {}", check_creatable_name(old).unwrap_err());
        return 1;
    }
    let from = FsStore::workspace_dir(old);
    if !from.is_dir() {
        eprintln!("roost ws mv: no workspace named '{old}' (`roost ws ls` lists workspaces)");
        return 1;
    }
    if FsStore::instance_running(&from) {
        eprintln!(
            "roost ws mv: workspace '{old}' is running — close it before renaming \
             (`roost ws ls` lists workspaces)"
        );
        return 1;
    }
    let to = FsStore::workspace_dir(new);
    if to.exists() {
        eprintln!("roost ws mv: workspace '{new}' already exists (`roost ws ls` lists workspaces)");
        return 1;
    }
    if let Err(e) = std::fs::rename(&from, &to) {
        eprintln!("roost ws mv: could not rename '{old}' to '{new}' ({e})");
        return 1;
    }
    0
}

/// RFC3339 UTC (`YYYY-MM-DDTHH:MM:SSZ`), seconds precision, for a file
/// mtime. Hand-rolled because the one wall-clock string in the CLI does not
/// justify a date-time dependency: the conversion is the standard
/// days-from-epoch algorithm (Hinnant), exact across the range `SystemTime`
/// can name, and pinned to known timestamps in the tests below.
fn rfc3339(t: SystemTime) -> String {
    let secs = match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    };
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let sod = secs.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", sod / 3600, (sod % 3600) / 60, sod % 60)
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian —
/// Howard Hinnant's `civil_from_days`. `div_euclid`/`rem_euclid` keep the
/// era arithmetic correct for pre-epoch (negative) day counts too.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::build_request;
    use super::{strip_workspace_flag, unreachable_message, VERBS};
    use crate::core::control::{Method, Request};

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    /// The pre-pass must find the workspace flag in every position for
    /// every verb — before the verb, after it, in either `-w` spelling —
    /// and hand back argv with the verb still first.
    #[test]
    fn cli_workspace_flag_strips_from_every_position_of_every_verb() {
        for verb in VERBS {
            let (flag, rest) = strip_workspace_flag(&argv(&["-w", "x", verb])).unwrap();
            assert_eq!(flag.as_deref(), Some("x"), "-w before {verb}");
            assert_eq!(rest, vec![verb.to_string()]);

            let (flag, rest) = strip_workspace_flag(&argv(&[verb, "-w", "x"])).unwrap();
            assert_eq!(flag.as_deref(), Some("x"), "-w after {verb}");
            assert_eq!(rest, vec![verb.to_string()]);

            let (flag, rest) = strip_workspace_flag(&argv(&[verb, "--workspace", "x"])).unwrap();
            assert_eq!(flag.as_deref(), Some("x"), "--workspace after {verb}");
            assert_eq!(rest, vec![verb.to_string()]);

            let (flag, rest) = strip_workspace_flag(&argv(&[verb, "--workspace=x"])).unwrap();
            assert_eq!(flag.as_deref(), Some("x"), "--workspace= after {verb}");
            assert_eq!(rest, vec![verb.to_string()]);
        }
    }

    /// A flag among the verb's own arguments must not disturb them, and a
    /// literal `-w` past the verb's `--` is sent data, not a selection.
    #[test]
    fn cli_workspace_flag_leaves_verb_arguments_and_dash_dash_data_alone() {
        let (flag, rest) =
            strip_workspace_flag(&argv(&["send", "5", "hi", "-w", "x", "--enter"])).unwrap();
        assert_eq!(flag.as_deref(), Some("x"));
        assert_eq!(rest, vec!["send".to_string(), "5".into(), "hi".into(), "--enter".into()]);

        let (flag, rest) = strip_workspace_flag(&argv(&["send", "5", "--", "-w", "x"])).unwrap();
        assert_eq!(flag, None, "past `--`, -w is text, not a selection");
        assert_eq!(rest, argv(&["send", "5", "--", "-w", "x"]));
    }

    /// The flag with no value (`-w` last, `$WORKSPACE` unset in a script)
    /// is a usage error, not a silent fallthrough to the default workspace.
    #[test]
    fn cli_workspace_flag_missing_its_value_is_an_error() {
        for args in [argv(&["-w"]), argv(&["list", "--workspace"])] {
            let err = strip_workspace_flag(&args).unwrap_err();
            assert!(err.contains("needs a workspace name"), "{err}");
        }
    }

    #[test]
    fn cli_workspace_flag_given_twice_is_an_error() {
        let err = strip_workspace_flag(&argv(&["-w", "a", "list", "--workspace=b"])).unwrap_err();
        assert!(err.contains("more than once"), "{err}");
    }

    /// D9: the unreachable error must name the tried workspace, list the
    /// running ones, and hand over a copy-pasteable `-w` command — or say
    /// plainly that nothing runs anywhere.
    #[test]
    fn cli_unreachable_error_names_the_target_and_suggests_the_running_one() {
        let e = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "no socket");
        let msg = unreachable_message("default", &["a".to_string(), "b".to_string()], &e);
        assert!(msg.contains("the default workspace"), "{msg}");
        assert!(msg.contains("a, b"), "must list every running workspace: {msg}");
        assert!(msg.contains("-w a"), "must suggest a copy-pasteable command: {msg}");

        let msg = unreachable_message("x", &["a".to_string()], &e);
        assert!(msg.contains("workspace 'x'"), "{msg}");
        assert!(msg.contains("-w a"), "{msg}");

        let msg = unreachable_message("x", &[], &e);
        assert!(msg.contains("roost is running in any workspace"), "{msg}");
    }

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
        match parse(&["focus", "5"]).method {
            Method::Focus { pane } => assert_eq!(pane, 5),
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
        let owned: Vec<String> =
            ["send", "--all", "3", "x"].iter().map(|s| s.to_string()).collect();
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
mod reply_timeout_tests {
    use super::{reply_timeout, send_request, REPLY_TIMEOUT, WAIT_REPLY_SLACK};
    use crate::core::control::{WAIT_DEFAULT_TIMEOUT_MS, WAIT_MAX_TIMEOUT_MS};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    /// Every verb but `wait` is answered in a frame, so they share one
    /// deadline; `wait` gets its own, whatever the caller asked for — a
    /// client that gave up first would report a wedged roost that was doing
    /// exactly what it was told.
    #[test]
    fn wait_gets_its_own_deadline_and_every_other_verb_the_default() {
        let plain = serde_json::json!({"method": "list"});
        assert_eq!(reply_timeout("list", &plain), REPLY_TIMEOUT);
        assert_eq!(reply_timeout("read", &plain), REPLY_TIMEOUT);

        // No --timeout: the server's own default, plus slack.
        let bare_wait = serde_json::json!({"method": "wait"});
        assert_eq!(
            reply_timeout("wait", &bare_wait),
            Duration::from_millis(WAIT_DEFAULT_TIMEOUT_MS) + WAIT_REPLY_SLACK
        );

        // An explicit one is honoured...
        let long = serde_json::json!({"method": "wait", "timeout_ms": 900_000});
        assert_eq!(reply_timeout("wait", &long), Duration::from_millis(900_000) + WAIT_REPLY_SLACK);

        // ...and clamped exactly where the server clamps it, so `--timeout`
        // of u64::MAX (which `cli_wait_timeout_saturates_instead_of_
        // overflowing` pins as reachable) cannot overflow the Duration.
        let absurd = serde_json::json!({"method": "wait", "timeout_ms": u64::MAX});
        assert_eq!(
            reply_timeout("wait", &absurd),
            Duration::from_millis(WAIT_MAX_TIMEOUT_MS) + WAIT_REPLY_SLACK
        );
    }

    /// A roost that accepts the connection and then never answers must cost
    /// the client its deadline, not the rest of the day. Before this, a
    /// wedged event loop left `roost list` blocked forever: no output, no
    /// exit status, nothing a script could recover from.
    #[test]
    fn a_server_that_never_replies_costs_the_deadline_and_no_more() {
        let dir = std::env::temp_dir().join(format!("roost-cli-timeout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path: PathBuf = dir.join("t.sock");
        let listener = UnixListener::bind(&path).unwrap();
        // Accept, read the request, and then say nothing at all — the
        // wedged-event-loop shape. Holding the stream is what keeps the
        // client's read from seeing EOF and returning early.
        let held = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("the client connects");
            // Long enough to outlast the client's 300 ms deadline several
            // times over, short enough not to pad the suite.
            std::thread::sleep(Duration::from_secs(2));
            drop(stream);
        });

        let req = serde_json::json!({"token": "t", "method": "list"});
        let started = Instant::now();
        let err = send_request(&path, &req, Duration::from_millis(300))
            .expect_err("a server that never replies must not resolve");
        let took = started.elapsed();

        assert!(
            matches!(err.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut),
            "the failure must read as a timeout, not something else: {err:?} ({:?})",
            err.kind()
        );
        assert!(took < Duration::from_secs(3), "gave up after {took:?}, deadline was 300ms");

        let _ = held.join();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod status_hook_tests {
    use super::status_hook;
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;

    fn scratch_sock(name: &str) -> (PathBuf, UnixListener) {
        let dir =
            std::env::temp_dir().join(format!("roost-status-hook-{name}-{}", std::process::id()));
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
            status_hook(
                Some(String::new()),
                String::new(),
                Some(sock.clone()),
                Some("working"),
                None
            ),
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
        assert_eq!(
            status_hook(Some("7".into()), "tok".into(), Some(sock.clone()), Some("working"), None),
            0
        );
        let (stream, _) = listener.accept().expect("status_hook never connected");
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read the status line");
        let v: serde_json::Value = serde_json::from_str(line.trim()).expect("valid JSON");
        assert!(v.get("message").is_none());
        let _ = std::fs::remove_dir_all(sock.parent().unwrap());
    }
}
