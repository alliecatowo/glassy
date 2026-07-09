//! glassy's library crate: exposes the same module tree `src/main.rs` used to
//! declare privately, so the `glassy` binary (a thin wrapper around this crate,
//! see `src/main.rs`) and out-of-crate targets — `benches/hot_paths.rs`,
//! integration tests — can reach glassy's internals.
//!
//! Module *paths* below are identical to the old bin-only `mod` list in
//! `main.rs` (same names, same relative file layout), so nothing inside any of
//! these modules had to change: every `crate::foo::bar` reference resolves the
//! same way whether `crate` is this lib or (previously) the bin.
//!
//! Only `pub mod` here is new API surface at the *crate-root* level; visibility
//! *within* the module tree (pub vs pub(crate) vs private) is unchanged except
//! where a handful of hot-path benchmark targets were deliberately widened —
//! see `benches/hot_paths.rs` and the `#[doc(hidden)]` re-exports it needs.
pub mod app;
pub mod bell;
pub mod color;
pub mod config;
pub mod gui;
pub mod image;
pub mod input;
pub mod ipc;
pub mod pane;
pub mod pty;
pub mod renderer;
pub mod session;
pub mod text;
