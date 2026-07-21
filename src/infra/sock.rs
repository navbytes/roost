//! Status socket (design doc §6.1): agent-side extensions/hooks report exact
//! status and session ids as newline-delimited JSON over a unix socket.
//!
//! Message shape (pane comes from the ROOST_PANE env var roost sets; token
//! from ROOST_TOKEN — roost drops any message whose token doesn't match the
//! one it issued to that pane, so panes can't spoof each other):
//!   { "pane": "3", "token": "<hex>", "event": "session", "session": "<uuid>" }
//!   { "pane": "3", "token": "<hex>", "event": "status",  "status": "working"
//!                                    | "waiting" | "needs_input" | "exited" }

use anyhow::{bail, Result};
use serde::Deserialize;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::Arc;

use crate::core::control::Request;

/// Max bytes accepted for one status line. A well-formed message is well under
/// this; a client that streams without a newline is dropped instead of being
/// allowed to grow an unbounded buffer (local DoS).
const MAX_LINE: u64 = 64 * 1024;

/// Cap concurrent client connections so a buggy/looping extension that
/// reconnects rapidly can't spawn unbounded threads/FDs.
const MAX_CONN: usize = 64;

/// Is `dir` owned by us with no group/other access? Refusing otherwise stops
/// an attacker who pre-created the runtime dir from hosting our control socket
/// (tmux does the same for its socket dir).
fn dir_is_private_and_ours(dir: &Path) -> bool {
    match fs::metadata(dir) {
        Ok(m) => m.uid() == unsafe { libc::geteuid() } && (m.mode() & 0o077) == 0,
        Err(_) => false,
    }
}

/// Remove the socket file on clean exit so a stale socket isn't left behind.
pub fn cleanup(path: &Path) {
    let _ = fs::remove_file(path);
}

use crate::core::event::AppEvent;
use crate::core::status::AgentStatus;
use crate::core::workspace::PaneId;

pub fn socket_path() -> PathBuf {
    if let Some(dir) = std::env::var_os("ROOST_STATE") {
        return PathBuf::from(dir).join("roost.sock");
    }
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::state_dir()
                .or_else(dirs::data_local_dir)
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join("roost")
        })
        .join("roost.sock")
}

#[derive(Deserialize)]
struct Msg {
    pane: serde_json::Value, // tolerate string or number
    event: String,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

/// A control request is any message carrying a `method` field (status/session
/// reports don't). Returns None for non-control lines so they fall through to
/// the one-way `parse_line` path.
fn parse_control(line: &str) -> Option<Request> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    if v.get("method").is_none() {
        return None;
    }
    serde_json::from_value(v).ok()
}

fn parse_line(line: &str) -> Option<AppEvent> {
    let msg: Msg = serde_json::from_str(line).ok()?;
    let pane: PaneId = match &msg.pane {
        serde_json::Value::String(s) => s.parse().ok()?,
        serde_json::Value::Number(n) => n.as_u64()?,
        _ => return None,
    };
    // Missing token → empty string, which App rejects (fails closed).
    let token = msg.token.unwrap_or_default();
    match msg.event.as_str() {
        "session" => Some(AppEvent::Session(pane, token, msg.session?)),
        "status" => {
            let status = match msg.status?.as_str() {
                "working" => AgentStatus::Working,
                "waiting" => AgentStatus::Waiting,
                "needs_input" => AgentStatus::NeedsInput,
                "exited" => AgentStatus::Exited,
                _ => return None,
            };
            Some(AppEvent::Status(pane, token, status))
        }
        _ => None,
    }
}

/// Bind the socket and pump parsed events into the main loop. Returns the
/// bound path (exported to panes as ROOST_SOCK).
pub fn spawn_listener(tx: SyncSender<AppEvent>) -> Result<PathBuf> {
    let path = socket_path();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
        // The socket is the control plane (it can set session ids / status);
        // keep its directory private to the owner...
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
        // ...and refuse to run if it isn't actually ours and private (an
        // attacker may have pre-created it to intercept the socket).
        if !dir_is_private_and_ours(dir) {
            bail!("roost: socket directory {} has unsafe ownership/permissions", dir.display());
        }
    }
    let _ = fs::remove_file(&path); // stale socket from a previous run
    let listener = UnixListener::bind(&path)?;
    // Restrict the socket to the owner so another local user can't connect and
    // poison session ids / spoof status.
    let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));

    let conns = Arc::new(AtomicUsize::new(0));
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            // Shed load past the connection cap rather than spawning threads
            // without bound.
            if conns.load(Ordering::Relaxed) >= MAX_CONN {
                drop(stream);
                continue;
            }
            conns.fetch_add(1, Ordering::Relaxed);
            let conns = conns.clone();
            let tx = tx.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream);
                let mut buf = Vec::new();
                loop {
                    buf.clear();
                    // Cap the bytes read per line so a newline-less flood can't
                    // grow the buffer without bound.
                    let n = match reader.by_ref().take(MAX_LINE).read_until(b'\n', &mut buf) {
                        Ok(0) => break,       // EOF
                        Ok(n) => n,
                        Err(_) => break,
                    };
                    // Hit the cap without terminating — oversized line; drop the
                    // connection rather than trying to resync.
                    if buf.last() != Some(&b'\n') && n as u64 == MAX_LINE {
                        break;
                    }
                    let Ok(line) = std::str::from_utf8(&buf) else { continue };
                    let line = line.trim_end();
                    // Control request (has a `method`): execute on the main loop
                    // and write the reply back down this connection.
                    if let Some(req) = parse_control(line) {
                        let (rtx, rrx) = std::sync::mpsc::channel();
                        if tx.send(AppEvent::Command(req, rtx)).is_err() {
                            break; // main gone
                        }
                        let Ok(reply) = rrx.recv() else { break };
                        let mut json = match serde_json::to_string(&reply) {
                            Ok(s) => s,
                            Err(_) => continue,
                        };
                        json.push('\n');
                        if reader.get_mut().write_all(json.as_bytes()).is_err() {
                            break; // client hung up
                        }
                        continue;
                    }
                    match parse_line(line) {
                        Some(ev) => {
                            if tx.send(ev).is_err() {
                                break;
                            }
                        }
                        // A malformed line usually means a broken extension /
                        // hook integration — log it (ROOST_DEBUG) so it's
                        // debuggable instead of silently vanishing.
                        None => log_dropped(line),
                    }
                }
                conns.fetch_sub(1, Ordering::Relaxed);
            });
        }
    });
    Ok(path)
}

/// Append an unparseable socket line to `<state>/roost.log` when ROOST_DEBUG
/// is set. No-op otherwise (and never touches the TUI's stdout).
fn log_dropped(line: &str) {
    if std::env::var_os("ROOST_DEBUG").is_none() {
        return;
    }
    use std::io::Write;
    let log = socket_path().with_file_name("roost.log");
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(log) {
        let _ = writeln!(f, "dropped malformed socket line: {line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_string_and_numeric_pane_ids() {
        let ev = parse_line(r#"{"pane":"7","event":"status","token":"tok","status":"working"}"#);
        assert!(matches!(ev, Some(AppEvent::Status(7, ref t, AgentStatus::Working)) if t == "tok"));
        let ev = parse_line(r#"{"pane":7,"event":"session","token":"tok","session":"abc-123"}"#);
        match ev {
            Some(AppEvent::Session(7, t, s)) => {
                assert_eq!(t, "tok");
                assert_eq!(s, "abc-123");
            }
            _ => panic!("expected session event"),
        }
    }

    #[test]
    fn missing_token_parses_as_empty_and_is_rejected_downstream() {
        // A message without a token still parses (empty token), but App's
        // socket_authorized fails closed on an empty token.
        let ev = parse_line(r#"{"pane":"7","event":"status","status":"working"}"#);
        assert!(matches!(ev, Some(AppEvent::Status(7, ref t, _)) if t.is_empty()));
    }

    #[test]
    fn ignores_garbage() {
        assert!(parse_line("not json").is_none());
        assert!(parse_line(r#"{"pane":"x","event":"status","status":"working"}"#).is_none());
        assert!(parse_line(r#"{"pane":"1","event":"status","status":"???"}"#).is_none());
    }
}
