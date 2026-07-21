//! Infrastructure adapters — the production implementations of `ports`
//! traits plus the status socket listener. All real I/O lives here.

pub mod clipboard;
pub mod extension;
pub mod inspect;
pub mod kitty;
pub mod notify;
pub mod open;
pub mod pty;
pub mod sock;
pub mod store;
