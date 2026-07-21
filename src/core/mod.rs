//! The domain core: layout tree, workspace state, status model, app
//! orchestration, and the event vocabulary. Depends only on `ports` traits
//! and `agents` (domain adapters) — never on PTYs, sockets, or the fs.

pub mod app;
pub mod control;
pub mod event;
pub mod layout;
pub mod status;
pub mod workspace;
