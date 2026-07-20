//! Infrastructure adapters — the production implementations of `ports`
//! traits plus the status socket listener. All real I/O lives here.

pub mod inspect;
pub mod notify;
pub mod pty;
pub mod sock;
pub mod store;
