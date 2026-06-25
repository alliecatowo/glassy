//! Configuration file parsing: RawConfig accumulation, file I/O, and value parsing.

use alacritty_terminal::tty::Shell;
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::app::Config;
use crate::color;
use crate::renderer::DEFAULT_OPACITY;

use super::keymap::{build_keymap, default_keymap};

const DEFAULT_FONT_SIZE: f32 = 14.0;
const DEFAULT_SCROLLBACK: usize = 10_000;

/// Accumulated raw configuration before validation/finalization. Every field is
/// optional so the file and CLI layers can each set a subset.
#[derive(Default)]
pub(super) struct RawConfig {
    pub font_family: Option<String>,
    pub font_size: Option<f32>,
    pub theme: Option<String>,
    pub opacity: Option<f32>,
    pub padding: Option<f32>,
    pub padding_top: Option<f32>,
    pub padding_bottom: Option<f32>,
    pub padding_left: Option<f32>,
    pub padding_right: Option<f32>,
    pub shell: Option<Shell>,
    pub scrollback: Option<usize>,
    pub bell_visual: Option<bool>,
    pub bell_audible: Option<bool>,
    pub follow_system: Option<bool>,
    pub theme_light: Option<String>,
    pub theme_dark: Option<String>,
    pub status_bar: Option<bool>,
    pub pane_headers: Option<bool>,
    pub word_separator: Option<String>,
    pub ligatures: Option<bool>,
    pub font_features: Option<Vec<String>>,
    pub cwd: Option<String>,
    pub restore_session: Option<bool>,
    pub copy_on_select: Option<bool>,
    pub color_fg: Option<String>,
    pub color_bg: Option<String>,
    pub color_cursor: Option<String>,
    pub color_selection_bg: Option<String>,
    pub color_ansi: Option<[Option<String>; 16]>,
    pub profiles: HashMap<String, Vec<(String, String)>>,
    pub keybinding_overrides: Vec<(String, String)>,
    /// Path to an image file from which the theme should be auto-generated on
    /// startup (via `theme_gen`). When set, the generated theme overrides any
    /// `theme = …` setting and any `color.*` overrides.
    pub wallpaper_theme: Option<String>,
}

impl RawConfig {
    pub fn into_settings(self) -> Result<super::Settings> {
        let theme_input = self.theme.as_deref().unwrap_or("tokyo-night");
        let mut theme = color::theme_by_name(theme_input).unwrap_or_else(|| {
            log::warn!("glassy: unknown theme '{theme_input}'; using Tokyo Night");
            color::theme_by_name("tokyo-night").expect("default theme exists")
        });
        let theme_name = color::canonical_name(theme_input).to_string();

        // Apply custom color overrides if provided.
        if self.color_fg.is_some()
            || self.color_bg.is_some()
            || self.color_cursor.is_some()
            || self.color_selection_bg.is_some()
            || self.color_ansi.is_some()
        {
            if let Some(fg) = self.color_fg {
                theme.fg = parse_hex_color(&fg)?;
            }
            if let Some(bg) = self.color_bg {
                theme.bg = parse_hex_color(&bg)?;
            }
            if let Some(cursor) = self.color_cursor {
                theme.cursor = parse_hex_color(&cursor)?;
            }
            if let Some(sel_bg) = self.color_selection_bg {
                theme.selection_bg = parse_hex_color(&sel_bg)?;
            }
            if let Some(ansi_colors) = self.color_ansi {
                for (i, color_str) in ansi_colors.iter().enumerate() {
                    if let Some(color) = color_str {
                        theme.ansi16[i] = parse_hex_color(color)?;
                    }
                }
            }
        }

        // Apply wallpaper-generated theme if a path is configured.
        // This overrides all named-theme and color.* settings.
        if let Some(ref path) = self.wallpaper_theme {
            match super::theme_gen::from_image_path(path) {
                Ok(generated) => {
                    theme = generated;
                }
                Err(e) => {
                    log::warn!("glassy: wallpaper_theme '{path}' failed, using fallback: {e}");
                }
            }
        }

        let follow_system = self.follow_system.unwrap_or(false);
        let theme_dark =
            color::canonical_name(self.theme_dark.as_deref().unwrap_or(&theme_name)).to_string();
        let theme_light =
            color::canonical_name(self.theme_light.as_deref().unwrap_or("rose-pine-dawn"))
                .to_string();

        let opacity = self.opacity.unwrap_or(DEFAULT_OPACITY);
        let opacity = if opacity.is_finite() {
            opacity.clamp(0.0, 1.0)
        } else {
            DEFAULT_OPACITY
        };
        let font_size = self.font_size.unwrap_or(DEFAULT_FONT_SIZE);
        let font_size = if font_size.is_finite() && font_size > 0.0 {
            font_size
        } else {
            DEFAULT_FONT_SIZE
        };
        let config = Config {
            font_family: self.font_family,
            font_size,
            opacity,
            padding: self.padding,
            padding_top: self.padding_top,
            padding_bottom: self.padding_bottom,
            padding_left: self.padding_left,
            padding_right: self.padding_right,
            scrollback: self.scrollback.unwrap_or(DEFAULT_SCROLLBACK),
            shell: self.shell,
            bell_visual: self.bell_visual.unwrap_or(true),
            bell_audible: self.bell_audible.unwrap_or(false),
            theme: theme_name,
            follow_system,
            theme_light,
            theme_dark,
            status_bar: self.status_bar.unwrap_or(false),
            pane_headers: self.pane_headers.unwrap_or(false),
            word_separator: self.word_separator.unwrap_or_default(),
            ligatures: self.ligatures.unwrap_or(false),
            font_features: self.font_features.unwrap_or_default(),
            initial_cwd: self.cwd.filter(|s| !s.is_empty()).map(PathBuf::from),
            restore_session: self.restore_session.unwrap_or(false),
            copy_on_select: self.copy_on_select.unwrap_or(false),
            keymap: build_keymap(default_keymap(), &self.keybinding_overrides),
            wallpaper_theme: self
                .wallpaper_theme
                .filter(|s| !s.is_empty())
                .map(PathBuf::from),
        };

        Ok(super::Settings { config, theme })
    }

    /// Apply the named profile's key/value pairs over the base config, returning an
    /// error if the profile is unknown or one of its values fails to parse. Called
    /// after the file load and before CLI overrides, so the CLI still wins.
    pub fn activate_profile(&mut self, name: &str) -> Result<()> {
        let key = name.to_ascii_lowercase();
        let pairs =
            self.profiles.get(&key).cloned().with_context(|| {
                format!("unknown profile '{name}' (no [profile.{name}] section)")
            })?;
        for (k, v) in &pairs {
            apply_kv(k, v, self).with_context(|| format!("in [profile.{name}]"))?;
        }
        Ok(())
    }
}

/// The resolved config file path, honoring `$XDG_CONFIG_HOME` then `$HOME`.
/// Public so the in-app settings overlay can show + write it.
pub fn path() -> Option<PathBuf> {
    config_path()
}

/// Persist `updates` (`(key, value)` pairs) into the config file, preserving all
/// other lines, comments, and ordering. A key already present is updated in place;
/// a missing key is appended. Creates the parent directory and file if needed.
/// Used by the live settings overlay so changes survive a restart.
pub fn save(updates: &[(&str, String)]) -> Result<()> {
    let path = config_path().context("no config path (HOME/XDG unset)")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating config dir {}", dir.display()))?;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let out = merge_config(&existing, updates);
    std::fs::write(&path, out).with_context(|| format!("writing config {}", path.display()))?;
    Ok(())
}

/// Merge `updates` into the text of a config file: a present key is updated in
/// place (preserving its position), a missing one is appended; comments, blank
/// lines, unmanaged keys, and ordering are preserved. Pure for unit testing.
fn merge_config(existing: &str, updates: &[(&str, String)]) -> String {
    let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();
    let mut written = vec![false; updates.len()];

    for line in lines.iter_mut() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        let Some((key, _)) = trimmed.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        for (i, (k, v)) in updates.iter().enumerate() {
            if !written[i] && key == *k {
                // Strip newlines and carriage returns from value to prevent injection.
                let clean_v = v.replace(['\n', '\r'], "");
                *line = format!("{k} = {clean_v}");
                written[i] = true;
            }
        }
    }
    for (i, (k, v)) in updates.iter().enumerate() {
        if !written[i] {
            // Strip newlines and carriage returns from value to prevent injection.
            let clean_v = v.replace(['\n', '\r'], "");
            lines.push(format!("{k} = {clean_v}"));
        }
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// The resolved config file path, honoring `$XDG_CONFIG_HOME` then `$HOME`.
/// On macOS, uses ~/Library/Application Support/glassy/glassy.conf.
/// On other platforms, honors $XDG_CONFIG_HOME then ~/.config/glassy/glassy.conf.
fn config_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")?;
        return Some(PathBuf::from(home).join("Library/Application Support/glassy/glassy.conf"));
    }

    #[cfg(not(target_os = "macos"))]
    {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME")
            && !xdg.is_empty()
        {
            return Some(PathBuf::from(xdg).join("glassy/glassy.conf"));
        }
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join(".config/glassy/glassy.conf"))
    }
}

/// Section discriminant for the config file parser.
#[derive(Clone, PartialEq)]
enum Section {
    Global,
    Profile(String),
    Keybindings,
    Unknown,
}

/// Parse a `KEY=VALUE` config file into `raw`. Blank lines and `#`/`;` comments
/// are ignored; surrounding whitespace and a single layer of matching quotes are
/// stripped from values. An unknown key is warned about but not fatal; a value
/// that fails to parse for a known key is a hard error (with the line number).
pub(super) fn parse_config_file(text: &str, raw: &mut RawConfig) -> Result<()> {
    let mut section = Section::Global;
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        // Section header.
        if line.starts_with('[') && line.ends_with(']') {
            let name = line[1..line.len() - 1].trim();
            section = if name.eq_ignore_ascii_case("keybindings") {
                Section::Keybindings
            } else if let Some(profile_name) = name.strip_prefix("profile.") {
                let n = profile_name.trim().to_ascii_lowercase();
                if n.is_empty() {
                    Section::Unknown
                } else {
                    Section::Profile(n)
                }
            } else {
                Section::Unknown
            };
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            bail!("line {}: expected KEY=VALUE, got '{line}'", i + 1);
        };
        let key = key.trim().to_ascii_lowercase();
        let value = unquote(value.trim());
        match &section {
            Section::Global => {
                apply_kv(&key, value, raw).with_context(|| format!("line {}", i + 1))?;
            }
            Section::Profile(name) => {
                raw.profiles
                    .entry(name.clone())
                    .or_default()
                    .push((key, value.to_string()));
            }
            Section::Keybindings => {
                raw.keybinding_overrides.push((key, value.to_string()));
            }
            Section::Unknown => {
                // Skip content of unrecognized sections for forward-compat.
            }
        }
    }
    Ok(())
}

/// Strip one layer of matching single or double quotes from `s`, if present.
fn unquote(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let (a, b) = (bytes[0], bytes[bytes.len() - 1]);
        if (a == b'"' && b == b'"') || (a == b'\'' && b == b'\'') {
            return &s[1..s.len() - 1];
        }
    }
    s
}

/// Parse a hex color string (with or without leading #) to an Rgb.
pub(super) fn parse_hex_color(s: &str) -> Result<alacritty_terminal::vte::ansi::Rgb> {
    let hex = s.trim_start_matches('#');
    if hex.len() != 6 {
        bail!("color must be a 6-digit hex value, got '{s}'");
    }
    let r =
        u8::from_str_radix(&hex[0..2], 16).with_context(|| format!("invalid hex color '{s}'"))?;
    let g =
        u8::from_str_radix(&hex[2..4], 16).with_context(|| format!("invalid hex color '{s}'"))?;
    let b =
        u8::from_str_radix(&hex[4..6], 16).with_context(|| format!("invalid hex color '{s}'"))?;
    Ok(alacritty_terminal::vte::ansi::Rgb { r, g, b })
}

/// Apply a single recognized `key`/`value` pair into `raw`.
pub(super) fn apply_kv(key: &str, value: &str, raw: &mut RawConfig) -> Result<()> {
    match key {
        "font_family" => {
            if !value.is_empty() {
                raw.font_family = Some(value.to_string());
            }
        }
        "font_size" => {
            raw.font_size = Some(parse_pos_f32(value, "font_size")?);
        }
        "theme" => {
            if !value.is_empty() {
                raw.theme = Some(value.to_string());
            }
        }
        "opacity" => {
            let o: f32 = value
                .parse()
                .with_context(|| format!("opacity: invalid number '{value}'"))?;
            raw.opacity = Some(o);
        }
        "padding" => {
            let p: f32 = value
                .parse()
                .with_context(|| format!("padding: invalid number '{value}'"))?;
            if p < 0.0 {
                bail!("padding must be >= 0, got {p}");
            }
            raw.padding = Some(p);
        }
        "padding_top" => {
            let p: f32 = value
                .parse()
                .with_context(|| format!("padding_top: invalid number '{value}'"))?;
            if p < 0.0 {
                bail!("padding_top must be >= 0, got {p}");
            }
            raw.padding_top = Some(p);
        }
        "padding_bottom" => {
            let p: f32 = value
                .parse()
                .with_context(|| format!("padding_bottom: invalid number '{value}'"))?;
            if p < 0.0 {
                bail!("padding_bottom must be >= 0, got {p}");
            }
            raw.padding_bottom = Some(p);
        }
        "padding_left" => {
            let p: f32 = value
                .parse()
                .with_context(|| format!("padding_left: invalid number '{value}'"))?;
            if p < 0.0 {
                bail!("padding_left must be >= 0, got {p}");
            }
            raw.padding_left = Some(p);
        }
        "padding_right" => {
            let p: f32 = value
                .parse()
                .with_context(|| format!("padding_right: invalid number '{value}'"))?;
            if p < 0.0 {
                bail!("padding_right must be >= 0, got {p}");
            }
            raw.padding_right = Some(p);
        }
        "shell" => {
            if let Some(shell) = parse_shell(value) {
                raw.shell = Some(shell);
            }
        }
        "scrollback" => {
            let n: usize = value
                .parse()
                .with_context(|| format!("scrollback: invalid integer '{value}'"))?;
            raw.scrollback = Some(n.clamp(0, 1_000_000));
        }
        "bell_visual" => {
            raw.bell_visual = Some(parse_bool(value, "bell_visual")?);
        }
        "bell_audible" => {
            raw.bell_audible = Some(parse_bool(value, "bell_audible")?);
        }
        "follow_system" => {
            raw.follow_system = Some(parse_bool(value, "follow_system")?);
        }
        "theme_light" => {
            if !value.is_empty() {
                raw.theme_light = Some(value.to_string());
            }
        }
        "theme_dark" => {
            if !value.is_empty() {
                raw.theme_dark = Some(value.to_string());
            }
        }
        "status_bar" => {
            raw.status_bar = Some(parse_bool(value, "status_bar")?);
        }
        "pane_headers" => {
            raw.pane_headers = Some(parse_bool(value, "pane_headers")?);
        }
        "word_separator" => {
            raw.word_separator = Some(value.to_string());
        }
        "ligatures" => {
            raw.ligatures = Some(parse_bool(value, "ligatures")?);
        }
        "font_features" => {
            let features: Vec<String> = value
                .split([',', ' '])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|token| {
                    let tag = token.split('=').next().unwrap_or(token).trim();
                    if tag.len() != 4 || !tag.is_ascii() {
                        log::warn!(
                            "glassy: ignoring invalid font_features entry '{}' \
                             (tag must be exactly 4 ASCII characters)",
                            token
                        );
                    }
                    token.to_string()
                })
                .collect();
            raw.font_features = Some(features);
        }
        "cwd" => {
            if !value.is_empty() {
                raw.cwd = Some(value.to_string());
            }
        }
        "restore_session" => {
            raw.restore_session = Some(parse_bool(value, "restore_session")?);
        }
        "copy_on_select" => {
            raw.copy_on_select = Some(parse_bool(value, "copy_on_select")?);
        }
        "color.fg" => {
            parse_hex_color(value)?;
            raw.color_fg = Some(value.to_string());
        }
        "color.bg" => {
            parse_hex_color(value)?;
            raw.color_bg = Some(value.to_string());
        }
        "color.cursor" => {
            parse_hex_color(value)?;
            raw.color_cursor = Some(value.to_string());
        }
        "color.selection_bg" => {
            parse_hex_color(value)?;
            raw.color_selection_bg = Some(value.to_string());
        }
        k if k.starts_with("color.ansi") => {
            let ansi_idx = k
                .strip_prefix("color.ansi")
                .and_then(|s| s.parse::<usize>().ok())
                .filter(|&idx| idx < 16);
            if let Some(idx) = ansi_idx {
                parse_hex_color(value)?;
                if raw.color_ansi.is_none() {
                    raw.color_ansi = Some(Default::default());
                }
                if let Some(ref mut ansi) = raw.color_ansi {
                    ansi[idx] = Some(value.to_string());
                }
            } else {
                log::warn!("glassy: ignoring invalid color key '{k}'");
            }
        }
        "wallpaper_theme" => {
            if !value.is_empty() {
                raw.wallpaper_theme = Some(value.to_string());
            }
        }
        other => {
            log::warn!("glassy: ignoring unknown config key '{other}'");
        }
    }
    Ok(())
}

/// Parse a boolean for a named field, accepting the usual spellings.
pub(super) fn parse_bool(value: &str, field: &str) -> Result<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" => Ok(false),
        _ => bail!("{field} must be true/false (or yes/no, on/off, 1/0), got '{value}'"),
    }
}

/// Parse a strictly-positive float for a named field.
pub(super) fn parse_pos_f32(value: &str, field: &str) -> Result<f32> {
    let v: f32 = value
        .parse()
        .with_context(|| format!("{field}: invalid number '{value}'"))?;
    if !(v.is_finite() && v > 0.0) {
        bail!("{field} must be a positive number, got {value}");
    }
    Ok(v)
}

/// Split a `shell` value (a whitespace-separated program + args) into a `Shell`.
/// Returns `None` for an empty value.
pub(super) fn parse_shell(value: &str) -> Option<Shell> {
    let mut parts = value.split_whitespace();
    let program = parts.next()?.to_string();
    let args = parts.map(str::to_string).collect();
    Some(Shell::new(program, args))
}
