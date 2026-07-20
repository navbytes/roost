//! Generic shell adapter — panes that aren't agents. No sessions to resume;
//! "resume" just relaunches the shell in the saved cwd.

use super::{AgentAdapter, CommandSpec};
use std::path::Path;

pub struct ShellAdapter;

fn user_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into())
}

impl AgentAdapter for ShellAdapter {
    fn id(&self) -> &'static str {
        "shell"
    }

    fn launch(&self, cwd: &Path) -> CommandSpec {
        CommandSpec::new(user_shell(), cwd)
    }

    fn resume(&self, cwd: &Path, _session: &str) -> CommandSpec {
        self.launch(cwd)
    }
}
