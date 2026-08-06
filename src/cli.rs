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

use crate::infra::sock::socket_path;
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
    if !VERBS.contains(&verb.as_str()) {
        // Same convention as a bad flag inside a known verb: hard error,
        // instead of falling through to the TUI (which used to seize the
        // terminal on a typo, or panic off one).
        eprintln!("roost: unrecognized argument: {verb}\n\n{USAGE}");
        return Some(2);
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
    status_hook(
        std::env::var("ROOST_PANE").ok(),
        std::env::var("ROOST_TOKEN").unwrap_or_default(),
        std::env::var_os("ROOST_SOCK").map(Into::into),
        args.first().map(String::as_str),
    )
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
) -> i32 {
    let Some(pane) = pane.filter(|p| !p.is_empty()) else { return 0 };
    let Some(status) = status else { return 0 };
    let sock = sock.unwrap_or_else(socket_path);
    let msg = serde_json::json!({
        "pane": pane,
        "token": token,
        "event": "status",
        "status": status,
    });
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
  roost --help | -h
  roost --version | -V
(run `roost` with no args to launch the multiplexer)";

fn run(args: &[String]) -> i32 {
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
                0
            } else {
                let err = reply.get("err").and_then(|e| e.as_str()).unwrap_or("unknown error");
                eprintln!("roost: {err}");
                1
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
        "list" => {}
        "status" => {
            if let Some(p) = positional(rest).first() {
                m.insert("pane".into(), parse_pane(p)?.into());
            }
        }
        "spawn" => {
            let pos = positional(rest);
            let adapter = pos.first().ok_or("spawn needs an ADAPTER")?;
            m.insert("adapter".into(), adapter.as_str().into());
            if let Some(cwd) = flag_value(rest, "--cwd") {
                m.insert("cwd".into(), cwd.into());
            }
            if let Some(input) = flag_value(rest, "--input") {
                m.insert("initial_input".into(), input.into());
            }
        }
        "fork" => {
            if let Some(p) = positional(rest).first() {
                m.insert("pane".into(), parse_pane(p)?.into());
            }
        }
        "send" => {
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
            let pos = positional(rest);
            let pane = pos.first().ok_or("read needs a PANE")?;
            m.insert("pane".into(), parse_pane(pane)?.into());
            let mode = if let Some(n) = flag_value(rest, "--tail") {
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
            let pos = positional(rest);
            let pane = pos.first().ok_or("close needs a PANE")?;
            m.insert("pane".into(), parse_pane(pane)?.into());
            m.insert("force".into(), has_flag(rest, "--force").into());
        }
        "wait" => {
            let pos = positional(rest);
            if pos.is_empty() {
                return Err("wait needs at least one PANE".into());
            }
            let panes: Result<Vec<serde_json::Value>, String> =
                pos.iter().map(|p| parse_pane(p).map(Into::into)).collect();
            m.insert("panes".into(), serde_json::Value::Array(panes?));
            m.insert("until".into(), flag_value(rest, "--until").unwrap_or_else(|| "waiting".into()).into());
            if let Some(secs) = flag_value(rest, "--timeout") {
                let secs: u64 = secs.parse().map_err(|_| "--timeout needs a number (seconds)")?;
                m.insert("timeout_ms".into(), (secs * 1000).into());
            }
        }
        _ => return Err(format!("unknown verb: {verb}")),
    }
    Ok(serde_json::Value::Object(m))
}

fn parse_pane(s: &str) -> Result<u64, String> {
    s.parse().map_err(|_| format!("not a pane id: {s}"))
}

/// Args that aren't flags or flag values.
fn positional(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
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
    out
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
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

        assert_eq!(status_hook(None, String::new(), Some(sock.clone()), Some("working")), 0);
        assert_eq!(status_hook(Some(String::new()), String::new(), Some(sock.clone()), Some("working")), 0);
        assert_eq!(status_hook(Some("3".into()), "t".into(), Some(sock.clone()), None), 0);

        assert!(
            listener.accept().is_err(),
            "status_hook connected to the socket despite no pane/status"
        );
        let _ = std::fs::remove_dir_all(sock.parent().unwrap());
    }

    #[test]
    fn writes_exactly_the_status_line_the_socket_expects() {
        let (sock, listener) = scratch_sock("wire");

        let code = status_hook(Some("7".into()), "tok".into(), Some(sock.clone()), Some("needs_input"));
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

        let _ = std::fs::remove_dir_all(sock.parent().unwrap());
    }
}
