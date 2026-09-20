//! HD2 Armor Desk library surface.
//!
//! The binary is a thin `main` over this crate. Exposing the UI layer as a
//! library is what lets the integration tests build the real
//! [`ui::WorkspaceView`] and dispatch real clicks into it; without it, the
//! window interactions would only ever be exercised by hand.

pub mod single_instance;
pub mod ui;
pub mod update;

pub use ui::{state, workspace};
