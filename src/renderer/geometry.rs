//! Pure-CPU geometry helpers: atlas shelf-packer and GPU scissor-rect clamping.
//!
//! These types carry no GPU state and are unit-tested in `super::tests`. They
//! live here so `mod.rs` stays focused on the `Renderer` struct and its
//! submodule declarations.

use super::GLYPH_GAP;

// ---- Shelf packer -----------------------------------------------------------

/// Simple shelf packer for an atlas texture of side `size`.
pub(crate) struct Packer {
    pub(crate) size: u32,
    pub(crate) cursor_x: u32,
    pub(crate) cursor_y: u32,
    pub(crate) shelf_height: u32,
}

impl Packer {
    pub(crate) fn new(size: u32) -> Self {
        Self {
            size,
            cursor_x: 0,
            cursor_y: 0,
            shelf_height: 0,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.cursor_x = 0;
        self.cursor_y = 0;
        self.shelf_height = 0;
    }

    /// Reserve a `w`x`h` region. Returns its top-left origin, or `None` if the
    /// atlas is full (caller should clear the cache and retry).
    pub(crate) fn alloc(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        if w > self.size || h > self.size {
            return None;
        }
        // Wrap to a new shelf if the glyph doesn't fit the current row.
        if self.cursor_x + w > self.size {
            self.cursor_y += self.shelf_height + GLYPH_GAP;
            self.cursor_x = 0;
            self.shelf_height = 0;
        }
        if self.cursor_y + h > self.size {
            return None;
        }
        let origin = (self.cursor_x, self.cursor_y);
        self.cursor_x += w + GLYPH_GAP;
        self.shelf_height = self.shelf_height.max(h);
        Some(origin)
    }
}

// ---- Scissor rect -----------------------------------------------------------

/// A GPU scissor rectangle in surface pixels: an unsigned origin + extent,
/// already clamped to the surface so `set_scissor_rect` never rejects it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct ScissorRect {
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) w: u32,
    pub(crate) h: u32,
}

/// Clamp an integer pixel rect (which may be partly off-surface or have a
/// negative origin) to the `surface_w` x `surface_h` bounds, yielding a scissor
/// the GPU will accept. A rect fully outside the surface clamps to zero extent.
/// Pure geometry (no GPU state) so it is unit-tested directly.
pub(crate) fn clamp_scissor(
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    surface_w: u32,
    surface_h: u32,
) -> ScissorRect {
    // Left/top edges clamped to [0, surface]; right/bottom edges likewise, then
    // the extent is the (non-negative) difference.
    let x0 = x.max(0).min(surface_w as i32);
    let y0 = y.max(0).min(surface_h as i32);
    let x1 = (x + w.max(0)).max(0).min(surface_w as i32);
    let y1 = (y + h.max(0)).max(0).min(surface_h as i32);
    ScissorRect {
        x: x0 as u32,
        y: y0 as u32,
        w: (x1 - x0).max(0) as u32,
        h: (y1 - y0).max(0) as u32,
    }
}
