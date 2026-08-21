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
    //! Everything here used to be a subprocess: `lsof` for the cwd, `ps` for
    //! a process's argv, `pgrep -P` for its children — and `observe` runs on
    //! the event-loop thread every `DETECT_INTERVAL`, once per pane, walking
    //! each pane's whole subtree. Measured on macOS 15: `lsof` ~44 ms,
    //! `pgrep -P` ~49 ms, `ps -p` ~3 ms *each*, so two shell panes cost the
    //! UI ~300 ms of dead time every 2 s (tests/firehose.rs caught it as a
    //! single 344 ms echo-latency spike among 20 keystrokes at 88 ms). A
    //! pane whose subtree has no agent walks all of it — up to the 256-node
    //! guard below, i.e. seconds of frozen UI.
    //!
    //! macOS answers all three questions with plain syscalls, no fork, no
    //! exec, no dependency: `proc_pidinfo(PROC_PIDVNODEPATHINFO)` for the
    //! cwd, `sysctl(KERN_PROCARGS2)` for argv, `proc_listchildpids` for the
    //! children. Same answers, microseconds instead of milliseconds.

    use super::{match_agent, Observation};
    use libc::{c_int, c_void};
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::path::PathBuf;

    pub fn observe(pid: u32, known: &[String]) -> Option<Observation> {
        // The process's own argv; empty means gone or mid-exec — either way
        // don't trust the sample (leaves persisted state untouched).
        let own = proc_args(pid)?;
        if own.is_empty() {
            return None;
        }
        let cwd = cwd_of(pid);
        let agent = find_agent(pid, known);
        Some(Observation { cwd, agent })
    }

    /// The process's current working directory, straight from the kernel.
    fn cwd_of(pid: u32) -> Option<PathBuf> {
        // SAFETY: `proc_vnodepathinfo` is a plain C struct of integers and
        // fixed-size char arrays — no references, no `NonNull`, no enums —
        // so all-zeroes is a valid value for it, and `proc_pidinfo` below
        // overwrites the part it fills.
        let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
        let want = std::mem::size_of::<libc::proc_vnodepathinfo>() as c_int;
        // SAFETY: out-pointer to a local of exactly the size we declare, and
        // `PROC_PIDVNODEPATHINFO` is the flavor that fills that struct.
        let got = unsafe {
            libc::proc_pidinfo(
                pid as c_int,
                libc::PROC_PIDVNODEPATHINFO,
                0,
                &mut info as *mut _ as *mut c_void,
                want,
            )
        };
        // A short write means the kernel didn't fill the struct (process gone,
        // or not ours to inspect) — the path bytes would be garbage.
        if got != want {
            return None;
        }
        // libc models `vip_path: [c_char; MAXPATHLEN]` as [[c_char; 32]; 32]
        // for old-rustc reasons; it is one contiguous NUL-terminated path,
        // and `as_flattened` says so without an `unsafe` transmute.
        let bytes: Vec<u8> = info.pvi_cdir.vip_path[..]
            .as_flattened()
            .iter()
            .take_while(|c| **c != 0)
            .map(|c| *c as u8)
            .collect();
        if bytes.is_empty() {
            return None;
        }
        Some(PathBuf::from(OsString::from_vec(bytes)))
    }

    /// `KERN_ARGMAX` — the buffer size `KERN_PROCARGS2` demands (1 MiB on
    /// macOS 15). Constant for the life of the machine, so it is read once;
    /// a failed read disables argv inspection rather than guessing.
    ///
    /// The full size is not optional and an undersized buffer is not a
    /// recoverable error: measured, `sysctl` with 4 KiB returns **0** and
    /// sets `len` to the buffer size, having written bytes that parse to no
    /// argv at all. There is no truncation signal to check — the only way to
    /// know the answer is real is to have asked for `KERN_ARGMAX`.
    fn argmax() -> usize {
        use std::sync::OnceLock;
        static ARGMAX: OnceLock<usize> = OnceLock::new();
        *ARGMAX.get_or_init(|| {
            let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
            let mut value: c_int = 0;
            let mut len = std::mem::size_of::<c_int>();
            // SAFETY: 2-element mib, out-pointer to a local of the length we pass.
            let rc = unsafe {
                libc::sysctl(
                    mib.as_mut_ptr(),
                    2,
                    &mut value as *mut _ as *mut c_void,
                    &mut len,
                    std::ptr::null_mut(),
                    0,
                )
            };
            if rc == 0 && value > 0 {
                value as usize
            } else {
                0
            }
        })
    }

    /// A process's executable path followed by its argv, as `ps -o command=`
    /// used to give us. `None` when the process is gone or isn't ours to read
    /// (`KERN_PROCARGS2` is same-uid only), which is exactly the "don't trust
    /// this sample" signal the empty `ps` output used to be.
    fn proc_args(pid: u32) -> Option<Vec<String>> {
        let max = argmax();
        if max == 0 {
            return None;
        }
        // One allocation for the life of the thread, reused by every pane
        // and every node of every subtree walk — rather than a 1 MiB
        // alloc-and-zero per process inspected.
        // ponytail: the buffer is never shrunk, so the event-loop thread
        // keeps ~1 MiB resident. Free it between ticks only if roost's RSS
        // ever becomes something anyone measures.
        thread_local! {
            static BUF: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
        }
        BUF.with(|cell| {
            let mut buf = cell.borrow_mut();
            buf.resize(max, 0);
            let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as c_int];
            let mut len = buf.len();
            // SAFETY: 3-element mib, out-buffer of exactly `len` bytes.
            let rc = unsafe {
                libc::sysctl(
                    mib.as_mut_ptr(),
                    3,
                    buf.as_mut_ptr() as *mut c_void,
                    &mut len,
                    std::ptr::null_mut(),
                    0,
                )
            };
            if rc != 0 || len < 4 {
                return None;
            }
            Some(parse_procargs2(&buf[..len]))
        })
    }

    /// `KERN_PROCARGS2`'s layout: a 4-byte `argc`, the NUL-terminated
    /// executable path, NUL padding, then `argc` NUL-terminated argv strings
    /// (the environment follows, and is deliberately not returned).
    fn parse_procargs2(raw: &[u8]) -> Vec<String> {
        let argc = i32::from_ne_bytes([raw[0], raw[1], raw[2], raw[3]]).max(0) as usize;
        raw[4..]
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .take(argc.saturating_add(1)) // exec path + argv
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect()
    }

    fn cmd_agent(pid: u32, known: &[String]) -> Option<String> {
        match_agent(proc_args(pid)?.into_iter(), known)
    }

    /// Direct children of `pid`.
    ///
    /// `proc_listchildpids` returns the *count* of pids it wrote (verified on
    /// macOS 15, not merely assumed from the header) — but the `> 0` filter
    /// below makes the parse correct either way, since a byte count would
    /// only ever over-read into the zeroed tail.
    ///
    /// ponytail: one fixed 1024-pid buffer instead of a size-then-fill pair —
    /// a single process with more than 1024 direct children would have the
    /// tail ignored, which costs at worst a missed agent detection for one
    /// 2 s tick. Ask the kernel for the count first if that ever happens.
    fn child_pids(pid: u32) -> Vec<u32> {
        let mut buf = [0i32; 1024];
        // SAFETY: out-buffer of exactly the byte length we declare.
        let n = unsafe {
            libc::proc_listchildpids(
                pid as libc::pid_t,
                buf.as_mut_ptr() as *mut c_void,
                std::mem::size_of_val(&buf) as c_int,
            )
        };
        if n <= 0 {
            return Vec::new();
        }
        let n = (n as usize).min(buf.len());
        buf[..n].iter().filter(|p| **p > 0).map(|p| *p as u32).collect()
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

    #[cfg(test)]
    mod tests {
        use super::*;

        /// The three syscalls answer for *this* process the way the three
        /// subprocesses used to: our own cwd, our own argv, and — with a
        /// child we just forked — that child in our subtree.
        #[test]
        fn syscalls_answer_for_our_own_process() {
            let me = std::process::id();
            assert_eq!(
                cwd_of(me).unwrap().canonicalize().unwrap(),
                std::env::current_dir().unwrap().canonicalize().unwrap()
            );
            let args = proc_args(me).expect("our own argv is readable");
            assert!(!args.is_empty(), "argv must not be empty: {args:?}");
            // The test binary's own path is argv[0] (and the exec path).
            assert!(
                args.iter().any(|a| a.contains("inspect") || a.contains("roost")),
                "argv should name this binary: {args:?}"
            );
        }

        /// A spawned child shows up under us, and `find_agent` finds it by
        /// argv basename — the `node .../pi` case, one level down.
        #[test]
        fn finds_a_known_agent_in_a_child_process() {
            let mut child = std::process::Command::new("/bin/sleep")
                .arg("30")
                .spawn()
                .expect("spawn sleep");
            let kid = child.id();
            // The kernel lists it immediately; no settling needed.
            assert!(child_pids(std::process::id()).contains(&kid), "child not listed");
            let known = vec!["sleep".to_string()];
            assert_eq!(find_agent(std::process::id(), &known), Some("sleep".into()));
            let _ = child.kill();
            let _ = child.wait();
        }

        /// A dead pid yields nothing at all, which is what tells `observe` to
        /// leave the pane's persisted state untouched.
        #[test]
        fn a_dead_pid_is_not_observable() {
            let mut child =
                std::process::Command::new("/usr/bin/true").spawn().expect("spawn true");
            let dead = child.id();
            let _ = child.wait();
            assert!(observe(dead, &["pi".to_string()]).is_none());
        }

        /// The `KERN_PROCARGS2` layout parse, pinned without a live process:
        /// argc, exec path, NUL padding, then exactly argc argv strings —
        /// and never the environment that follows them.
        #[test]
        fn procargs2_parse_stops_after_argv() {
            let mut raw = 2i32.to_ne_bytes().to_vec();
            raw.extend_from_slice(b"/usr/bin/node\0\0\0");
            raw.extend_from_slice(b"node\0/usr/local/bin/pi\0");
            raw.extend_from_slice(b"SECRET=hunter2\0");
            let args = parse_procargs2(&raw);
            assert_eq!(args, vec!["/usr/bin/node", "node", "/usr/local/bin/pi"]);
            assert_eq!(match_agent(args.into_iter(), &["pi".to_string()]), Some("pi".into()));
        }
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
