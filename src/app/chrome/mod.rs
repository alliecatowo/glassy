//! Chrome painting, split into focused submodules by concern:
//!
//!  * [`status_bar`] — the status-bar segment table + `App::status_bar_inputs`
//!    / `App::paint_status_bar`;
//!  * [`settings_form`] — `App::paint_settings` / `App::apply_settings_events`
//!    (the settings-overlay form paint + apply);
//!  * [`confirm`] — the confirm-close modal (`App::paint_confirm_close`) and
//!    the inline tab-rename editor (`App::paint_tab_rename`), both small
//!    standalone overlay painters with no supporting data tables.
//!
//! Tab-bar and chip painting are in `tab_paint.rs` (a sibling of `app::chrome`,
//! not a submodule here — it predates this split).
//!
//! Split out of the former single flat `chrome.rs` (~1480 lines covering all
//! four concerns at once) so each file stays focused on one paint/apply
//! surface; behaviour is identical — every call site (`render.rs`,
//! `multipane.rs`) still calls these as `App` methods unchanged, since Rust
//! merges `impl App` blocks across files.

// Re-export `super::*` to every submodule via this parent so each `use
// super::*` (one level down) resolves the same `app` symbols the original
// flat `chrome.rs` relied on — mirrors the `app::mouse` split's convention.
use super::*;

mod confirm;
mod settings_form;
mod status_bar;

// `ConfirmCloseResult` is named explicitly at a couple of call sites
// (`render.rs`'s `use chrome::ConfirmCloseResult::*;`), so re-export it at
// this module's top level to keep that path working post-split.
pub(crate) use confirm::ConfirmCloseResult;
