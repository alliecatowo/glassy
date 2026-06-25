//! Mouse input handlers, split into focused submodules:
//!
//!  * this file — cursor-motion dispatch (`handle_cursor_moved`) and the
//!    shared `use super::*` surface for the submodules below;
//!  * [`button`] — button press/release (`handle_mouse_input`): overlay
//!    routing, link open, context menus, and the glassy text-selection /
//!    paste path (incl. Alt-drag rectangular selection);
//!  * [`wheel`] — the scroll wheel / touchpad (`handle_mouse_wheel`):
//!    help-panel scroll, tab-strip swipe, and content scroll / report.
//!
//! Split out of the former single `mouse.rs` (>700 lines) to keep each file
//! under the size budget; behaviour is identical — the dispatcher in
//! `event_loop.rs` still calls these `App` methods unchanged.

// Re-export `super::*` to every submodule via this parent so each `use
// super::*` (one level down) resolves the same `app` symbols the original
// flat `mouse.rs` relied on.
use super::*;

mod button;
mod wheel;

impl App {
    // -------------------------------------------------------------------------
    // Cursor motion
    // -------------------------------------------------------------------------

    /// Handle a `WindowEvent::CursorMoved` event.
    pub(in crate::app) fn handle_cursor_moved(
        &mut self,
        position: winit::dpi::PhysicalPosition<f64>,
        event_loop: &ActiveEventLoop,
    ) {
        self.mouse_px = (position.x, position.y);
        // Any open GUI overlay (settings, dropdown/context menu, help panel,
        // pane ⋮ menu) owns the pointer: its immediate-mode widgets compute
        // hover / press / slider-drag from `mouse_px` during paint, so every
        // motion must trigger a repaint for those highlights to track the
        // pointer. It also means motion must NOT fall through to drive
        // tab-drag, gutter-drag, terminal hover, or text selection beneath
        // the overlay. Mirror the settings treatment for all of them.
        // The dropdown / context menu (`gui::menu`) highlights the row under
        // the pointer. Mirror the hovered row into `menu_sel` so mouse hover
        // and keyboard nav share one selection, and repaint only when that
        // row actually changes — not every pixel of motion across the panel.
        if self.menu_open && !self.settings_open && !self.help_open {
            if let Some(action) = self.menu_hit_test(position.x, position.y) {
                let items: &[MenuAction] = self.menu_items.as_deref().unwrap_or(MenuAction::ALL);
                if let Some(idx) = items.iter().position(|&a| a == action)
                    && idx != self.menu_sel
                {
                    self.menu_sel = idx;
                    self.mark_dirty(event_loop);
                }
            }
            return;
        }
        // Tab right-click menu: mirror the hovered row into `tab_menu_sel` so
        // mouse hover and keyboard nav share one selection (repaint on change).
        if self.tab_menu_target.is_some() {
            if let Some(action) = self.tab_menu_hit_test(position.x, position.y)
                && let Some(idx) = self.tab_menu_actions().iter().position(|&a| a == action)
                && idx != self.tab_menu_sel
            {
                self.tab_menu_sel = idx;
                self.mark_dirty(event_loop);
            }
            return;
        }
        if self.settings_open
            || self.help_open
            || self.pane_menu_open.is_some()
            || self.palette.is_some()
            || self.search.is_some()
        {
            self.mark_dirty(event_loop);
            return;
        }
        let cell = self.px_to_cell(position.x, position.y);
        let moved = cell != self.mouse_cell;
        self.mouse_cell = cell;

        // Drag-to-reorder a tab: while a tab chip is held, move it under
        // the pointer's pixel position and lift it as a drag-ghost. Takes
        // priority over selection/hover; repaint on any motion so the ghost
        // tracks the pointer.
        if self.dragging_tab.is_some() {
            let _ = self.drag_tab_to(position.x as f32, position.y as f32);
            self.force_full_redraw = true;
            self.mark_dirty(event_loop);
            return;
        }

        // Dragging a pane resize gutter: re-tile under the pointer. Takes
        // priority over hover/selection; repaint so the divider + content
        // follow. The OS resize cursor stays set for the drag's duration.
        if self.dragging_gutter.is_some() {
            if self.drag_gutter_to(position.x, position.y) {
                self.mark_dirty(event_loop);
            }
            return;
        }

        // Gutter hover: over a split's divider band, switch the OS cursor to
        // a resize arrow and draw the divider transiently fat/bright. Only
        // costs a hit-test on motion; off any gutter restores the default.
        {
            let new_gutter = self.gutter_at(position.x, position.y);
            if new_gutter != self.hovered_gutter {
                self.apply_gutter_cursor(new_gutter.as_ref());
                self.hovered_gutter = new_gutter;
                self.mark_dirty(event_loop);
            }
            // Over a gutter, suppress tab-bar/selection hover handling below.
            if self.hovered_gutter.is_some() {
                return;
            }
        }

        // Pane header hover: repaint only on an enter/leave or ⋮-button
        // edge, not on every pixel of motion — otherwise dragging the
        // pointer across a header queues a frame per event for no visual
        // change. Track the hovered header and diff it.
        if self.is_split() {
            let new_hover = self.pane_header_at(position.x, position.y);
            if new_hover != self.hovered_pane_header {
                self.hovered_pane_header = new_hover;
                self.mark_dirty(event_loop);
            }
        } else if self.hovered_pane_header.is_some() {
            self.hovered_pane_header = None;
        }

        // Tab-bar hover highlighting: track the item under the pointer (only
        // while over the bar's pixel band), repaint when it changes.
        {
            // 0 when the strip is hidden, so the top band routes to the terminal.
            let bar_h = self.effective_tab_bar_h() as f64;
            let new_hover = if position.y < bar_h {
                self.strip_item_at_px(position.x as f32, position.y as f32)
            } else {
                None
            };
            if new_hover != self.hovered_strip_item {
                self.hovered_strip_item = new_hover;
                self.mark_dirty(event_loop);
            }
        }

        // Extend an in-progress glassy text selection while dragging.
        if self.selecting {
            self.update_selection();
            self.mark_dirty(event_loop);
        } else if moved {
            // Motion reports drive hover highlighting (e.g. the Claude
            // Code TUI highlights the element under the pointer, which
            // needs any-motion mode 1003 with no button held).
            let mode = self.term_mode();
            if let Some(button) = motion_button(mode, self.held_button) {
                self.report_mouse(button, true, true, mode);
            } else if !mode.intersects(TermMode::MOUSE_MODE) {
                // Track the hovered link so it can be underlined and
                // Ctrl+clicked.  OSC 8 links take priority; for cells
                // with no OSC 8 annotation we fall back to the
                // plain-text URL/path scanner.
                let (c, r) = self.mouse_cell;
                let link = self
                    .cell_hyperlink(c, r)
                    .or_else(|| self.plain_link_at(c, r));
                if link != self.hovered_link {
                    self.hovered_link = link;
                    self.mark_dirty(event_loop);
                }
            }
        }
    }
}
