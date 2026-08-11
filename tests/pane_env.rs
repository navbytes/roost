//! P11 (SPEC-parity) end to end: host-identity env must not leak into panes.
//!
//! roost is launched with the identity vars a real host stack would set
//! (iTerm2 + an outer tmux + kitty + …); the pane's shell dumps its actual
//! environment to a file. The pane must see roost's identity
//! (`TERM_PROGRAM=roost` + version), none of the host's, and the unchanged
//! TERM / ROOST_* contract.
//!
//! P18 shares this binary because it asks the same question of the same
//! surface — what environment does a pane's shell actually come up with — and
//! reuses the env-dump seam below.

// The shared harness is compiled per test binary; helpers other tenants use
// are dead code from this binary's view — not real rot.
#[allow(dead_code)]
mod harness;

use std::time::{Duration, Instant};

/// Poll for a file to exist non-empty, returning its contents.
fn wait_for_file(path: &std::path::Path, timeout: Duration) -> Option<String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(s) = std::fs::read_to_string(path) {
            if !s.trim().is_empty() {
                return Some(s);
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The value of `key` in an `env(1)` dump, matched on whole lines so
/// `TMUX=` can never be satisfied by `TMUX_PANE=`.
fn env_val<'a>(dump: &'a str, key: &str) -> Option<&'a str> {
    dump.lines().find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
}

#[test]
fn pane_children_see_roost_identity_not_the_hosts() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    // Every var P11 names, valued as the host stack would set them.
    let host_env: &[(&str, &str)] = &[
        ("TERM_PROGRAM", "iTerm.app"),
        ("TERM_PROGRAM_VERSION", "3.5.9"),
        ("KITTY_WINDOW_ID", "3"),
        ("KITTY_PID", "4242"),
        ("ITERM_SESSION_ID", "w0t0p0:AAAA"),
        ("ITERM_PROFILE", "Default"),
        ("WEZTERM_PANE", "9"),
        ("WEZTERM_UNIX_SOCKET", "/tmp/wezterm-sock"),
        ("TMUX", "/tmp/tmux-1000/default,42,0"),
        ("TMUX_PANE", "%7"),
        ("ZELLIJ", "0"),
        ("ZELLIJ_SESSION_NAME", "jazzy-lemur"),
        ("VSCODE_INJECTION", "1"),
    ];
    let Some(mut h) =
        harness::spawn_or_skip_with_env("pane env gate", &harness::one_pane(cwd), host_env)
    else {
        return;
    };
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");

    // The pane dumps its whole env; the done-marker file makes the read
    // race-free (the dump is complete before the marker exists).
    let env_path = h.state_dir().join("pane.env");
    let done_path = h.state_dir().join("pane.env.done");
    h.write_bytes(
        format!("env > {} && printf ok > {}\r", env_path.display(), done_path.display())
            .as_bytes(),
    );
    wait_for_file(&done_path, Duration::from_secs(5)).expect("pane shell never dumped its env");
    let dump = std::fs::read_to_string(&env_path).expect("read pane env dump");

    // roost's identity, not the host's.
    assert_eq!(env_val(&dump, "TERM_PROGRAM"), Some("roost"), "dump:\n{dump}");
    assert_eq!(
        env_val(&dump, "TERM_PROGRAM_VERSION"),
        Some(env!("CARGO_PKG_VERSION")),
        "dump:\n{dump}"
    );
    // Every other host-identity var is gone entirely.
    for (var, _) in host_env {
        if *var == "TERM_PROGRAM" || *var == "TERM_PROGRAM_VERSION" {
            continue;
        }
        assert_eq!(env_val(&dump, var), None, "{var} leaked into the pane; dump:\n{dump}");
    }
    // TERM / ROOST_* behavior unchanged.
    assert_eq!(env_val(&dump, "TERM"), Some("xterm-256color"));
    assert_eq!(env_val(&dump, "ROOST_PANE"), Some("1"));

    let _ = h.quit_and_wait(Duration::from_secs(5));
}

/// P18 end to end: a pane's shell must be a *login* shell.
///
/// The probe is deliberately behavioral rather than a flag check. What P18 is
/// actually about is the user's login profile running — `~/.zprofile` putting
/// Homebrew on PATH so `claude`/`pi` resolve — so the gate gives the pane a
/// private `$HOME` holding a `.profile` that exports a marker, and then asks
/// the pane's own environment whether it ran. A non-login shell never sources
/// it, which is exactly the "works in a terminal tab, `command not found` in
/// the mux" failure (tmux #1623). The harness pins `SHELL=/bin/sh`, and both
/// dash and bash read `~/.profile` as a login shell when no `.bash_profile`
/// exists, so one fixture file covers whichever `/bin/sh` is.
#[test]
fn a_pane_shell_runs_its_login_profile() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let home =
        std::env::temp_dir().join(format!("roost-p18-home-{}-{stamp}", std::process::id()));
    std::fs::create_dir_all(&home).expect("create fixture HOME");
    std::fs::write(
        home.join(".profile"),
        "ROOST_LOGIN_PROBE=profile-ran\nexport ROOST_LOGIN_PROBE\n",
    )
    .expect("write fixture .profile");

    let home_str = home.to_str().expect("fixture HOME is valid utf8").to_string();
    let Some(mut h) = harness::spawn_or_skip_with_env(
        "login-shell gate",
        &harness::one_pane(cwd),
        &[("HOME", home_str.as_str())],
    ) else {
        let _ = std::fs::remove_dir_all(&home);
        return;
    };
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");

    let env_path = h.state_dir().join("login.env");
    let done_path = h.state_dir().join("login.env.done");
    h.write_bytes(
        format!("env > {} && printf ok > {}\r", env_path.display(), done_path.display())
            .as_bytes(),
    );
    let dumped = wait_for_file(&done_path, Duration::from_secs(5)).is_some();
    let dump = dumped
        .then(|| std::fs::read_to_string(&env_path).ok())
        .flatten()
        .unwrap_or_default();
    let _ = std::fs::remove_dir_all(&home);
    assert!(dumped, "pane shell never dumped its env");

    assert_eq!(
        env_val(&dump, "ROOST_LOGIN_PROBE"),
        Some("profile-ran"),
        "the pane's shell never sourced ~/.profile, so it is not a login shell; dump:\n{dump}"
    );
    // The fixture HOME really was the one in force — otherwise the assertion
    // above could only ever have failed, proving nothing.
    assert_eq!(env_val(&dump, "HOME"), Some(home_str.as_str()), "dump:\n{dump}");

    let _ = h.quit_and_wait(Duration::from_secs(5));
}
