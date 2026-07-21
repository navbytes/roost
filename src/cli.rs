//! Client mode: `roost <verb> ...` connects to a running roost's control socket,
//! issues one request, prints the reply, and exits. This is the tmux-style
//! actuation surface an LLM (or a human, or a script) drives — the same
//! `control::Request` the socket executes on the main loop.
//!
//! Targeting is daemonless: `$ROOST_SOCK` (set in every pane, so an in-pane
//! agent needs no config) else the default per-state-dir socket path.
//! Credential precedence: `$ROOST_CONTROL_TOKEN`, else `<state>/control.token`
//! (the fleet token), else `$ROOST_TOKEN` (an in-pane agent's own pane token,
//! which the ownership model scopes to its subtree).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

use crate::infra::sock::socket_path;
use crate::infra::store::FsStore;

const VERBS: &[&str] = &["list", "status", "spawn", "fork", "send", "read", "close", "wait"];

/// If the first CLI arg is a control verb, run as a client and return the exit
/// code. Otherwise return None so `main` launches the TUI.
pub fn maybe_run() -> Option<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let verb = args.first()?;
    if verb == "--help" || verb == "-h" {
        eprintln!("{USAGE}");
        return Some(0);
    }
    if !VERBS.contains(&verb.as_str()) {
        return None; // not a control verb → run the TUI
    }
    Some(run(&args))
}

const USAGE: &str = "\
roost — control a running instance:
  roost list
  roost status [PANE]
  roost spawn ADAPTER [--cwd DIR] [--input TEXT]
  roost fork [PANE]
  roost send PANE TEXT... [--enter]
  roost read PANE [--tail N | --full]
  roost close PANE [--force]
  roost wait PANE... [--until STATUS] [--timeout SEC]
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

/// The control credential: explicit env, else the fleet token file, else the
/// pane's own token (for an in-pane agent).
fn resolve_token() -> String {
    if let Ok(t) = std::env::var("ROOST_CONTROL_TOKEN") {
        return t;
    }
    let path = FsStore::default_path().with_file_name("control.token");
    if let Ok(t) = std::fs::read_to_string(path) {
        return t.trim().to_string();
    }
    std::env::var("ROOST_TOKEN").unwrap_or_default()
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
            let pane = pos.first().ok_or("send needs a PANE")?;
            m.insert("pane".into(), parse_pane(pane)?.into());
            let text = pos[1..].join(" ");
            m.insert("text".into(), text.into());
            m.insert("submit".into(), has_flag(rest, "--enter").into());
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

fn send_request(sock: &std::path::Path, req: &serde_json::Value) -> std::io::Result<serde_json::Value> {
    let mut stream = UnixStream::connect(sock)?;
    let mut line = serde_json::to_string(req).unwrap_or_default();
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    stream.flush()?;
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
}
