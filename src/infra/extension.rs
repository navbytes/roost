//! Keep the pi status extension in sync with this build.
//!
//! roost's status/session reporting for pi panes depends on the `roost.ts`
//! extension living at `~/.pi/agent/extensions/roost.ts`. Because roost doesn't
//! (yet) ship a package that installs it, users copy it by hand — and it then
//! silently rots when roost's socket protocol changes (e.g. the per-pane token,
//! which roost now *requires*: a stale extension's messages are dropped).
//!
//! On startup we install/update it from the copy compiled into the binary,
//! but only when pi is actually set up (we never create `~/.pi`), and only when
//! the on-disk copy is missing or differs. Opt out with `ROOST_NO_EXT_INSTALL`.

use std::path::PathBuf;

/// The extension source, embedded at build time so the binary is self-contained.
const BUNDLED: &str = include_str!("../../extensions/roost.ts");

/// Ensure `~/.pi/agent/extensions/roost.ts` matches this build. Returns a short
/// message to surface when it installed or updated the file, else None.
pub fn ensure_pi_extension() -> Option<String> {
    if std::env::var_os("ROOST_NO_EXT_INSTALL").is_some() {
        return None;
    }
    let agent_dir = dirs::home_dir()?.join(".pi").join("agent");
    // Only touch things when pi is present — never create ~/.pi ourselves.
    if !agent_dir.is_dir() {
        return None;
    }
    let ext_dir = agent_dir.join("extensions");
    let target = ext_dir.join("roost.ts");

    let existing = std::fs::read_to_string(&target).ok();
    if existing.as_deref() == Some(BUNDLED) {
        return None; // already current
    }
    let updating = existing.is_some();

    std::fs::create_dir_all(&ext_dir).ok()?;
    write_atomic(&target, BUNDLED)?;

    Some(if updating {
        "updated the roost pi extension to match this build".into()
    } else {
        "installed the roost pi extension (~/.pi/agent/extensions/roost.ts)".into()
    })
}

/// Write via a temp file + rename so a crash can't leave a half-written
/// extension that pi would try to load.
fn write_atomic(target: &PathBuf, contents: &str) -> Option<()> {
    let tmp = target.with_extension("ts.tmp");
    std::fs::write(&tmp, contents).ok()?;
    std::fs::rename(&tmp, target).ok()
}
