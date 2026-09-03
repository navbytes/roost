//! config.json — the key-bindings escape hatch (`src/ui/input.rs`) — is
//! actually read from wherever `ROOST_STATE` points, end to end. The parsing
//! and translation logic itself is unit-tested directly in `src/ui/input.rs`
//! (no PTY needed there); this file proves the one thing that genuinely
//! needs a real process: that roost finds the file in its *own* isolated
//! state dir rather than, say, silently skipping it or reading the
//! developer's real one. (Not a unit test: `$ROOST_STATE` is a process-global
//! env var, racy across unit tests running in parallel in one process — see
//! `src/infra/sock.rs`'s own tests for the same reason.)

#[allow(dead_code)]
mod harness;

use std::time::Duration;

/// Alt+Shift+z, meta-ESC encoding: `ESC` then the shifted glyph, the same
/// shape `tests/rekeyed_chords.rs`'s `ALT_SHIFT_S`/`ALT_SHIFT_M` use for
/// every other shifted-letter chord. This is roost's own ToggleFloat chord
/// (the 2026-09-03 re-key moved it here off `alt+f`, which collided at the
/// byte level with Alt+Right — see the amendment on `default_chord_action`
/// in `src/ui/input.rs`), so disabling it is a live discriminator the same
/// way disabling `alt+f` used to be: without the disable entry below, this
/// chord never reaches the pane at all, and only config.json being read
/// from the right place makes it arrive.
///
/// Deliberately *not* `alt+f` any more: `alt+f` is unbound by default after
/// the re-key, so it forwards to the pane whether or not config.json is
/// read at all — a test built on it would stay green while proving nothing.
const ALT_SHIFT_Z: &[u8] = b"\x1bZ";

#[test]
fn config_json_disabling_alt_shift_z_is_read_from_the_roost_state_dir() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let config = serde_json::json!({ "keys": { "alt+shift+z": "disable" } }).to_string();
    let Some(mut h) =
        harness::spawn_or_skip_with_config("config-keys gate", &harness::one_pane(cwd), &config)
    else {
        return;
    };
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");

    // Same ECHOCTL trick tests/cursor_mode.rs uses: park a dumb `cat` sink
    // so the bytes roost forwards echo back onto the screen verbatim.
    h.write_bytes(b"printf READY; cat\r");
    h.wait_for(Duration::from_secs(5), |s| s.contents().contains("READY"))
        .expect("pane never reached the cat sink");

    h.write_bytes(ALT_SHIFT_Z);
    // Without the disable entry, alt+shift+z is roost's own ToggleFloat
    // chord — it would never reach the pane at all, so this can only pass
    // if config.json was actually read from the harness's own ROOST_STATE
    // dir (which starts empty but for what the harness itself wrote into
    // it).
    if h.wait_for(Duration::from_secs(5), |s| s.contents().contains("^[Z")).is_none() {
        panic!(
            "alt+shift+z was not forwarded to the pane — config.json's disable entry \
             was not picked up from ROOST_STATE:\n{}",
            h.screen().contents()
        );
    }

    let _ = h.quit_and_wait(Duration::from_secs(5));
}

/// The `~/.config/roost/config.json` fallback, end to end, with no
/// `ROOST_STATE` in sight — the route a user who never read the docs
/// actually takes. Nothing is written to the state dir, so a `disable` that
/// takes effect can only have been read from the config dir.
///
/// Linux only, and not for lack of care: on macOS `dirs::state_dir()` is
/// `None`, so the state dir falls back to `dirs::data_local_dir()` —
/// `~/Library/Application Support` — which is exactly what
/// `dirs::config_dir()` returns there too. The two candidate paths are the
/// same file, so there is no fallback for a macOS test to distinguish. The
/// resolver's own handling of that equal-paths case is unit-tested in
/// `src/infra/config.rs` instead, on every platform.
#[cfg(target_os = "linux")]
#[test]
fn config_json_disabling_alt_shift_z_is_found_in_the_xdg_config_dir() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let config = serde_json::json!({ "keys": { "alt+shift+z": "disable" } }).to_string();
    let mut h = match harness::Harness::try_spawn_xdg(&harness::one_pane(cwd), Some(&config)) {
        Ok(h) => h,
        Err(reason) => {
            eprintln!("SKIP xdg-config gate: {reason}");
            return;
        }
    };
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");

    h.write_bytes(b"printf READY; cat\r");
    h.wait_for(Duration::from_secs(5), |s| s.contents().contains("READY"))
        .expect("pane never reached the cat sink");

    h.write_bytes(ALT_SHIFT_Z);
    // Same reasoning as the ROOST_STATE test above: without the disable
    // entry alt+shift+z is roost's own ToggleFloat chord and never reaches
    // the pane, so `^[Z` on screen proves the file was read — and here it
    // could only have come from $XDG_CONFIG_HOME/roost/.
    if h.wait_for(Duration::from_secs(5), |s| s.contents().contains("^[Z")).is_none() {
        panic!(
            "alt+shift+z was not forwarded to the pane — config.json was not picked up from \
             $XDG_CONFIG_HOME/roost/:\n{}",
            h.screen().contents()
        );
    }

    let _ = h.quit_and_wait(Duration::from_secs(5));
}

/// Alt+Left as a modern terminal delivers it: the xterm CSI-modifier form,
/// `CSI 1;3D` (modifier 3 = alt).
const ALT_LEFT_CSI: &[u8] = b"\x1b[1;3D";

/// The reported need: "I rely on Opt+arrow to move the cursor by words."
/// `disable` has to actually hand the key to the pane for that to work — and
/// until 2026-09-02 it did not: a disabled non-`Char` chord was *swallowed*,
/// so the chord reached neither roost nor the shell. A key that vanishes is
/// the worst of the three possible outcomes, and it was the one `disable`
/// produced on exactly the chords the keyword exists to give back.
///
/// This needs a PTY rather than a unit test because the interesting part is
/// the whole path: a real terminal's bytes in, crossterm's parse, the
/// override lookup, and the re-encode out into a live shell.
///
/// **What it pins, precisely.** roost reads parsed `KeyEvent`s
/// (`crossterm::event::read`), never the byte stream, so it cannot replay
/// what arrived — Alt+Left reaches it identically whether the terminal sent
/// `CSI 1;3D` or `ESC ESC [ D`, and it must therefore *choose* an encoding.
/// It chooses meta-ESC, the same convention every other forwarded Alt chord
/// already uses (`ESC Z` for Alt+Shift+z, two tests above), so `disable` speaks one
/// language rather than two. A shell binding the other spelling wants
/// `bindkey "^[[1;3D"` as well — documented in the README, and the reason
/// this test asserts the exact bytes instead of merely "something arrived".
#[test]
fn a_disabled_alt_arrow_is_handed_to_the_pane_rather_than_swallowed() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let config = serde_json::json!({ "keys": { "alt+left": "disable", "alt+right": "disable" } })
        .to_string();
    let Some(mut h) =
        harness::spawn_or_skip_with_config("disabled arrows", &harness::one_pane(cwd), &config)
    else {
        return;
    };
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");
    assert!(
        h.wait_for(Duration::from_secs(15), |s| s.contents().contains("1 main")).is_some(),
        "roost never drew its tab bar",
    );

    // `cat -v` renders control bytes literally, so the pane's own view of
    // what arrived is on screen — no guessing from behaviour.
    h.write_bytes(b"cat -v\r");
    h.settle(Duration::from_secs(2));
    h.write_bytes(ALT_LEFT_CSI);
    h.write_bytes(b"\r");
    assert!(
        h.wait_for(Duration::from_secs(5), |s| s.contents().contains("^[^[[D")).is_some(),
        "a disabled Alt+Left never reached the shell — it was swallowed:\n{}",
        h.screen().contents(),
    );

    assert!(h.quit_and_wait(Duration::from_secs(5)).is_some(), "roost did not exit cleanly");
}
