//! Events flowing into the main loop from PTY reader threads (and, in M3,
//! from the status socket listener).

use crate::workspace::PaneId;

pub enum AppEvent {
    /// Raw bytes from a pane's PTY.
    Output(PaneId, Vec<u8>),
    /// The pane's child process exited (EOF on the PTY).
    Exit(PaneId),
}
