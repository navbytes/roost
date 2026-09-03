//! Named workspaces, end to end against the real binary (change
//! `named-workspaces`, spec `workspaces` + `control-targeting`): several
//! instances at once under one `ROOST_STATE` root, one instance per
//! workspace, the offline `ws` verbs, cross-instance session claims, and the
//! identity signals a named workspace emits.
//!
//! The unit tests in `src/infra/store.rs` and `src/cli.rs` prove the pure
//! halves (name grammar, selection precedence, row formatting); these prove
//! what a user experiences — `roost -w a` and `roost -w b` both start,
//! `ws ls` sees both, a second instance on a running workspace is refused,
//! and a claimed session blocks the second window's restore until the first
//! quits.

#[allow(dead_code)]
mod harness;

use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(10);

/// One shell pane (`panes` of them, actually) seeded straight into
/// `<root>/workspaces/<name>/` — a named workspace's state directory holds
/// exactly what a state directory holds.
fn seed_workspace(
    root: &std::path::Path,
    name: &str,
    panes: usize,
    adapter: &str,
    session: Option<&str>,
) {
    let dir = root.join("workspaces").join(name);
    std::fs::create_dir_all(&dir).expect("create the workspace directory");
    let mut pane_specs = serde_json::Map::new();
    for i in 1..=panes as u64 {
        let mut spec = serde_json::json!({"adapter": adapter, "cwd": "/tmp"});
        if let Some(s) = session {
            spec["session"] = serde_json::json!(s);
        }
        pane_specs.insert(i.to_string(), spec);
    }
    let fixture = serde_json::json!({
        "version": 1,
        "active_tab": 0,
        "tabs": [{"name": "main", "layout": {"pane": 1}, "panes": pane_specs}],
    });
    std::fs::write(dir.join("workspace.json"), fixture.to_string()).expect("seed workspace.json");
}

/// A control-verb run against a shared root, the way a user in a plain shell
/// does it: `ROOST_STATE` at the root, `-w` on the argv, no inherited
/// `ROOST_SOCK`/`ROOST_TOKEN`.
fn cli(root: &std::path::Path, args: &[&str]) -> Output {
    cli_env(root, args, &[])
}

fn cli_env(root: &std::path::Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_roost"));
    cmd.args(args).env("ROOST_STATE", root);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.env_remove("ROOST_SOCK").env_remove("ROOST_TOKEN").env_remove("ROOST_CONTROL_TOKEN");
    cmd.output().expect("run roost")
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Debug aid for auth failures: what is actually on disk under the root?
fn dump_tree(root: &std::path::Path) -> String {
    let mut out = String::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                let p = e.path();
                out.push_str(&format!("\n  {}", p.display()));
                if p.is_dir() {
                    stack.push(p);
                }
            }
        }
    }
    out
}

/// Block until `-w <name>`'s control socket AND its control token exist.
/// The socket binds slightly before the token is written; a client that
/// wins that race reads an empty token and gets "unauthorized" — a race,
/// not a verdict (this failed exactly that way under parallel load).
fn wait_for_workspace_socket(root: &std::path::Path, name: &str) {
    let dir = root.join("workspaces").join(name);
    let deadline = Instant::now() + WAIT;
    while !(dir.join("roost.sock").exists() && dir.join("control.token").exists()) {
        assert!(Instant::now() < deadline, "the workspace socket/token never appeared at {dir:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn spawn_named(
    root: &std::path::Path,
    name: &str,
    what: &str,
    envs: &[(&str, &str)],
) -> Option<harness::Harness> {
    match harness::Harness::try_spawn_at(root, &["-w", name], envs) {
        Ok(h) => Some(h),
        Err(reason) => {
            eprintln!("SKIP {what}: {reason}");
            None
        }
    }
}

// ---- 6.1: two instances, one root, both listed ---------------------------

/// `roost -w a` and `roost -w b` under one `ROOST_STATE` root both start,
/// each keeps its own layout state, and `ws ls` (offline) shows both running
/// with their own counts — plus the default workspace, idle and untouched.
#[test]
fn two_instances_under_one_root_both_start_and_ws_ls_shows_both() {
    let root = harness::shared_state_dir("wstwo");
    // A one-pane workspace and a two-pane one, so the counts are telling.
    seed_workspace(&root, "a", 1, "shell", None);
    seed_workspace(&root, "b", 2, "shell", None);

    let Some(mut ha) = spawn_named(&root, "a", "two-instance gate", &[]) else { return };
    let Some(mut hb) = spawn_named(&root, "b", "two-instance gate", &[]) else {
        let _ = ha.quit_and_wait(WAIT);
        return;
    };
    assert!(ha.settle(WAIT), "instance a never painted");
    assert!(hb.settle(WAIT), "instance b never settled");
    wait_for_workspace_socket(&root, "a");
    wait_for_workspace_socket(&root, "b");

    // Both sockets bound inside their own directories, both instances
    // saving their own state.
    assert!(root.join("workspaces/a/workspace.json").exists(), "a saves in its own directory");
    assert!(root.join("workspaces/b/workspace.json").exists(), "b saves in its own directory");

    // The offline listing sees both running, with counts, and the default
    // (never started, still listed).
    let o = cli(&root, &["ws", "ls"]);
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(&o));
    let text = out(&o);
    assert!(text.contains("a\trunning"), "a is running: {text:?}");
    assert!(text.contains("b\trunning"), "b is running: {text:?}");
    assert!(text.contains("default\tidle"), "the default workspace is listed idle: {text:?}");
    let a_row = text.lines().find(|l| l.starts_with("a\t")).expect("a's row");
    assert!(a_row.contains("panes=1"), "a's count: {a_row:?}");
    let b_row = text.lines().find(|l| l.starts_with("b\t")).expect("b's row");
    assert!(b_row.contains("panes=2"), "b's count: {b_row:?}");

    // ...and --json is a JSON array with the same fields, nothing else.
    let o = cli(&root, &["ws", "ls", "--json"]);
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(&o));
    let arr: serde_json::Value =
        serde_json::from_str(out(&o).trim()).expect("ws ls --json prints exactly one JSON value");
    let rows = arr.as_array().expect("a JSON array");
    let b = rows.iter().find(|r| r["name"] == "b").expect("b's row in the JSON");
    assert_eq!(b["running"], serde_json::json!(true));
    assert_eq!(b["panes"], serde_json::json!(2));
    assert_eq!(b["tabs"], serde_json::json!(1));
    assert!(
        b["adapters"].as_array().expect("adapters array").contains(&serde_json::json!("shell")),
        "adapters in use: {b}"
    );

    let _ = ha.quit_and_wait(WAIT);
    let _ = hb.quit_and_wait(WAIT);
    let _ = std::fs::remove_dir_all(&root);
}

// ---- 6.2: second instance refused ----------------------------------------

/// A second instance on a running workspace is refused: the message names
/// the workspace, points at `-w` and `ws ls`, and the exit is 1.
///
/// The refusal needs a PTY of its own: `main` checks tty before the lock, so
/// a piped second instance exits 2 (usage) without ever reaching the lock —
/// a bare-subprocess run would prove nothing about the refusal itself.
#[test]
fn a_second_instance_on_a_running_workspace_is_refused() {
    let root = harness::shared_state_dir("refuse");
    seed_workspace(&root, "a", 1, "shell", None);
    let Some(mut h) = spawn_named(&root, "a", "refusal gate", &[]) else { return };
    assert!(h.settle(WAIT), "instance a never settled");
    wait_for_workspace_socket(&root, "a");

    let Some(mut second) = spawn_named(&root, "a", "second-instance gate", &[]) else {
        let _ = h.quit_and_wait(WAIT);
        return;
    };
    let needle: &[u8] = b"already running in workspace";
    let said = second.wait_for_host_bytes(WAIT, |b| b.windows(needle.len()).any(|w| w == needle));
    assert!(
        said,
        "the refusal never appeared. raw: {:?}",
        String::from_utf8_lossy(&second.host_bytes())
    );
    assert_eq!(second.wait_for_exit_status(WAIT), Some(1), "the refusal exits 1");
    // (`second` stays bound — dropping it here would delete the SHARED root
    // (the harness's state_dir is the root, and Drop removes it) out from
    // under the still-running first instance. It drops at scope end, after
    // `h` has quit.)

    // The first instance is unharmed and still answers.
    let o = cli(&root, &["-w", "a", "list"]);
    assert!(o.status.success(), "the first instance still answers: {o:?}");

    let _ = h.quit_and_wait(WAIT);
    let _ = std::fs::remove_dir_all(&root);
}

// ---- 6.3: control targeting ----------------------------------------------

/// Out-of-pane targeting: `roost -w b list` reaches `b`; `roost list` with
/// only `b` running prints the unreachable error naming the tried workspace,
/// listing `b`, and suggesting `-w b`; `ROOST_WORKSPACE` targets too.
#[test]
fn control_verbs_target_the_flagged_workspace_and_name_what_is_running() {
    let root = harness::shared_state_dir("target");
    seed_workspace(&root, "b", 1, "shell", None);

    let Some(mut hb) = spawn_named(&root, "b", "targeting gate", &[]) else { return };
    assert!(hb.settle(WAIT), "instance b never settled");
    wait_for_workspace_socket(&root, "b");

    // The flag reaches b from a plain shell.
    let o = cli(&root, &["-w", "b", "list"]);
    if !o.status.success() {
        eprintln!("DEBUG tree:{}\nstderr={}", dump_tree(&root), err(&o));
    }
    assert!(o.status.success(), "-w b list must reach b: {o:?} stderr={}", err(&o));
    assert!(out(&o).trim_start().starts_with('['), "pane JSON: {:?}", out(&o));

    // The same verb with no flag tries the default workspace and is told
    // exactly where roost IS running.
    let o = cli(&root, &["list"]);
    assert_eq!(o.status.code(), Some(1), "{o:?}");
    let e = err(&o);
    assert!(e.contains("default"), "names the workspace it tried: {e:?}");
    assert!(e.contains('b'), "lists the running workspace: {e:?}");
    assert!(e.contains("-w b"), "suggests the flag: {e:?}");

    // The env half of the precedence chain.
    let o = cli_env(&root, &["list"], &[("ROOST_WORKSPACE", "b")]);
    assert!(o.status.success(), "ROOST_WORKSPACE=b list must reach b: {o:?}");

    let _ = hb.quit_and_wait(WAIT);
    let _ = std::fs::remove_dir_all(&root);
}

// ---- 6.4: cross-instance session claims ----------------------------------

/// The claim conflict, end to end: two workspaces hold a pane with the same
/// saved session id; the second instance to start shows the placeholder
/// naming the holder, keeps its saved session id, and resumes normally once
/// the first quits.
///
/// `HOME` is pointed at a fresh tree so the pi adapter finds no session
/// storage at all (`Unknown`, never a real-FS `Gone`) — the saved id then
/// reaches the claim path exactly as a real stored conversation would. The
/// pane's agent itself may or may not be installed on this machine; the
/// claim verdict is independent of that (the claim is taken before the
/// launch), and the assertions compare only against the claim placeholder,
/// never against a live agent pane.
#[test]
fn a_session_claimed_by_one_workspace_shows_a_placeholder_in_the_other() {
    let root = harness::shared_state_dir("claims");
    let home = harness::shared_state_dir("claimshome");
    let session = "roost-claim-e2e-test";
    seed_workspace(&root, "a", 1, "pi", Some(session));
    seed_workspace(&root, "b", 1, "pi", Some(session));

    let Some(mut ha) = spawn_named(
        &root,
        "a",
        "claim gate (holder)",
        &[("HOME", home.as_os_str().to_str().expect("utf8"))],
    ) else {
        return;
    };
    assert!(ha.settle(WAIT), "instance a never settled");

    let Some(mut firstb) = spawn_named(
        &root,
        "b",
        "claim gate (conflicted)",
        &[("HOME", home.as_os_str().to_str().expect("utf8"))],
    ) else {
        let _ = ha.quit_and_wait(WAIT);
        return;
    };
    assert!(firstb.settle(WAIT), "instance b never settled");
    let saw = firstb.wait_for(WAIT, |s| {
        // The bar is width-truncated (the pid suffix gets cut off on a
        // 120-col grid), so assert the two spec-mandated facts without the
        // closing quote: the session id, and the holder's name.
        let c = s.contents();
        c.contains(session) && c.contains("held by workspace 'a")
    });
    assert!(
        saw.is_some(),
        "b's claimed pane never showed the placeholder naming a; screen tail:\n{}\nroot tree:{}\nclaims: {}",
        firstb
            .screen()
            .contents()
            .lines()
            .rev()
            .take(8)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<String>(),
        dump_tree(&root),
        std::fs::read_dir(root.join("claims"))
            .map(|d| d
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(", "))
            .unwrap_or_else(|_| "none".into()),
    );

    // The saved session id survives for a later restart (spec: b's saved
    // layout still records the session).
    let saved = std::fs::read_to_string(root.join("workspaces/b/workspace.json"))
        .expect("read b's saved state");
    assert!(saved.contains(session), "b's workspace.json keeps the saved id: {saved:?}");

    // And a: its own pane resumed normally (it held the claim first).
    assert!(
        !ha.screen().contents().contains("held by workspace"),
        "a's own restore was never blocked by its own claim"
    );

    // Conflict clears: quit a, relaunch b, the pane resumes (no placeholder).
    let _ = ha.quit_and_wait(WAIT);
    let _ = firstb.quit_and_wait(WAIT);
    let Some(mut secondb) = spawn_named(
        &root,
        "b",
        "claim gate (cleared)",
        &[("HOME", home.as_os_str().to_str().expect("utf8"))],
    ) else {
        return;
    };
    assert!(secondb.settle(WAIT), "the relaunched b never settled");
    let cleared = secondb.wait_for(WAIT, |s| {
        let c = s.contents();
        c.contains("main") && !c.contains("held by workspace")
    });
    assert!(cleared.is_some(), "after the holder quits, b resumes without the placeholder");
    let _ = secondb.quit_and_wait(WAIT);
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&root);
}

// ---- 6.5: default regression ---------------------------------------------

/// With no flag and no `ROOST_WORKSPACE`, everything lands where it always
/// did: state and socket in `ROOST_STATE` itself, no `workspaces/` tree, and
/// `ws ls` shows exactly the default workspace running.
#[test]
fn the_default_workspace_writes_the_same_files_in_the_same_places() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let Some(mut h) = harness::spawn_or_skip("default regression", &harness::one_pane(cwd)) else {
        return;
    };
    assert!(h.settle(WAIT), "initial frame never settled");

    let state = h.state_dir();
    let deadline = Instant::now() + WAIT;
    while !state.join("roost.sock").exists() {
        assert!(Instant::now() < deadline, "the default socket never appeared");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(state.join("workspace.json").exists(), "state stays in the root");
    assert!(
        !state.join("workspaces").exists(),
        "the default workspace never creates a workspaces/ tree"
    );

    let o = cli(state, &["ws", "ls"]);
    assert_eq!(o.status.code(), Some(0));
    let text = out(&o);
    assert!(text.contains("default\trunning"), "the default is listed running: {text:?}");
    assert!(
        !text.lines().any(|l| l.starts_with("a\t") || l.starts_with("b\t")),
        "no named workspaces exist: {text:?}"
    );

    let _ = h.quit_and_wait(WAIT);
}

// ---- 6.6: identity signals + guarded verbs -------------------------------

/// A pane's environment carries `ROOST_WORKSPACE`, the named-workspace title
/// sequence reaches the host terminal, and `ws rm`/`ws mv` refuse while the
/// workspace is running and refuse `default` outright — but succeed on an
/// idle workspace.
#[test]
fn pane_env_title_and_guarded_ws_verbs() {
    let root: PathBuf = harness::shared_state_dir("identity");
    seed_workspace(&root, "b", 1, "shell", None);

    let Some(mut hb) = spawn_named(&root, "b", "identity gate", &[]) else { return };
    assert!(hb.settle(WAIT), "instance b never settled");
    wait_for_workspace_socket(&root, "b");

    // The pane environment carries ROOST_WORKSPACE — typed into the shell
    // pane, echoed back onto the screen.
    let o = cli(&root, &["-w", "b", "send", "1", "echo $ROOST_WORKSPACE", "--enter"]);
    assert!(o.status.success(), "send into b's pane: {o:?} stderr={}", err(&o));
    // contents() includes the pane borders, so strip them before matching
    // the echoed line.
    let echoed = hb
        .wait_for(WAIT, |s| s.contents().lines().any(|l| l.trim().trim_matches('│').trim() == "b"));
    let dbg_lines: Vec<String> = hb
        .screen()
        .contents()
        .lines()
        .map(|l| l.trim().trim_matches('│').trim().to_string())
        .collect();
    assert!(
        echoed.is_some(),
        "$ROOST_WORKSPACE=b never reached the pane; lines: {:?}",
        &dbg_lines[..8]
    );

    // The named-workspace title sequence reaches the host terminal.
    let needle: &[u8] = b"\x1b]2;roost \xc2\xb7 b \xc2\xb7";
    let titled = hb.wait_for_host_bytes(WAIT, |b| b.windows(needle.len()).any(|w| w == needle));
    assert!(titled, "the host title never carried the workspace segment");

    // rm/mv refuse while running...
    let o = cli(&root, &["ws", "rm", "b"]);
    assert_eq!(o.status.code(), Some(1), "rm while running: {o:?}");
    assert!(err(&o).contains("running"), "names the refusal: {:?}", err(&o));
    let o = cli(&root, &["ws", "mv", "b", "c"]);
    assert_eq!(o.status.code(), Some(1), "mv while running: {o:?}");

    // ...and refuse `default` regardless of running state.
    let o = cli(&root, &["ws", "rm", "default"]);
    assert_eq!(o.status.code(), Some(1), "rm default: {o:?}");
    let o = cli(&root, &["ws", "mv", "default", "x"]);
    assert_eq!(o.status.code(), Some(1), "mv default: {o:?}");

    // ...and renaming succeeds once the instance is gone.
    let _ = hb.quit_and_wait(WAIT);
    let o = cli(&root, &["ws", "mv", "b", "c"]);
    assert_eq!(o.status.code(), Some(0), "renaming an idle workspace: {}", err(&o));
    assert!(root.join("workspaces/c").is_dir(), "the directory was renamed");
    assert!(!root.join("workspaces/b").exists());
    let _ = std::fs::remove_dir_all(&root);
}
