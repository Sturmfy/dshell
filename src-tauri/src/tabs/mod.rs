//! Tab model: descriptors, per-tab runtime state, geometry, and the manager.
//!
//! Only [`manager`] is allowed to touch multi-webview APIs; everything else in
//! this module is plain data.

pub mod layout;
pub mod manager;
pub mod state;
