//! Status socket (design doc §6.1): agent-side extensions/hooks report exact
//! status and session ids as newline-delimited JSON over a unix socket.
//!
//! Message shape (pane comes from the ROOST_PANE env var roost sets):
//!   { "pane": "3", "event": "session", "session": "<uuid>" }
//!   { "pane": "3", "event": "status",  "status": "working" | "waiting"
//!                                              | "needs_input" | "exited" }

use anyhow::Result;
use serde::Deserialize;
use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::mpsc::Sender;

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
    session: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

fn parse_line(line: &str) -> Option<AppEvent> {
    let msg: Msg = serde_json::from_str(line).ok()?;
    let pane: PaneId = match &msg.pane {
        serde_json::Value::String(s) => s.parse().ok()?,
        serde_json::Value::Number(n) => n.as_u64()?,
        _ => return None,
    };
    match msg.event.as_str() {
        "session" => Some(AppEvent::Session(pane, msg.session?)),
        "status" => {
            let status = match msg.status?.as_str() {
                "working" => AgentStatus::Working,
                "waiting" => AgentStatus::Waiting,
                "needs_input" => AgentStatus::NeedsInput,
                "exited" => AgentStatus::Exited,
                _ => return None,
            };
            Some(AppEvent::Status(pane, status))
        }
        _ => None,
    }
}

/// Bind the socket and pump parsed events into the main loop. Returns the
/// bound path (exported to panes as ROOST_SOCK).
pub fn spawn_listener(tx: Sender<AppEvent>) -> Result<PathBuf> {
    let path = socket_path();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let _ = fs::remove_file(&path); // stale socket from a previous run
    let listener = UnixListener::bind(&path)?;

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let tx = tx.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stream).lines() {
                    let Ok(line) = line else { break };
                    if let Some(ev) = parse_line(&line) {
                        if tx.send(ev).is_err() {
                            break;
                        }
                    }
                }
            });
        }
    });
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_string_and_numeric_pane_ids() {
        let ev = parse_line(r#"{"pane":"7","event":"status","status":"working"}"#);
        assert!(matches!(ev, Some(AppEvent::Status(7, AgentStatus::Working))));
        let ev = parse_line(r#"{"pane":7,"event":"session","session":"abc-123"}"#);
        match ev {
            Some(AppEvent::Session(7, s)) => assert_eq!(s, "abc-123"),
            _ => panic!("expected session event"),
        }
    }

    #[test]
    fn ignores_garbage() {
        assert!(parse_line("not json").is_none());
        assert!(parse_line(r#"{"pane":"x","event":"status","status":"working"}"#).is_none());
        assert!(parse_line(r#"{"pane":"1","event":"status","status":"???"}"#).is_none());
    }
}
