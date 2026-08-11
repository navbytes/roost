//! A saved workspace whose only tab holds no panes must still start usable.
//!
//! roost writes exactly that file itself: closing the last pane quits, but the
//! removal still runs, and with one tab left there is no tab to drop — so an
//! emptied `Stack` root reaches disk. Loading it drew chrome around a blank
//! body, with the dead-pane hints (`↵ relaunch`) because pane 0 has no
//! runtime, and no key that could make a pane. The repair lives in
//! `Workspace::validate_and_repair`; this pins the end-to-end symptom.

#[allow(dead_code)]
mod harness;

use std::time::Duration;

const EMPTY_TAB: &str = r#"{
  "version": 1,
  "active_tab": 0,
  "tabs": [
    { "name": "tab2", "layout": { "stack": { "children": [], "expanded": 0 } }, "panes": {} }
  ]
}"#;

#[test]
fn a_workspace_of_empty_tabs_starts_with_a_live_pane() {
    let Some(mut h) = harness::spawn_or_skip("empty-tab recovery", EMPTY_TAB) else {
        return;
    };
    // The repaired workspace is the default one: a single tab named "main".
    assert!(
        h.wait_for(Duration::from_secs(10), |s| s.contents().contains("1 main")).is_some(),
        "the tab bar never appeared: {}",
        h.screen().contents(),
    );
    assert!(h.settle(Duration::from_secs(5)), "the first frame never settled");

    let screen = h.screen().contents();
    // The pane is alive, so the hint bar offers the normal keys — not the
    // dead-pane relaunch set, which is what a paneless tab used to show.
    assert!(screen.contains("Alt+n"), "no normal-mode hints:\n{screen}");
    assert!(!screen.contains("relaunch"), "focused pane is dead:\n{screen}");
}
