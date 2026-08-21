//! Generic shell adapter — panes that aren't agents. No sessions to resume;
//! "resume" just relaunches the shell in the saved cwd.

use super::{AgentAdapter, CommandSpec};
use std::path::Path;

pub struct ShellAdapter;

fn user_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into())
}

/// Shells whose `-l` is a documented login-shell flag.
///
/// P18: anything not on this list spawns bare. An unknown shell that rejects
/// the flag would die at birth, and every shell pane failing to start is a far
/// worse outcome than the missing profile this item fixes. The list covers
/// every shell `$SHELL` realistically names on a developer machine.
const LOGIN_FLAG_SHELLS: &[&str] =
    &["ash", "bash", "csh", "dash", "fish", "ksh", "ksh93", "mksh", "pdksh", "sh", "tcsh", "zsh"];

/// P18: shell panes are login shells, the way tmux and every terminal
/// emulator spawn theirs. Without it `~/.zprofile` never runs, so on macOS the
/// Homebrew PATH — where `claude` and `pi` usually live — is missing, and the
/// pane hits `command not found` for a binary that works fine in a terminal
/// tab (the tmux #1623 class of report).
///
/// **Why `-l` and not a dash-prefixed argv0**, which is what tmux and the
/// emulators actually use: roost spawns through portable-pty, whose
/// `CommandBuilder` derives *both* the executable to resolve and argv[0] from
/// the same `args[0]` (`cmdbuilder.rs`: `search_path(&args[0])`, then
/// `arg0(&args[0])`). There is no seam for an argv0 that differs from the
/// program, so `-zsh` would be looked up on `PATH` and fail. The two spellings
/// mean the same thing to every shell here — read the login profile — and `-l`
/// is the one this process layer can express. (The alternative,
/// `CommandBuilder::new_default_prog()`, does set a dash-argv0, but it picks
/// the shell itself and would take `CommandSpec::program` out of roost's
/// hands entirely.)
///
/// Pure and separate from `$SHELL` lookup so the decision is unit-testable
/// without mutating the process environment.
fn shell_spec(shell: &str, cwd: &Path) -> CommandSpec {
    let spec = CommandSpec::new(shell, cwd);
    let base = shell.rsplit('/').next().unwrap_or(shell);
    if LOGIN_FLAG_SHELLS.contains(&base) {
        spec.arg("-l")
    } else {
        spec
    }
}

impl AgentAdapter for ShellAdapter {
    fn id(&self) -> &'static str {
        "shell"
    }

    fn launch(&self, cwd: &Path) -> CommandSpec {
        shell_spec(&user_shell(), cwd)
    }

    fn resume(&self, cwd: &Path, _session: &str) -> CommandSpec {
        self.launch(cwd)
    }

    // Never consulted: `resume` above ignores the session id entirely.
    fn resume_flag(&self) -> &'static str {
        "--resume"
    }
}

#[cfg(test)]
mod tests {
    use super::{shell_spec, LOGIN_FLAG_SHELLS};
    use std::path::Path;

    #[test]
    fn known_shells_are_spawned_as_login_shells() {
        let cwd = Path::new("/tmp");
        for shell in ["/bin/zsh", "/bin/bash", "/usr/local/bin/fish", "/bin/sh"] {
            let spec = shell_spec(shell, cwd);
            assert_eq!(spec.program, shell);
            assert_eq!(spec.args, vec!["-l".to_string()], "{shell} should be a login shell");
            assert_eq!(spec.cwd, cwd);
        }
        // A bare name (no directory component) resolves the same way.
        assert_eq!(shell_spec("zsh", cwd).args, vec!["-l".to_string()]);
    }

    #[test]
    fn an_unrecognized_shell_is_spawned_bare() {
        // P18's safety valve: a shell that might reject `-l` must still start.
        // Losing the login profile is recoverable; a pane that dies at birth
        // is not.
        let cwd = Path::new("/tmp");
        for shell in ["/usr/bin/elvish", "/opt/weird/myshell", "/bin/bashful"] {
            assert!(shell_spec(shell, cwd).args.is_empty(), "{shell} should spawn bare");
        }
    }

    #[test]
    fn the_login_flag_list_matches_on_the_basename_only() {
        // `/opt/homebrew/bin/zsh` is as much zsh as `/bin/zsh` is, and a
        // shell merely *containing* a known name is not one.
        let cwd = Path::new("/tmp");
        assert_eq!(shell_spec("/opt/homebrew/bin/zsh", cwd).args, vec!["-l".to_string()]);
        assert!(shell_spec("/bin/notzsh", cwd).args.is_empty());
        assert!(LOGIN_FLAG_SHELLS.contains(&"zsh"));
    }
}
