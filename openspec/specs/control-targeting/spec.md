# control-targeting Specification

## Purpose

Defines how a roost control client run outside a pane chooses which running instance to talk to when several run at once, and what it reports when the chosen instance is not reachable.

## Requirements

### Requirement: Target resolution order
A control verb SHALL resolve its target instance in this order: an explicit `-w <name>` flag; the `ROOST_SOCK` environment variable set inside a pane; the `ROOST_WORKSPACE` environment variable; the default workspace. The `-w` flag SHALL be accepted before or after the verb.

#### Scenario: Explicit flag from a plain shell
- **WHEN** `roost -w b` is running and the user runs `roost -w b spawn --cwd ~/x claude` from a shell that is not inside any pane
- **THEN** the pane is spawned in workspace `b`

#### Scenario: Inside a pane without a flag
- **WHEN** the user runs `roost list` from inside a pane of workspace `a`
- **THEN** the verb targets workspace `a`

#### Scenario: Flag wins inside a pane
- **WHEN** the user runs `roost -w b list` from inside a pane of workspace `a`
- **THEN** the verb targets workspace `b`

#### Scenario: Environment variable from a plain shell
- **WHEN** the user runs `roost list` with `ROOST_WORKSPACE=b` from a shell that is not inside any pane
- **THEN** the verb targets workspace `b`

### Requirement: Unreachable target reporting
When the target instance is not running, the client SHALL print which workspace it tried, list the workspaces that are running, suggest `-w <name>`, and exit with status 1. When no workspace is running at all it SHALL say so.

#### Scenario: Wrong workspace
- **WHEN** only `roost -w a` is running and the user runs `roost list` from a plain shell
- **THEN** the client prints that no roost is running in the default workspace, that `a` is running, and suggests `-w a`, then exits with status 1

#### Scenario: Nothing running
- **WHEN** no roost is running and the user runs `roost list`
- **THEN** the client prints that no roost is running in any workspace and exits with status 1

### Requirement: Offline verbs need no instance
`roost keys` and every `roost ws` verb SHALL work without any running instance.

#### Scenario: Listing with nothing running
- **WHEN** no roost is running and the user runs `roost ws ls`
- **THEN** the list prints and the exit status is 0
