//! Observe what's *actually* running in a pane from the OS, so roost can
//! persist reality (the live cwd, and whether a known agent CLI is running)
//! rather than only what it launched. This is what lets a pane you `cd`'d and
//! typed `pi` into come back as pi in the right directory after a restart.
//!
//! Agents like pi and Claude Code are Node scripts, so their process `comm`
//! is "node" — we match on the command line's argv basenames instead (e.g.
//! `node /usr/local/bin/pi` → "pi"). We look at the pane's process and its
//! descendants, so it works whether the agent is the pane's direct child
//! (picker-launched) or a child of its shell (typed at the prompt).

use crate::ports::Observation;

/// Inspect `pid` (the pane's child) for its working directory and any known
/// agent running in its process subtree. Returns None when the process can't
/// be inspected at all (dead, or unsupported platform) so the caller leaves
/// the pane's persisted state untouched rather than clobbering it.
pub fn observe(pid: u32, known_agents: &[String]) -> Option<Observation> {
    platform::observe(pid, known_agents)
}

/// Does any argv element's file-name equal a known agent? (`node .../pi` → pi)
fn match_agent(cmdline_args: impl Iterator<Item = String>, known: &[String]) -> Option<String> {
    for arg in cmdline_args {
        if arg.is_empty() {
            continue;
        }
        let base = std::path::Path::new(&arg)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&arg);
        if let Some(a) = known.iter().find(|a| a.as_str() == base) {
            return Some(a.clone());
        }
    }
    None
}

#[cfg(target_os = "linux")]
mod platform {
    use super::{match_agent, Observation};
    use std::path::PathBuf;

    pub fn observe(pid: u32, known: &[String]) -> Option<Observation> {
        if !PathBuf::from(format!("/proc/{pid}")).exists() {
            return None; // process gone / not inspectable
        }
        // An empty cmdline means the process is mid-execve (or a kernel
        // thread) — a live shell/agent always has a non-empty one. Don't
        // trust such a sample; returning None leaves persisted state alone
        // rather than briefly mis-classifying the pane as a bare shell.
        match std::fs::read(format!("/proc/{pid}/cmdline")) {
            Ok(c) if !c.iter().all(|b| *b == 0) => {}
            _ => return None,
        }
        let cwd = std::fs::read_link(format!("/proc/{pid}/cwd")).ok();
        let agent = find_agent(pid, known);
        Some(Observation { cwd, agent })
    }

    fn cmd_agent(pid: u32, known: &[String]) -> Option<String> {
        let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        let args = raw
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned());
        match_agent(args, known)
    }

    fn children(pid: u32) -> Vec<u32> {
        let mut out = Vec::new();
        if let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) {
            for t in tasks.flatten() {
                if let Ok(list) = std::fs::read_to_string(t.path().join("children")) {
                    out.extend(list.split_whitespace().filter_map(|c| c.parse::<u32>().ok()));
                }
            }
        }
        out
    }

    /// Check the process and its descendants (breadth-first) for a known agent.
    fn find_agent(pid: u32, known: &[String]) -> Option<String> {
        let mut stack = vec![pid];
        let mut seen = 0;
        while let Some(p) = stack.pop() {
            seen += 1;
            if seen > 256 {
                break; // pathological tree guard
            }
            if let Some(a) = cmd_agent(p, known) {
                return Some(a);
            }
            stack.extend(children(p));
        }
        None
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{match_agent, Observation};
    use std::path::PathBuf;
    use std::process::Command;

    pub fn observe(pid: u32, known: &[String]) -> Option<Observation> {
        // The process's own command; empty means gone or mid-exec — either
        // way don't trust the sample (leaves persisted state untouched).
        let own = Command::new("ps")
            .args(["-o", "command=", "-p", &pid.to_string()])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        if own.is_empty() {
            return None;
        }
        let cwd = cwd_of(pid);
        let agent = find_agent(pid, known);
        Some(Observation { cwd, agent })
    }

    fn cwd_of(pid: u32) -> Option<PathBuf> {
        // `lsof -a -p <pid> -d cwd -Fn` → a line like `n/actual/path`.
        let out = Command::new("lsof")
            .args(["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"])
            .output()
            .ok()?;
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some(path) = line.strip_prefix('n') {
                return Some(PathBuf::from(path));
            }
        }
        None
    }

    fn cmd_agent(pid: u32, known: &[String]) -> Option<String> {
        let out = Command::new("ps").args(["-o", "command=", "-p", &pid.to_string()]).output().ok()?;
        let cmd = String::from_utf8_lossy(&out.stdout);
        match_agent(cmd.split_whitespace().map(|s| s.to_string()), known)
    }

    fn child_pids(pid: u32) -> Vec<u32> {
        Command::new("pgrep")
            .args(["-P", &pid.to_string()])
            .output()
            .ok()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .split_whitespace()
                    .filter_map(|c| c.parse::<u32>().ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn find_agent(pid: u32, known: &[String]) -> Option<String> {
        let mut stack = vec![pid];
        let mut seen = 0;
        while let Some(p) = stack.pop() {
            seen += 1;
            if seen > 256 {
                break;
            }
            if let Some(a) = cmd_agent(p, known) {
                return Some(a);
            }
            stack.extend(child_pids(p));
        }
        None
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod platform {
    use super::Observation;
    pub fn observe(_pid: u32, _known: &[String]) -> Option<Observation> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::match_agent;

    #[test]
    fn matches_node_wrapped_agent_by_argv_basename() {
        let known = vec!["pi".to_string(), "claude".to_string()];
        let args = ["node", "/home/u/.npm-global/bin/pi"].iter().map(|s| s.to_string());
        assert_eq!(match_agent(args, &known), Some("pi".to_string()));
    }

    #[test]
    fn matches_native_binary() {
        let known = vec!["pi".to_string()];
        let args = ["/usr/local/bin/pi", "--session", "x"].iter().map(|s| s.to_string());
        assert_eq!(match_agent(args, &known), Some("pi".to_string()));
    }

    #[test]
    fn plain_shell_matches_nothing() {
        let known = vec!["pi".to_string(), "claude".to_string()];
        assert_eq!(match_agent(["bash"].iter().map(|s| s.to_string()), &known), None);
        assert_eq!(match_agent(["-zsh"].iter().map(|s| s.to_string()), &known), None);
    }

    #[test]
    fn does_not_match_substring() {
        let known = vec!["pi".to_string()];
        // "pinky" must not match "pi"
        assert_eq!(match_agent(["/bin/pinky"].iter().map(|s| s.to_string()), &known), None);
    }
}
