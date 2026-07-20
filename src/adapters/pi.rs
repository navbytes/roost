//! pi (badlogic/pi-mono coding agent) adapter.
//!
//! Ground truth (pi docs):
//! - sessions auto-persist to `~/.pi/agent/sessions/`, organized by cwd
//! - `pi --session <path|id>` resumes an exact session (partial UUID ok)
//! - `-c/--continue` resumes the most recent session in cwd
//! - extensions live in `~/.pi/agent/extensions/*.ts` and get lifecycle
//!   events (`session_start`, `agent_start`, `agent_end`, `session_shutdown`)
//!
//! Status/session detection: the bundled `extensions/roost.ts` pi extension
//! reports exact events over a unix socket ($XDG_RUNTIME_DIR/roost.sock),
//! tagged with the ROOST_PANE env var we set at spawn. See design doc §6.1.

use super::{AgentAdapter, CommandSpec};
use std::path::Path;

pub struct PiAdapter;

impl AgentAdapter for PiAdapter {
    fn id(&self) -> &'static str {
        "pi"
    }

    fn launch(&self, cwd: &Path) -> CommandSpec {
        CommandSpec::new("pi", cwd)
    }

    fn resume(&self, cwd: &Path, session: &str) -> CommandSpec {
        CommandSpec::new("pi", cwd).arg("--session").arg(session)
    }

    fn detect_session(&self, _cwd: &Path) -> Option<String> {
        // TODO(M2): diff ~/.pi/agent/sessions/<cwd-encoded>/ before/after
        // spawn as a fallback when the roost.ts extension isn't installed.
        None
    }
}
