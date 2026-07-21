# Claude Code → roost status hooks

Claude Code panes get exact status the same way pi panes do — by pointing
Claude Code's hooks at roost's status socket. Add this to
`~/.claude/settings.json` (requires `nc` from netcat-openbsd, or swap in
`socat - UNIX-CONNECT:$ROOST_SOCK`):

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "[ -n \"$ROOST_PANE\" ] && printf '{\"pane\":\"%s\",\"token\":\"%s\",\"event\":\"status\",\"status\":\"working\"}\\n' \"$ROOST_PANE\" \"$ROOST_TOKEN\" | nc -U -q0 \"$ROOST_SOCK\" 2>/dev/null; true"
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "[ -n \"$ROOST_PANE\" ] && printf '{\"pane\":\"%s\",\"token\":\"%s\",\"event\":\"status\",\"status\":\"waiting\"}\\n' \"$ROOST_PANE\" \"$ROOST_TOKEN\" | nc -U -q0 \"$ROOST_SOCK\" 2>/dev/null; true"
          }
        ]
      }
    ],
    "Notification": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "[ -n \"$ROOST_PANE\" ] && printf '{\"pane\":\"%s\",\"token\":\"%s\",\"event\":\"status\",\"status\":\"needs_input\"}\\n' \"$ROOST_PANE\" \"$ROOST_TOKEN\" | nc -U -q0 \"$ROOST_SOCK\" 2>/dev/null; true"
          }
        ]
      }
    ]
  }
}
```

Outside roost, `$ROOST_PANE` is unset and every hook no-ops instantly —
identical to the pi extension's behavior. Session-id detection for Claude
Code panes doesn't need a hook at all: roost's filesystem fallback watches
`~/.claude/projects/<encoded-cwd>/*.jsonl`.
