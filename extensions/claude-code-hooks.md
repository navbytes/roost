# Claude Code → roost status hooks

Claude Code panes get exact status the same way pi panes do — by pointing
Claude Code's hooks at roost's status socket.

**roost does this for you.** At startup it merges the three hooks below into
`~/.claude/settings.json`, the same way it installs the pi extension: only
when `~/.claude` already exists (it never creates the directory itself),
and only adding/updating its own three entries — the *values* already in
that file, your own hooks included, aren't touched. (The file does get
reformatted through roost's own JSON pretty-printer, so a hand-tab-indented
file will show a whitespace-only diff even though nothing semantic changed.)
Running roost again never appends duplicates: it recognizes its own entries
(and any hand-copied hook from an older version of this doc) and updates
them in place if this build changed, e.g. the binary moved. If
`~/.claude/settings.json` is a symlink — common for dotfiles-managed
configs — roost writes through it to the real file and leaves the link
itself alone.

Opt out with `ROOST_NO_EXT_INSTALL=1` (the same variable that disables the pi
extension install) and manage the hooks yourself — see below.

## What it writes

```json
{
  "hooks": {
    "PreToolUse": [
      { "hooks": [ { "type": "command", "command": "/path/to/roost __status working # roost-status-hook" } ] }
    ],
    "Stop": [
      { "hooks": [ { "type": "command", "command": "/path/to/roost __status waiting # roost-status-hook" } ] }
    ],
    "Notification": [
      { "hooks": [ { "type": "command", "command": "/path/to/roost __status needs_input # roost-status-hook" } ] }
    ]
  }
}
```

`/path/to/roost` is the absolute path roost resolved for its own binary at
install time (`std::env::current_exe()`), so it works regardless of `PATH`.
The trailing `# roost-status-hook` is a shell comment (inert — it's how a
later launch recognizes and updates its own entries rather than appending
duplicates); harmless to drop if you're copying this by hand.

`roost __status <status>` is an internal subcommand — it doesn't appear in
`roost --help`, it's plumbing a hook config invokes. It reads `$ROOST_PANE`,
`$ROOST_TOKEN` and `$ROOST_SOCK` from the hook's environment and writes the
same status line the socket has always expected, then exits. Outside roost
`$ROOST_PANE` is unset and it exits 0 instantly without touching the socket
— identical to the pi extension's no-op behavior.

This replaces the old `nc -U -q0 "$ROOST_SOCK"` command an earlier version of
this doc had you copy by hand: macOS's built-in `/usr/bin/nc` is BSD netcat
and doesn't support `-q` at all (that's `netcat-openbsd`, not installed by
default on macOS), so that command silently failed to report status out of
the box on roost's primary platform. `roost __status` needs no netcat/socat
and behaves identically on macOS and Linux.

## By hand

Prefer to manage this yourself? Set `ROOST_NO_EXT_INSTALL=1` and add the JSON
above to `~/.claude/settings.json`, pointing `command` at your `roost`
binary's absolute path (`which roost`, or wherever you installed it) followed
by ` __status working` / `__status waiting` / `__status needs_input` for the
three hooks respectively.

Session-id detection for Claude Code panes doesn't need a hook at all: roost's
filesystem fallback watches `~/.claude/projects/<encoded-cwd>/*.jsonl`.
