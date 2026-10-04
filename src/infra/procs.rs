//! The one place roost signals a process it did not just fork.
//!
//! Every caller used to reach for `libc::kill` behind its own `pid > 1`
//! guard and its own SAFETY comment: `kill(-pid)` with a pid of 0 is roost's
//! own process group, and with a pid of 1 it is `kill(-1)` — every process
//! the caller may signal, which is the whole machine. rustix makes the call
//! itself safe and refuses 0 in its types (`Pid` is non-zero), but it still
//! represents 1, so the guard has to live somewhere. It lives here, once:
//! `target` is the only way to turn a raw id into something `kill` accepts,
//! and it says no to everything that is not a plausible pane or pane
//! descendant.

use std::io;

use rustix::process::{getsid, kill_process, kill_process_group, Pid, Signal};

/// A raw id as a signal target, or `None` for the ids that must never be
/// one: 0 (roost's own group), 1 (init — and `-1` for a group kill), and
/// anything that does not fit a `pid_t`.
fn target(raw: u32) -> Option<Pid> {
    i32::try_from(raw).ok().filter(|r| *r > 1).and_then(Pid::from_raw)
}

fn refused() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "refusing to signal pid 0, pid 1 or a non-pid")
}

/// SIGHUP one process — what a closed terminal would send it.
pub fn hangup(pid: u32) -> io::Result<()> {
    Ok(kill_process(target(pid).ok_or_else(refused)?, Signal::HUP)?)
}

/// SIGKILL one process.
pub fn kill(pid: u32) -> io::Result<()> {
    Ok(kill_process(target(pid).ok_or_else(refused)?, Signal::KILL)?)
}

/// SIGHUP a whole process group (`kill(-pgid)`).
pub fn hangup_group(pgid: u32) -> io::Result<()> {
    Ok(kill_process_group(target(pgid).ok_or_else(refused)?, Signal::HUP)?)
}

/// SIGKILL a whole process group (`kill(-pgid)`).
pub fn kill_group(pgid: u32) -> io::Result<()> {
    Ok(kill_process_group(target(pgid).ok_or_else(refused)?, Signal::KILL)?)
}

/// The session id of `pid`, or `None` when it is gone (or not ours to ask
/// about). `getsid(2)` answers for any pid the caller can see, dead leader
/// or not, which is what the macOS session sweep relies on.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn session_of(pid: u32) -> Option<u32> {
    let sid = getsid(Some(Pid::from_raw(i32::try_from(pid).ok()?)?)).ok()?;
    u32::try_from(sid.as_raw_pid()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::process::Command;

    #[test]
    fn ids_that_would_reach_roost_or_the_machine_are_refused() {
        // 0 is roost's own group, 1 is init / `kill(-1)`, and a value past
        // `i32::MAX` would wrap negative — a different broadcast target.
        for raw in [0, 1, i32::MAX as u32 + 1, u32::MAX] {
            assert!(target(raw).is_none(), "{raw} must not be a signal target");
            for r in [hangup(raw), kill(raw), hangup_group(raw), kill_group(raw)] {
                assert_eq!(r.unwrap_err().kind(), io::ErrorKind::InvalidInput, "{raw}");
            }
        }
        assert!(target(2).is_some());
    }

    #[test]
    fn kill_delivers_sigkill_to_the_named_process() {
        let mut child = Command::new("sleep").arg("60").spawn().unwrap();
        kill(child.id()).unwrap();
        assert_eq!(child.wait().unwrap().signal(), Some(9));
    }

    #[test]
    fn hangup_delivers_sighup_to_the_named_process() {
        let mut child = Command::new("sleep").arg("60").spawn().unwrap();
        hangup(child.id()).unwrap();
        assert_eq!(child.wait().unwrap().signal(), Some(1));
    }

    #[test]
    fn kill_group_reaches_a_group_led_by_its_target() {
        use std::os::unix::process::CommandExt;
        // `process_group(0)` makes the child its own group leader, which is
        // what `setsid` does for a pane — pgid == pid.
        let mut child = Command::new("sleep").arg("60").process_group(0).spawn().unwrap();
        kill_group(child.id()).unwrap();
        assert_eq!(child.wait().unwrap().signal(), Some(9));
    }

    #[test]
    fn session_of_answers_for_a_live_pid_and_not_for_a_refused_one() {
        assert!(session_of(std::process::id()).is_some());
        assert_eq!(session_of(0), None);
    }
}
