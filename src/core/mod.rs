//! The domain core: layout tree, workspace state, status model, app
//! orchestration, and the event vocabulary. Depends only on `ports` traits
//! and `agents` (domain adapters) — never on PTYs, sockets, or the fs.

pub mod app;
pub mod control;
pub mod detect;
pub mod event;
pub mod layout;
#[cfg(test)]
mod layout_props;
pub mod overlay;
pub mod session_resolver;
pub mod status;
pub mod textfield;
pub mod workspace;
