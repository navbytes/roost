## Purpose

Guarantees that one agent conversation is driven by at most one roost pane across all concurrently running instances, so two windows never resume or adopt the same session.

## ADDED Requirements

### Requirement: Exclusive claim per session
A running instance SHALL hold an exclusive claim on every agent session id assigned to one of its panes, taken when a pane is restored with a saved session and when a session is detected for a new pane, and released when the pane closes or the instance exits. A claim SHALL be released automatically when the holding process dies for any reason, so no stale claim can outlive its owner.

#### Scenario: Claim released on crash
- **WHEN** an instance holding a claim on session `S` is killed with SIGKILL
- **THEN** another instance can restore a pane with session `S` immediately afterwards

### Requirement: Restore conflict
When a pane's saved session is claimed by another running instance at restore time, the pane SHALL NOT launch its agent. It SHALL show a placeholder stating the session id and the workspace that holds it, and SHALL keep its saved session id so that a later restart resumes normally once the claim is free.

#### Scenario: Same session saved in two workspaces
- **WHEN** workspaces `a` and `b` both hold a pane with session `S`, `a` is running, and the user runs `roost -w b`
- **THEN** `b` starts, its `S` pane shows a placeholder naming workspace `a`, and `b`'s saved layout still records `S` for that pane

#### Scenario: Conflict clears
- **WHEN** the situation above holds, the user quits workspace `a`, and later relaunches `roost -w b`
- **THEN** the `S` pane in `b` resumes its agent normally

### Requirement: Detection conflict
Session detection for a newly spawned pane SHALL never adopt a session id that another running instance has claimed.

#### Scenario: Concurrent spawn in one directory
- **WHEN** workspaces `a` and `b` each spawn the same agent in the same directory at the same time
- **THEN** each pane ends up with a different session id, and neither instance shows the other's session

### Requirement: Single instance unaffected
With only one instance running, claims SHALL never block a restore or a detection.

#### Scenario: Ordinary restart
- **WHEN** a single instance quits and is relaunched
- **THEN** every pane resumes exactly as it did before this change

### Requirement: Claims are private to the user
Claim state SHALL live under the owner-only state root, so no other user on the machine can read, take or release a claim.

#### Scenario: Another user
- **WHEN** a different user on the machine attempts to open the claim state
- **THEN** the operating system denies access
