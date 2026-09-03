## Purpose

Lets a user run several roost instances at once without a daemon: a workspace is a named, isolated layout-and-session state that exactly one running instance owns at a time, selected by name instead of by a raw state directory.

## ADDED Requirements

### Requirement: Workspace selection
The system SHALL open the workspace named by the `-w <name>` or `--workspace <name>` flag; when no flag is given, the workspace named by the `ROOST_WORKSPACE` environment variable; and when neither is set, the default workspace. The flag SHALL take precedence over the environment variable. The name `default` SHALL denote the default workspace wherever a name is accepted.

#### Scenario: Flag selects a workspace
- **WHEN** the user runs `roost -w tripto`
- **THEN** the instance opens the workspace named `tripto`

#### Scenario: Environment variable selects a workspace
- **WHEN** the user runs `roost` with `ROOST_WORKSPACE=tripto` in the environment
- **THEN** the instance opens the workspace named `tripto`

#### Scenario: Flag beats environment variable
- **WHEN** the user runs `roost -w work` with `ROOST_WORKSPACE=tripto` in the environment
- **THEN** the instance opens `work`

#### Scenario: No selection is today's behaviour
- **WHEN** the user runs `roost` with no flag and no `ROOST_WORKSPACE`
- **THEN** the instance opens the default workspace from the same files it used before this change

### Requirement: Workspace names
A workspace name MUST be 1 to 32 characters, start with a lowercase letter or digit, and contain only lowercase letters, digits, `.`, `_` and `-`. The system SHALL reject any other name before touching any state, print the reason, and exit with status 2. Uppercase letters SHALL be rejected with a hint showing the lowercase form, so names never collide on case-insensitive filesystems.

#### Scenario: Invalid character
- **WHEN** the user runs `roost -w "my ws"`
- **THEN** roost prints that the name is invalid and which characters are allowed, creates nothing, and exits with status 2

#### Scenario: Uppercase name
- **WHEN** the user runs `roost -w Tripto`
- **THEN** roost rejects the name, suggests `tripto`, and exits with status 2

#### Scenario: Name too long
- **WHEN** the user runs `roost -w` with a 33-character name
- **THEN** roost rejects the name, states the limit, and exits with status 2

### Requirement: Implicit creation
Opening a valid name that does not exist yet SHALL create that workspace with the same first-run state as a fresh roost, without a confirmation step.

#### Scenario: First use creates the workspace
- **WHEN** the user runs `roost -w scratch` and no workspace named `scratch` exists
- **THEN** roost starts with a fresh first-run layout, and `roost ws ls` afterwards lists `scratch`

### Requirement: Isolation and storage
Each named workspace SHALL keep its layout state, instance lock, control socket, control token, control log and performance log separate from every other workspace and from the default workspace, in a directory named after the workspace under the state root. The default workspace's files SHALL stay exactly where they are today, with no migration. When `ROOST_STATE` is set, named workspaces SHALL live under that directory, so the two mechanisms compose.

#### Scenario: Two named workspaces run concurrently
- **WHEN** `roost -w a` and `roost -w b` run at the same time
- **THEN** both start, each saves its own layout, and neither can read or corrupt the other's state

#### Scenario: Default and named run concurrently
- **WHEN** `roost` and `roost -w b` run at the same time
- **THEN** both start, and the default workspace's files are the same files roost used before this change

#### Scenario: Root override composes
- **WHEN** the user runs `ROOST_STATE=/tmp/r roost -w a`
- **THEN** the workspace `a` is stored under `/tmp/r`, and `ROOST_STATE=/tmp/r roost ws ls` lists it

### Requirement: Single instance per workspace
A second instance started on a workspace that is already running SHALL refuse to start with a message that names the workspace, tells the user to open another workspace with `-w <name>`, and mentions `roost ws ls`, and SHALL exit with status 1.

#### Scenario: Second instance on the same named workspace
- **WHEN** `roost -w a` is running and the user runs `roost -w a` again
- **THEN** the second process prints that roost is already running in workspace `a`, points at `-w` and `ws ls`, and exits with status 1

#### Scenario: Second instance on the default workspace
- **WHEN** `roost` is running and the user runs `roost` again
- **THEN** the second process prints that roost is already running in the default workspace, points at `-w` and `ws ls`, and exits with status 1

### Requirement: Shared keybinding config
A named workspace SHALL read keybinding configuration from the root `config.json`. When a `config.json` exists inside the workspace's own directory, it SHALL replace the root file wholesale for that workspace. The default workspace and plain `ROOST_STATE` isolation SHALL keep reading `config.json` from their own directory as today.

#### Scenario: Named workspace uses root config
- **WHEN** the root `config.json` rebinds a key and the user runs `roost -w a` with no `config.json` in the workspace directory
- **THEN** the rebinding is active in workspace `a`

#### Scenario: Workspace-local config overrides
- **WHEN** both the root and the workspace directory contain a `config.json`
- **THEN** only the workspace directory's file is applied in that workspace

### Requirement: Workspace listing
`roost ws ls` (and `roost ws` with no verb) SHALL list the default workspace and every named workspace with: name, whether an instance is running on it, tab count, pane count, the adapters in use, and the last-saved time. It SHALL work with no instance running, SHALL exit with status 0 when the list is empty, and SHALL print a JSON array with the same fields when `--json` is given.

#### Scenario: Mixed running and idle
- **WHEN** `roost -w a` is running, `b` exists but is not running, and the user runs `roost ws ls` from a plain shell
- **THEN** the output shows `default`, `a` marked running, and `b` marked idle, each with counts and last-saved time

#### Scenario: Machine-readable listing
- **WHEN** the user runs `roost ws ls --json`
- **THEN** stdout is a JSON array with one object per workspace carrying the same fields, and nothing else

### Requirement: Rename and delete
`roost ws mv <old> <new>` SHALL rename an idle workspace, and `roost ws rm <name>` SHALL delete an idle workspace's directory and everything in it. Both SHALL refuse with a message and exit status 1 while the workspace is running, and SHALL refuse for `default`. Deleting a workspace SHALL NOT touch any agent's own session files.

#### Scenario: Rename while idle
- **WHEN** no instance is running on `a` and the user runs `roost ws mv a tripto`
- **THEN** `roost ws ls` lists `tripto` with `a`'s tabs and panes, and `a` is gone

#### Scenario: Delete while running is refused
- **WHEN** `roost -w a` is running and the user runs `roost ws rm a`
- **THEN** roost prints that `a` is running, deletes nothing, and exits with status 1

#### Scenario: Default is protected
- **WHEN** the user runs `roost ws rm default` or `roost ws mv default x`
- **THEN** roost refuses and exits with status 1

### Requirement: Identity signals
Every pane SHALL receive the environment variable `ROOST_WORKSPACE` set to its workspace name, `default` for the default workspace. A named workspace SHALL set the terminal title to `roost · {workspace} · {focused pane name}` and reset it to `roost` on exit; the default workspace SHALL keep today's title. `roost status` SHALL include the workspace name. Desktop notifications raised from a named workspace SHALL include the workspace name. The on-screen chrome SHALL NOT change for either kind of workspace.

#### Scenario: Pane environment
- **WHEN** a pane is spawned in workspace `a`
- **THEN** a process in that pane sees `ROOST_WORKSPACE=a`

#### Scenario: Title in a named workspace
- **WHEN** workspace `a` is running with a pane named `claude` focused
- **THEN** the terminal title is `roost · a · claude`

#### Scenario: Default title unchanged
- **WHEN** the default workspace is running with a pane named `claude` focused
- **THEN** the terminal title is `roost · claude`
