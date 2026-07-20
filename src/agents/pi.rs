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
use std::path::{Path, PathBuf};

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

    /// pi organizes sessions under ~/.pi/agent/sessions/ by cwd. We scan the
    /// whole root: only files newer than our spawn time matter, and a fresh
    /// pane can only have produced one of those.
    fn session_root(&self, _cwd: &Path) -> Option<PathBuf> {
        Some(dirs::home_dir()?.join(".pi").join("agent").join("sessions"))
    }
}
