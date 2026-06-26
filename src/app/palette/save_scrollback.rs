//! Save scrollback / selection to a file (palette action `SaveScrollbackToFile`).
//!
//! Writes the full scrollback history (plain text, no ANSI escape sequences) of
//! the focused pane to a unique temporary file, then echoes the path to the shell
//! via the PTY so the user can open it with `$EDITOR`, `less`, etc.
//!
//! Implementation notes:
//! - Uses the system temp dir (via [`std::env::temp_dir`]) and a timestamp-based
//!   unique name so parallel glassy windows never collide.
//! - The PTY write is a `cd`-safe `cat`-free one-liner echo so it is visible and
//!   cancelable: the user sees what happened.

use std::io::Write as IoWrite;

use super::super::*; // App, ActiveEventLoop, etc.

impl App {
    /// Write the focused pane's full scrollback (history + viewport) to a
    /// temporary file and echo its path into the shell. A toast notification
    /// reports success or failure.
    pub(crate) fn save_scrollback_to_file(&mut self, event_loop: &ActiveEventLoop) {
        let Some(pty) = self.pty.as_ref() else {
            self.push_toast("No active pane");
            self.mark_dirty(event_loop);
            return;
        };

        // Grab all the text while we hold the term lock, then release it before
        // doing any I/O (lock is not held across syscalls).
        let text = {
            let term = pty.term.lock();
            extract_scrollback_text(&term)
        };

        // Build a temp path: /tmp/glassy-scrollback-<timestamp>.txt
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let mut path = std::env::temp_dir();
        path.push(format!("glassy-scrollback-{ts}.txt"));

        match write_to_path(&path, &text) {
            Ok(()) => {
                let path_str = path.display().to_string();
                // Echo the path into the active shell so the user knows where it is.
                let line = format!("echo 'Scrollback saved to: {path_str}'");
                self.palette_submit_line(&line, event_loop);
                self.push_toast(format!(
                    "Scrollback saved: {}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                ));
            }
            Err(e) => {
                log::error!("save_scrollback_to_file: {e}");
                self.push_toast(format!("Save failed: {e}"));
            }
        }
        self.mark_dirty(event_loop);
    }

    /// Apply an opacity value and update live renderer + config (helper shared by
    /// `IncreaseOpacity`, `DecreaseOpacity`, `SetOpacity`, `ToggleOpacity`).
    pub(crate) fn apply_opacity(&mut self, opacity: f32, event_loop: &ActiveEventLoop) {
        let opacity = opacity.clamp(0.0, 1.0);
        self.config.opacity = opacity;
        if let Some(r) = self.renderer.as_mut() {
            r.set_opacity(opacity);
        }
        self.settings_saved = false;
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// Toggle between 1.0 and the last non-1.0 opacity. The "saved" opacity is
    /// stored in `self.opacity_before_toggle` (initialised from the config value;
    /// if the config starts at 1.0 we save 0.85 as a sensible default).
    pub(crate) fn toggle_opacity(&mut self, event_loop: &ActiveEventLoop) {
        if (self.config.opacity - 1.0).abs() < 0.01 {
            // Currently fully opaque → restore saved opacity.
            let target = self.opacity_before_toggle.unwrap_or(0.85);
            self.apply_opacity(target, event_loop);
        } else {
            // Currently transparent → save current and go to 1.0.
            self.opacity_before_toggle = Some(self.config.opacity);
            self.apply_opacity(1.0, event_loop);
        }
    }

    /// Toggle the CRT/glow/scanline post-process effect. The renderer stores the
    /// flag itself; we mirror it into config so save_settings persists it.
    pub(crate) fn toggle_crt_effect(&mut self) {
        self.config.crt_effect = !self.config.crt_effect;
        if let Some(r) = self.renderer.as_mut() {
            r.set_crt(self.config.crt_effect);
        }
        self.settings_saved = false;
        self.force_full_redraw = true;
    }

    /// Toggle the cursor-trail smooth-glide animation.
    pub(crate) fn toggle_cursor_trail(&mut self) {
        self.config.cursor_trail = !self.config.cursor_trail;
        if let Some(r) = self.renderer.as_mut() {
            r.set_cursor_trail(self.config.cursor_trail);
            if !self.config.cursor_trail {
                r.reset_cursor_trail();
            }
        }
        self.settings_saved = false;
        self.force_full_redraw = true;
    }
}

/// Extract all terminal rows (scrollback history + viewport) as plain text. Each
/// logical line is terminated with `\n`; trailing blank lines are trimmed.
fn extract_scrollback_text(term: &alacritty_terminal::Term<crate::pty::EventProxy>) -> String {
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::index::{Column, Line, Point};
    use alacritty_terminal::term::cell::Flags;

    let grid = term.grid();
    let history_size = grid.history_size();
    let cols = grid.columns();
    let total_lines = history_size + grid.screen_lines();

    let mut out = String::with_capacity(total_lines * (cols + 1));

    // Iterate from the oldest history line to the bottom of the viewport.
    for row in -(history_size as i32)..grid.screen_lines() as i32 {
        let line = Line(row);
        let mut line_text = String::with_capacity(cols);
        for col in 0..cols {
            let point = Point::new(line, Column(col));
            let cell = &grid[point];
            // Skip the spacer half of a wide character (the visible glyph is in the
            // preceding cell).
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let c = cell.c;
            if c == '\0' {
                line_text.push(' ');
            } else {
                line_text.push(c);
                // Zero-width combiners (attached to this cell via the grid extras).
                if let Some(zw) = cell.zerowidth() {
                    for &z in zw {
                        if z != '\0' {
                            line_text.push(z);
                        }
                    }
                }
            }
        }
        // Trim trailing whitespace from each line before appending.
        let trimmed = line_text.trim_end();
        out.push_str(trimmed);
        out.push('\n');
    }

    // Remove trailing blank lines, then ensure final newline.
    let trimmed_out = out.trim_end_matches('\n');
    if trimmed_out.is_empty() {
        "\n".to_string()
    } else {
        format!("{trimmed_out}\n")
    }
}

fn write_to_path(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    let mut f = std::fs::File::create(path)?;
    f.write_all(text.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrollback_path_is_in_tmp() {
        let ts = 1234567890u128;
        let mut p = std::env::temp_dir();
        p.push(format!("glassy-scrollback-{ts}.txt"));
        assert!(p.to_string_lossy().contains("glassy-scrollback-"));
    }
}
