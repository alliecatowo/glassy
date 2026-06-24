//! Theme import from TOML/YAML files (Alacritty-compatible or base16 format).

use anyhow::{Result, Context, bail};
use crate::color::Theme;
use crate::color;

/// Import a color theme from a TOML/YAML file (Alacritty-compatible or base16 format).
/// Supports both inline Alacritty color tables and base16 palette arrays.
pub fn import_theme_from_file(path: &str) -> Result<Theme> {
    use std::fs;

    let text = fs::read_to_string(path)
        .with_context(|| format!("could not read theme file '{path}'"))?;

    // Try to parse as TOML first (covers Alacritty format).
    if let Ok(theme) = import_theme_toml(&text) {
        return Ok(theme);
    }

    // Try YAML (base16, iTerm2, etc.)
    if let Ok(theme) = import_theme_yaml(&text) {
        return Ok(theme);
    }

    bail!("could not parse '{path}' as a valid theme file (TOML or YAML)")
}

/// Parse Alacritty TOML theme format (has [colors] section with various keys).
pub(crate) fn import_theme_toml(text: &str) -> Result<Theme> {
    // Simple TOML parser for just the colors section (don't want toml dependency).
    let mut fg = None;
    let mut bg = None;
    let mut cursor = None;
    let mut ansi16 = [None; 16];

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Look for color definitions like: foreground = "#ffffff"
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim().to_lowercase();
            let value = unquote(value.trim());

            match key.as_str() {
                "foreground" => fg = Some(parse_hex_color(value)?),
                "background" => bg = Some(parse_hex_color(value)?),
                "cursor" => cursor = Some(parse_hex_color(value)?),
                k if k.starts_with("color") => {
                    // Handle color0..color15
                    if let Some(idx_str) = k.strip_prefix("color")
                        && let Ok(idx) = idx_str.parse::<usize>()
                        && idx < 16
                    {
                        ansi16[idx] = Some(parse_hex_color(value)?);
                    }
                }
                _ => {}
            }
        }
    }

    // Default values if not provided.
    let fg = fg.unwrap_or(color::TOKYO_NIGHT.fg);
    let bg = bg.unwrap_or(color::TOKYO_NIGHT.bg);
    let cursor = cursor.unwrap_or(color::TOKYO_NIGHT.cursor);
    let selection_bg = color::TOKYO_NIGHT.selection_bg;

    // Fill any missing ANSI colors with Tokyo Night defaults.
    let mut final_ansi = color::TOKYO_NIGHT.ansi16;
    for (i, rgb) in ansi16.iter().enumerate() {
        if let Some(c) = rgb {
            final_ansi[i] = *c;
        }
    }

    Ok(Theme {
        fg,
        bg,
        cursor,
        selection_bg,
        ansi16: final_ansi,
    })
}

/// Parse base16 YAML format (e.g., https://github.com/chriskempson/base16).
/// Format: base00: "#ffffff" ... base0f: "#000000"
pub(crate) fn import_theme_yaml(text: &str) -> Result<Theme> {
    let mut colors = [None; 16];

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim().to_lowercase();
            let value = unquote(value.trim().trim_start_matches('"').trim_end_matches('"'));

            if let Some(hex) = key.strip_prefix("base")
                && hex.len() == 2
                && let Ok(idx) = u8::from_str_radix(hex, 16)
                && (idx as usize) < 16
            {
                colors[idx as usize] = Some(parse_hex_color(value)?);
            }
        }
    }

    // Map base16 palette (base00-0f) to standard terminal colors.
    // base00=black, base01=red, ..., base0f=white (simplified mapping).
    let mut ansi16 = color::TOKYO_NIGHT.ansi16;
    for (i, color) in colors.iter().enumerate() {
        if let Some(c) = color {
            ansi16[i] = *c;
        }
    }

    // Derive fg/bg from base05/base00 (text/background in base16).
    let fg = colors[5].unwrap_or(color::TOKYO_NIGHT.fg);
    let bg = colors[0].unwrap_or(color::TOKYO_NIGHT.bg);
    let cursor = colors[7].unwrap_or(color::TOKYO_NIGHT.cursor); // base07 = white
    let selection_bg = color::TOKYO_NIGHT.selection_bg; // Use default if not specified

    Ok(Theme {
        fg,
        bg,
        cursor,
        selection_bg,
        ansi16,
    })
}

/// Remove surrounding quotes from a string.
fn unquote(s: &str) -> &str {
    s.trim_start_matches('"')
        .trim_end_matches('"')
        .trim_start_matches('\'')
        .trim_end_matches('\'')
}

/// Parse a hex color string (e.g., "#ff0000") into an RGB triple.
fn parse_hex_color(s: &str) -> Result<alacritty_terminal::vte::ansi::Rgb> {
    use alacritty_terminal::vte::ansi::Rgb;
    let s = s.trim_start_matches('#');
    if s.len() != 6 {
        bail!("invalid hex color '{s}' (must be 6 digits)");
    }
    let r = u8::from_str_radix(&s[0..2], 16)?;
    let g = u8::from_str_radix(&s[2..4], 16)?;
    let b = u8::from_str_radix(&s[4..6], 16)?;
    Ok(Rgb { r, g, b })
}
