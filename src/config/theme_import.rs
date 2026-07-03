//! Theme import from Alacritty-TOML, base16-YAML, Kitty/Ghostty-conf, and
//! iTerm2 `.itermcolors` (plist) files.

use super::parse::parse_hex_color;
use crate::color;
use crate::color::Theme;
use anyhow::{Context, Result, bail};

/// Import a color theme from a theme file. Supports Alacritty TOML, base16
/// YAML, Kitty/Ghostty conf, and iTerm2 `.itermcolors` (plist) formats.
///
/// The file's extension picks the parser directly (`.itermcolors` -> plist,
/// `.conf` -> Kitty/Ghostty, `.toml` -> Alacritty, `.yaml`/`.yml` -> base16).
/// Any other extension (or none) falls back to content-sniffing: each parser
/// is tried in turn and the first one that recognizes at least one relevant
/// key wins.
pub fn import_theme_from_file(path: &str) -> Result<Theme> {
    use std::fs;
    use std::path::Path;

    let text =
        fs::read_to_string(path).with_context(|| format!("could not read theme file '{path}'"))?;

    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase());

    match ext.as_deref() {
        Some("itermcolors") => return import_theme_plist(&text),
        Some("conf") => return import_theme_kv(&text),
        Some("toml") => return import_theme_toml(&text),
        Some("yaml") | Some("yml") => return import_theme_yaml(&text),
        _ => {}
    }

    // Unknown or missing extension: sniff the content by trying each parser.
    // Each one now bails when it matches zero relevant keys (see below), so
    // this chain only "succeeds" on a parser that actually recognized the
    // file's format.
    if let Ok(theme) = import_theme_toml(&text) {
        return Ok(theme);
    }
    if let Ok(theme) = import_theme_yaml(&text) {
        return Ok(theme);
    }
    if let Ok(theme) = import_theme_kv(&text) {
        return Ok(theme);
    }
    if let Ok(theme) = import_theme_plist(&text) {
        return Ok(theme);
    }

    bail!(
        "could not parse '{path}' as a valid theme file \
         (TOML, YAML, Kitty/Ghostty conf, or iTerm2 plist)"
    )
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
                "foreground" => fg = Some(super::parse::parse_hex_color(value)?),
                "background" => bg = Some(super::parse::parse_hex_color(value)?),
                "cursor" => cursor = Some(super::parse::parse_hex_color(value)?),
                k if k.starts_with("color") => {
                    // Handle color0..color15
                    if let Some(idx_str) = k.strip_prefix("color")
                        && let Ok(idx) = idx_str.parse::<usize>()
                        && idx < 16
                    {
                        ansi16[idx] = Some(super::parse::parse_hex_color(value)?);
                    }
                }
                _ => {}
            }
        }
    }

    // If nothing matched, this isn't actually an Alacritty TOML theme (e.g. it's
    // base16 YAML, a Kitty/Ghostty conf, or garbage) — don't silently succeed with
    // an all-defaults Theme, which would mask the real format from the caller.
    if fg.is_none() && bg.is_none() && ansi16.iter().all(|c| c.is_none()) {
        bail!("no recognized Alacritty color keys found (foreground/background/color0-15)");
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
                colors[idx as usize] = Some(super::parse::parse_hex_color(value)?);
            }
        }
    }

    // If nothing matched, this isn't base16 YAML — don't silently succeed with an
    // all-defaults Theme (see the matching guard in `import_theme_toml`).
    if colors.iter().all(|c| c.is_none()) {
        bail!("no recognized base16 color keys found (base00-base0f)");
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

/// Parse Kitty/Ghostty conf format. Kitty uses space-separated `key value`
/// pairs (`foreground #c0caf5`, `color0 #15161e` .. `color15`, `cursor`,
/// `selection_background`); Ghostty uses `key = value` pairs where the hex
/// may omit the leading `#` (`foreground = c0caf5`, `cursor-color = ...`,
/// `selection-background = ...`) plus a repeated `palette = N=#hex` key for
/// the 16-color ANSI table. Both separators are accepted by one scanner.
pub(crate) fn import_theme_kv(text: &str) -> Result<Theme> {
    let mut fg = None;
    let mut bg = None;
    let mut cursor = None;
    let mut selection_bg = None;
    let mut ansi16 = [None; 16];

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }

        // Ghostty-style `key = value` takes priority (checked first since a
        // Kitty line never contains '='); otherwise fall back to Kitty-style
        // `key value` split on the first run of whitespace.
        let Some((key, value)) = line
            .split_once('=')
            .or_else(|| line.split_once(char::is_whitespace))
        else {
            continue;
        };
        let key = key.trim().to_lowercase();
        let value = unquote(value.trim());

        match key.as_str() {
            "foreground" => fg = Some(parse_hex_color(value)?),
            "background" => bg = Some(parse_hex_color(value)?),
            "cursor" | "cursor-color" | "cursor_color" => cursor = Some(parse_hex_color(value)?),
            "selection_background" | "selection-background" => {
                selection_bg = Some(parse_hex_color(value)?);
            }
            "palette" => {
                // Ghostty: `palette = N=#hex` — the first split above already
                // consumed the `palette =` part, so `value` is `N=#hex`.
                if let Some((idx_str, hex)) = value.split_once('=')
                    && let Ok(idx) = idx_str.trim().parse::<usize>()
                    && idx < 16
                {
                    ansi16[idx] = Some(parse_hex_color(hex.trim())?);
                }
            }
            k if k.starts_with("color") => {
                // Kitty: color0..color15
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

    // If nothing matched, this isn't a Kitty/Ghostty conf — don't silently
    // succeed with an all-defaults Theme (see the matching guard in
    // `import_theme_toml`).
    if fg.is_none() && bg.is_none() && ansi16.iter().all(|c| c.is_none()) {
        bail!("no recognized Kitty/Ghostty color keys found");
    }

    let fg = fg.unwrap_or(color::TOKYO_NIGHT.fg);
    let bg = bg.unwrap_or(color::TOKYO_NIGHT.bg);
    let cursor = cursor.unwrap_or(color::TOKYO_NIGHT.cursor);
    let selection_bg = selection_bg.unwrap_or(color::TOKYO_NIGHT.selection_bg);

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

/// Which `Theme` slot a top-level iTerm2 plist color dict maps to.
enum PlistTarget {
    Fg,
    Bg,
    Cursor,
    Selection,
    Ansi(usize),
}

/// Classify a top-level `<key>` string from an `.itermcolors` plist into the
/// `Theme` slot it feeds, if any (unrecognized keys like "Badge Color" or
/// "Link Color" are ignored).
fn classify_plist_key(key: &str) -> Option<PlistTarget> {
    match key {
        "Foreground Color" => Some(PlistTarget::Fg),
        "Background Color" => Some(PlistTarget::Bg),
        "Cursor Color" => Some(PlistTarget::Cursor),
        "Selection Color" => Some(PlistTarget::Selection),
        _ => {
            let n = key.strip_prefix("Ansi ")?.strip_suffix(" Color")?;
            let idx: usize = n.parse().ok()?;
            (idx < 16).then_some(PlistTarget::Ansi(idx))
        }
    }
}

/// Convert a plist `<real>` component (0.0..=1.0) into an 0..=255 channel.
fn component_to_u8(v: f64) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Minimal iTerm2 `.itermcolors` XML plist reader.
///
/// Only cares about top-level `<key>NAME</key>` entries where NAME is
/// "Foreground Color", "Background Color", "Cursor Color", "Selection
/// Color", or "Ansi 0 Color" .. "Ansi 15 Color", each followed by a `<dict>`
/// containing `<key>Red/Green/Blue Component</key>` + `<real>N</real>`
/// pairs ("Alpha Component" and "Color Space" are ignored). Doesn't parse
/// XML in general — it just scans for the handful of tags it needs, so it
/// works regardless of whether tags are one-per-line (iTerm2's own export
/// format) or share a line.
pub(crate) fn import_theme_plist(text: &str) -> Result<Theme> {
    let mut fg = None;
    let mut bg = None;
    let mut cursor = None;
    let mut selection_bg = None;
    let mut ansi16 = [None; 16];

    let mut depth: i32 = 0;
    let mut pending_key: Option<&str> = None;
    let mut target: Option<PlistTarget> = None;
    let mut target_depth: i32 = -1;
    let (mut r, mut g, mut b): (Option<f64>, Option<f64>, Option<f64>) = (None, None, None);

    // Walk the document tag-by-tag (not line-by-line) so same-line and
    // adjacent-line tag pairs are handled identically.
    let mut rest = text;
    while let Some(lt) = rest.find('<') {
        let after_lt = &rest[lt + 1..];
        let Some(gt) = after_lt.find('>') else {
            break;
        };
        let tag = &after_lt[..gt];
        let after_tag = &after_lt[gt + 1..];
        rest = after_tag;

        match tag {
            "dict" => {
                depth += 1;
                if target.is_none()
                    && let Some(key) = pending_key
                    && let Some(t) = classify_plist_key(key)
                {
                    target = Some(t);
                    target_depth = depth;
                    r = None;
                    g = None;
                    b = None;
                }
            }
            "/dict" => {
                if target.is_some() && depth == target_depth {
                    if let (Some(rr), Some(gg), Some(bb)) = (r, g, b) {
                        let rgb = alacritty_terminal::vte::ansi::Rgb {
                            r: component_to_u8(rr),
                            g: component_to_u8(gg),
                            b: component_to_u8(bb),
                        };
                        match target.take() {
                            Some(PlistTarget::Fg) => fg = Some(rgb),
                            Some(PlistTarget::Bg) => bg = Some(rgb),
                            Some(PlistTarget::Cursor) => cursor = Some(rgb),
                            Some(PlistTarget::Selection) => selection_bg = Some(rgb),
                            Some(PlistTarget::Ansi(idx)) => ansi16[idx] = Some(rgb),
                            None => {}
                        }
                    }
                    target = None;
                    target_depth = -1;
                }
                depth -= 1;
            }
            "key" => {
                if let Some(close) = rest.find("</key>") {
                    pending_key = Some(rest[..close].trim());
                    rest = &rest[close + "</key>".len()..];
                }
            }
            "real" => {
                if let Some(close) = rest.find("</real>") {
                    let content = rest[..close].trim();
                    rest = &rest[close + "</real>".len()..];
                    if target.is_some()
                        && let Some(key) = pending_key
                        && let Ok(v) = content.parse::<f64>()
                    {
                        match key {
                            "Red Component" => r = Some(v),
                            "Green Component" => g = Some(v),
                            "Blue Component" => b = Some(v),
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // If nothing matched, this isn't an iTerm2 plist — don't silently
    // succeed with an all-defaults Theme (see the matching guard in
    // `import_theme_toml`).
    if fg.is_none() && bg.is_none() && ansi16.iter().all(|c| c.is_none()) {
        bail!("no recognized iTerm2 plist color keys found");
    }

    let fg = fg.unwrap_or(color::TOKYO_NIGHT.fg);
    let bg = bg.unwrap_or(color::TOKYO_NIGHT.bg);
    let cursor = cursor.unwrap_or(color::TOKYO_NIGHT.cursor);
    let selection_bg = selection_bg.unwrap_or(color::TOKYO_NIGHT.selection_bg);

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

/// Remove surrounding quotes from a string.
fn unquote(s: &str) -> &str {
    s.trim_start_matches('"')
        .trim_end_matches('"')
        .trim_start_matches('\'')
        .trim_end_matches('\'')
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const KITTY_CONF: &str = r#"
# Kitty color scheme (Tokyo Night)
foreground #c0caf5
background #1a1b26
cursor #c0caf5
selection_background #33467c
color0  #15161e
color1  #f7768e
color2  #9ece6a
color3  #e0af68
color4  #7aa2f7
color5  #bb9af7
color6  #7dcfff
color7  #a9b1d6
color8  #414868
color9  #f7768e
color10 #9ece6a
color11 #e0af68
color12 #7aa2f7
color13 #bb9af7
color14 #7dcfff
color15 #c0caf5
"#;

    // Ghostty's palette values include one deliberately without a leading
    // '#' (index 15) to exercise the no-# hex path.
    const GHOSTTY_CONF: &str = r#"
# Ghostty color scheme (Tokyo Night)
foreground = c0caf5
background = 1a1b26
cursor-color = c0caf5
selection-background = 33467c
palette = 0=#15161e
palette = 1=#f7768e
palette = 2=#9ece6a
palette = 3=#e0af68
palette = 4=#7aa2f7
palette = 5=#bb9af7
palette = 6=#7dcfff
palette = 7=#a9b1d6
palette = 8=#414868
palette = 9=#f7768e
palette = 10=#9ece6a
palette = 11=#e0af68
palette = 12=#7aa2f7
palette = 13=#bb9af7
palette = 14=#7dcfff
palette = 15=c0caf5
"#;

    // Shaped like a real iTerm2-Color-Schemes export: one tag per line,
    // includes an ignorable key (Badge Color) to prove it doesn't leak into
    // the adjacent recognized dict.
    const ITERM_PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Ansi 0 Color</key>
	<dict>
		<key>Alpha Component</key>
		<real>1</real>
		<key>Blue Component</key>
		<real>0.1176470588235294</real>
		<key>Color Space</key>
		<string>sRGB</string>
		<key>Green Component</key>
		<real>0.08627450980392157</real>
		<key>Red Component</key>
		<real>0.08235294117647059</real>
	</dict>
	<key>Ansi 15 Color</key>
	<dict>
		<key>Alpha Component</key>
		<real>1</real>
		<key>Blue Component</key>
		<real>0.9607843137254902</real>
		<key>Color Space</key>
		<string>sRGB</string>
		<key>Green Component</key>
		<real>0.792156862745098</real>
		<key>Red Component</key>
		<real>0.7529411764705882</real>
	</dict>
	<key>Badge Color</key>
	<dict>
		<key>Alpha Component</key>
		<real>0.5</real>
		<key>Blue Component</key>
		<real>0.5</real>
		<key>Color Space</key>
		<string>sRGB</string>
		<key>Green Component</key>
		<real>0.5</real>
		<key>Red Component</key>
		<real>0.5</real>
	</dict>
	<key>Background Color</key>
	<dict>
		<key>Alpha Component</key>
		<real>1</real>
		<key>Blue Component</key>
		<real>0.1490196078431373</real>
		<key>Color Space</key>
		<string>sRGB</string>
		<key>Green Component</key>
		<real>0.1058823529411765</real>
		<key>Red Component</key>
		<real>0.1019607843137255</real>
	</dict>
	<key>Cursor Color</key>
	<dict>
		<key>Alpha Component</key>
		<real>1</real>
		<key>Blue Component</key>
		<real>1</real>
		<key>Color Space</key>
		<string>sRGB</string>
		<key>Green Component</key>
		<real>0.8117647058823529</real>
		<key>Red Component</key>
		<real>0.4901960784313726</real>
	</dict>
	<key>Foreground Color</key>
	<dict>
		<key>Alpha Component</key>
		<real>1</real>
		<key>Blue Component</key>
		<real>0.9607843137254902</real>
		<key>Color Space</key>
		<string>sRGB</string>
		<key>Green Component</key>
		<real>0.792156862745098</real>
		<key>Red Component</key>
		<real>0.7529411764705882</real>
	</dict>
	<key>Selection Color</key>
	<dict>
		<key>Alpha Component</key>
		<real>1</real>
		<key>Blue Component</key>
		<real>0.4862745098039216</real>
		<key>Color Space</key>
		<string>sRGB</string>
		<key>Green Component</key>
		<real>0.2745098039215686</real>
		<key>Red Component</key>
		<real>0.2</real>
	</dict>
</dict>
</plist>
"#;

    const BASE16_YAML: &str = r#"
scheme: "Tokyo Night"
author: "someone"
base00: "1a1b26"
base01: "16161e"
base02: "2f3549"
base03: "444b6a"
base04: "787c99"
base05: "a9b1d6"
base06: "cbccd1"
base07: "d5d6db"
base08: "f7768e"
base09: "ff9e64"
base0A: "e0af68"
base0B: "9ece6a"
base0C: "7dcfff"
base0D: "7aa2f7"
base0E: "bb9af7"
base0F: "b4f9f8"
"#;

    fn write_temp(name: &str, ext: &str, contents: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "glassy-theme-import-test-{name}-{}.{ext}",
            std::process::id()
        ));
        std::fs::write(&p, contents).expect("write temp theme file");
        p
    }

    // -- Kitty ---------------------------------------------------------

    #[test]
    fn kv_parses_kitty_snippet() {
        let theme = import_theme_kv(KITTY_CONF).expect("kitty snippet should parse");
        assert_eq!((theme.fg.r, theme.fg.g, theme.fg.b), (0xc0, 0xca, 0xf5));
        assert_eq!((theme.bg.r, theme.bg.g, theme.bg.b), (0x1a, 0x1b, 0x26));
        assert_eq!(
            (
                theme.selection_bg.r,
                theme.selection_bg.g,
                theme.selection_bg.b
            ),
            (0x33, 0x46, 0x7c)
        );
        assert_eq!(
            (theme.ansi16[1].r, theme.ansi16[1].g, theme.ansi16[1].b),
            (0xf7, 0x76, 0x8e)
        );
        assert_eq!(
            (theme.ansi16[15].r, theme.ansi16[15].g, theme.ansi16[15].b),
            (0xc0, 0xca, 0xf5)
        );
    }

    // -- Ghostty ---------------------------------------------------------

    #[test]
    fn kv_parses_ghostty_snippet_incl_palette_and_bare_hex() {
        let theme = import_theme_kv(GHOSTTY_CONF).expect("ghostty snippet should parse");
        assert_eq!((theme.fg.r, theme.fg.g, theme.fg.b), (0xc0, 0xca, 0xf5));
        assert_eq!((theme.bg.r, theme.bg.g, theme.bg.b), (0x1a, 0x1b, 0x26));
        assert_eq!(
            (theme.cursor.r, theme.cursor.g, theme.cursor.b),
            (0xc0, 0xca, 0xf5)
        );
        assert_eq!(
            (
                theme.selection_bg.r,
                theme.selection_bg.g,
                theme.selection_bg.b
            ),
            (0x33, 0x46, 0x7c)
        );
        assert_eq!(
            (theme.ansi16[0].r, theme.ansi16[0].g, theme.ansi16[0].b),
            (0x15, 0x16, 0x1e)
        );
        // index 15 in the fixture is written without a leading '#'.
        assert_eq!(
            (theme.ansi16[15].r, theme.ansi16[15].g, theme.ansi16[15].b),
            (0xc0, 0xca, 0xf5)
        );
    }

    // -- iTerm2 plist ------------------------------------------------------

    #[test]
    fn plist_parses_itermcolors_snippet() {
        let theme = import_theme_plist(ITERM_PLIST).expect("itermcolors snippet should parse");
        assert_eq!((theme.fg.r, theme.fg.g, theme.fg.b), (0xc0, 0xca, 0xf5));
        assert_eq!((theme.bg.r, theme.bg.g, theme.bg.b), (0x1a, 0x1b, 0x26));
        assert_eq!(
            (theme.cursor.r, theme.cursor.g, theme.cursor.b),
            (0x7d, 0xcf, 0xff)
        );
        assert_eq!(
            (
                theme.selection_bg.r,
                theme.selection_bg.g,
                theme.selection_bg.b
            ),
            (0x33, 0x46, 0x7c)
        );
        assert_eq!(
            (theme.ansi16[0].r, theme.ansi16[0].g, theme.ansi16[0].b),
            (0x15, 0x16, 0x1e)
        );
        assert_eq!(
            (theme.ansi16[15].r, theme.ansi16[15].g, theme.ansi16[15].b),
            (0xc0, 0xca, 0xf5)
        );
    }

    // -- base16 YAML (still works after the bug fix) ------------------------

    #[test]
    fn yaml_still_parses_base16() {
        let theme = import_theme_yaml(BASE16_YAML).expect("base16 yaml should parse");
        // fg = base05, bg = base00, cursor = base07
        assert_eq!((theme.fg.r, theme.fg.g, theme.fg.b), (0xa9, 0xb1, 0xd6));
        assert_eq!((theme.bg.r, theme.bg.g, theme.bg.b), (0x1a, 0x1b, 0x26));
        assert_eq!(
            (theme.cursor.r, theme.cursor.g, theme.cursor.b),
            (0xd5, 0xd6, 0xdb)
        );
    }

    // -- Bug fix: bail on zero matched keys ------------------------------

    // `color::Theme` doesn't implement `Debug`, so `expect_err`/`unwrap_err`
    // (which require `T: Debug` for their panic message) don't compile here —
    // match on the `Result` instead.

    #[test]
    fn toml_zero_match_errors() {
        // Valid-looking TOML with '=' signs, but no color keys at all.
        let text = "[window]\nopacity = 0.9\ntitle = \"term\"\n";
        match import_theme_toml(text) {
            Ok(_) => panic!("unrelated TOML keys must not match"),
            Err(err) => assert!(err.to_string().contains("no recognized Alacritty")),
        }
    }

    #[test]
    fn yaml_zero_match_errors() {
        let text = "name: \"not a theme\"\nauthor: \"nobody\"\n";
        match import_theme_yaml(text) {
            Ok(_) => panic!("unrelated YAML keys must not match"),
            Err(err) => assert!(err.to_string().contains("no recognized base16")),
        }
    }

    #[test]
    fn kv_zero_match_errors() {
        let text = "font_size 12\nenable_audio_bell no\n";
        match import_theme_kv(text) {
            Ok(_) => panic!("unrelated conf keys must not match"),
            Err(err) => assert!(err.to_string().contains("no recognized Kitty/Ghostty")),
        }
    }

    #[test]
    fn plist_zero_match_errors() {
        let text = "<plist version=\"1.0\"><dict></dict></plist>";
        match import_theme_plist(text) {
            Ok(_) => panic!("empty plist dict must not match"),
            Err(err) => assert!(err.to_string().contains("no recognized iTerm2")),
        }
    }

    #[test]
    fn garbage_input_errors_not_silent_tokyo_night() {
        let path = write_temp(
            "garbage",
            "theme",
            "hello world\nthis is not a theme file\njust some prose here\n",
        );
        let result = import_theme_from_file(path.to_str().unwrap());
        let _ = std::fs::remove_file(&path);
        assert!(
            result.is_err(),
            "garbage input must error, not silently return Tokyo Night defaults"
        );
    }

    // -- Extension-based dispatch ------------------------------------------

    #[test]
    fn dispatch_picks_toml_parser_for_toml_extension() {
        let path = write_temp(
            "dispatch",
            "toml",
            "[colors.primary]\nforeground = \"#c0caf5\"\nbackground = \"#1a1b26\"\n",
        );
        let theme =
            import_theme_from_file(path.to_str().unwrap()).expect("toml extension should parse");
        let _ = std::fs::remove_file(&path);
        assert_eq!((theme.fg.r, theme.fg.g, theme.fg.b), (0xc0, 0xca, 0xf5));
    }

    #[test]
    fn dispatch_picks_yaml_parser_for_yaml_extension() {
        let path = write_temp("dispatch", "yaml", BASE16_YAML);
        let theme =
            import_theme_from_file(path.to_str().unwrap()).expect("yaml extension should parse");
        let _ = std::fs::remove_file(&path);
        assert_eq!((theme.bg.r, theme.bg.g, theme.bg.b), (0x1a, 0x1b, 0x26));
    }

    #[test]
    fn dispatch_picks_kv_parser_for_conf_extension() {
        let path = write_temp("dispatch", "conf", GHOSTTY_CONF);
        let theme =
            import_theme_from_file(path.to_str().unwrap()).expect("conf extension should parse");
        let _ = std::fs::remove_file(&path);
        assert_eq!((theme.fg.r, theme.fg.g, theme.fg.b), (0xc0, 0xca, 0xf5));
    }

    #[test]
    fn dispatch_picks_plist_parser_for_itermcolors_extension() {
        let path = write_temp("dispatch", "itermcolors", ITERM_PLIST);
        let theme = import_theme_from_file(path.to_str().unwrap())
            .expect("itermcolors extension should parse");
        let _ = std::fs::remove_file(&path);
        assert_eq!((theme.bg.r, theme.bg.g, theme.bg.b), (0x1a, 0x1b, 0x26));
    }

    #[test]
    fn dispatch_does_not_fall_back_on_matching_but_bad_extension() {
        // A .toml file with content that only a Kitty conf parser could read
        // must NOT silently fall back to KV — the extension pins the parser.
        let path = write_temp("dispatch-strict", "toml", KITTY_CONF);
        let result = import_theme_from_file(path.to_str().unwrap());
        let _ = std::fs::remove_file(&path);
        assert!(
            result.is_err(),
            ".toml extension must use the TOML parser only, not sniff"
        );
    }

    // -- Content-sniff fallback for unknown/no extension --------------------

    #[test]
    fn sniff_fallback_finds_kitty_conf_with_unknown_extension() {
        // Kitty's space-separated format has no '=' or ':' anywhere, so it
        // cannot be mistaken for TOML or YAML — a clean sniff-chain case.
        let path = write_temp("sniff", "txt", KITTY_CONF);
        let theme = import_theme_from_file(path.to_str().unwrap())
            .expect("kitty content should be found via sniff fallback");
        let _ = std::fs::remove_file(&path);
        assert_eq!((theme.bg.r, theme.bg.g, theme.bg.b), (0x1a, 0x1b, 0x26));
        assert_eq!(
            (theme.ansi16[1].r, theme.ansi16[1].g, theme.ansi16[1].b),
            (0xf7, 0x76, 0x8e)
        );
    }

    #[test]
    fn sniff_fallback_finds_itermcolors_with_no_extension() {
        let path = write_temp("sniff-noext", "", ITERM_PLIST);
        // write_temp always appends a `.` + ext; strip the trailing dot for a
        // "no extension" path by renaming.
        let noext = path.with_extension("");
        std::fs::rename(&path, &noext).expect("rename to extensionless path");
        let theme = import_theme_from_file(noext.to_str().unwrap())
            .expect("itermcolors content should be found via sniff fallback");
        let _ = std::fs::remove_file(&noext);
        assert_eq!((theme.fg.r, theme.fg.g, theme.fg.b), (0xc0, 0xca, 0xf5));
    }
}
