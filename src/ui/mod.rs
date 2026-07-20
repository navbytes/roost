//! UI layer: rendering (ratatui), key translation, and mouse routing.
//! Depends on the core through `App<B>`'s public surface only.

pub mod input;
pub mod mouse;
pub mod render;
