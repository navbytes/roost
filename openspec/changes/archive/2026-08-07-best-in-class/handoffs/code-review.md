# Codebase correctness sweep (reviewer, 2026-08-06) — verdict: request changes

Clippy clean (3 style lints). The panic hunt came up nearly empty: byte_at,
find_url_at, extract_selection, float_rect, feed_window, inner_cell,
resize_pane's i-1 and the C6 stack geometry are all correctly guarded; a live
instance was stress-driven through 13 terminal sizes down to 1×1 with splits
and a stack open without a panic. The real defects are in the socket protocol
and in error swallowing.

## P0 — control plane wedges permanently (CONFIRMED, reproduced)

`src/infra/sock.rs:176-202` + `src/core/control.rs:110-114`. `parse_control`
does `serde_json::from_value::<Request>(v).ok()?` — any deser failure returns
None, falls through to `parse_line` (also None: no pane/event), lands in
`log_dropped`, and **nothing is written back**. Connection stays open;
`cli.rs:222`'s `read_line` has no timeout.

Measured against a live instance:

    valid list        -> REPLY {"ok":[...
    missing token     -> *** NO REPLY, CLIENT HANGS ***
    unknown method    -> *** NO REPLY, CLIENT HANGS ***
    send missing text -> *** NO REPLY, CLIENT HANGS ***
    bad pane type     -> *** NO REPLY, CLIENT HANGS ***
    bad token (authz) -> REPLY {"err":"unauthorized..."}

64 such lines (MAX_CONN) park 64 threads in `read_until` forever; the next
legitimate client is shed at sock.rs:148 with BrokenPipe. Control plane dead
until restart. Trigger is mundane, not adversarial — the documented interface
is "an LLM writes JSON to $ROOST_SOCK", and an agent retrying a typo'd verb
reaches 64 in seconds. `{"method":"list"}` with no token is enough.

Fix: when a line parses as JSON *and* has a `method` key but from_value
fails, reply `{"err":"<serde error>"}` instead of falling through — e.g.
`parse_control` returns `Option<Result<Request,String>>`. Independently set
`stream.set_read_timeout(Some(30s))` so no client holds a slot indefinitely,
and log the shed-load drop at sock.rs:149.

## P1-1 — a resize can kill the entire fleet (CONFIRMED, reproduced)

`src/main.rs:346` `terminal.clear()?`. ratatui-core **0.1.2** changed
`Terminal::clear()` (buffers.rs:148) to snapshot the cursor:
`get_cursor_position()` → `crossterm::cursor::position()` → writes `ESC[6n`
and blocks up to **2 s** reading stdin. 0.1.0 had no such query; Cargo.toml
pins `ratatui = "0.30"`, so this arrived via a patch bump with no roost
change. Measured: 0 DSR queries over 5 s idle, exactly 1 per resize after.
On a host that doesn't answer:

    *** roost DIED at 1x200, exit=1
    Error: The cursor position could not be read within a normal duration

`run()` → Err → `app.shutdown()` (app.rs:4422) SIGHUPs then SIGKILLs every
pane. A cosmetic clear tears down the fleet. Non-answering hosts are real:
nested multiplexers that swallow DSR, some CI/serial terminals, a dropped ssh
hop mid-resize. Fix: `let _ = terminal.clear();` and treat draw()/size() the
same — count consecutive failures, bail only after N.

## P1-2 — socket listener failure swallowed (CONFIRMED, hit accidentally)

`src/main.rs:238` `spawn_listener(...).ok()` discards three distinct
failures: create_dir_all, the `dir_is_private_and_ours` bail (a *security*
refusal), and bind. roost then runs looking normal with **no control plane**:
no $ROOST_SOCK in panes, so the pi extension / Claude hooks silently never
report status, control.log is never written (audit gap), every `roost <verb>`
fails. Reproduced with a 112-char state dir (macOS sun_path limit is 104).
Fix: match and `set_flash` the error (the mechanism already exists two lines
below for ensure_pi_extension); keep `dir_is_private_and_ours` fatal — a
security refusal that degrades to "silently no socket" is the wrong default.

## P2-3 — OSC 52 clipboard relay is size-capped but not rate-capped (CONFIRMED)

`src/infra/pty.rs:342-348`: `Osc52Write` goes straight to host_writes with no
throttle, unlike `queue_host_notify` (pty.rs:365) which enforces
HOST_NOTIFY_INTERVAL. OSC52_PAYLOAD_CAP bounds one write, not the rate.
Measured from one shell loop: 5000 relayed sequences, 129812 host stdout
bytes — 5000 writes to the user's real clipboard. At the 100 KB cap that's
~500 MB of relay. tmux rate-limits this path for exactly this reason. Fix:
give Osc52Write the same per-pane interval gate OSC 9 already has.

## P3 — latent / hardening

4. `workspace.rs:68,72` `self.tabs.len() - 1` underflows if tabs is empty.
   Unreachable today (validate_and_repair + close_pane_id guard), but
   `active_tab()` is pub, called from ~40 sites; `self.tabs.get(..)
   .or(self.tabs.first())` removes the class. PLAUSIBLE as a live bug.
5. `main.rs:188` panic hook calls `ratatui::restore()` from *any* thread — a
   PTY-reader panic restores the terminal while main keeps drawing into a
   non-raw, non-alt-screen terminal.
6. `pty.rs:595` `kill(-(pid as pid_t), SIGKILL)`: if process_id() ever
   returned Some(0), `-0 == 0` signals roost's own group. Not reachable via
   portable-pty today; `if pid > 1` costs one line.

## What's good (specific, and why the panic hunt came up empty)

byte_at/char-boundary handling in the rename field; float_rect written
without .clamp() to avoid panic-on-min>max; extract_selection's
wide-continuation snap-back; the `alive: Arc<AtomicBool>` guard against stale
Exit events on recycled pane ids; try_wait-with-cap instead of a blocking
reap on the UI thread; the bounded event channel as real PTY backpressure;
MAX_EVENTS_PER_TICK against firehose starvation; atomic 0600 save with
corrupt-file salvage; LOW-3 re-authorization of waiters at fire time. The
`ponytail:` ceiling comments are doing their job.

## Not verified

Linux-only `session_members` /proc parsing (macOS box ran the pgrep branch);
FD/thread accounting for a pane whose descendant holds the slave PTY past the
session sweep; `resolve_actor`'s non-constant-time token compare (behind a
0600 socket, judged not worth reporting).
