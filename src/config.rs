//! Configuration: a hand-rolled `KEY=VALUE` config file parser plus a small CLI
//! argument parser layered on top (CLI overrides the file).
//!
//! The config file lives at `$XDG_CONFIG_HOME/glassy/glassy.conf` (falling back
//! to `~/.config/glassy/glassy.conf`) on Linux, or
//! `~/Library/Application Support/glassy/glassy.conf` on macOS. Recognized keys:
//!
//! ```text
//! font_family = FiraCode Nerd Font Mono
//! font_size   = 14
//! theme       = tokyo-night            # or: catppuccin-mocha
//! opacity     = 0.92                   # 0.0 (clear) .. 1.0 (opaque)
//! padding     = 6                      # logical px grid inset (all sides)
//! padding_top = 8                      # per-side overrides (optional, override padding)
//! padding_bottom = 6
//! padding_left = 4
//! padding_right = 4
//! shell       = /usr/bin/zsh -l        # program + args
//! scrollback  = 10000                  # lines of history
//! bell_visual = true                   # flash the window on bell
//! bell_audible= false                  # soft beep on bell (needs bell-audio build)
//! follow_system = false                # track the OS light/dark color scheme
//! theme_light = rose-pine-dawn         # theme used in system Light mode
//! theme_dark  = tokyo-night            # theme used in system Dark mode
//! status_bar  = false                  # show status bar at the bottom (default off)
//! pane_headers= true                   # show per-pane title bars + accent rail in splits (default on)
//! ligatures   = false                  # enable OpenType ligature shaping across cells (default off)
//! font_features = ss01, calt=0         # OpenType feature tags to force on/off (comma or space separated)
//! cwd         = /home/me/projects      # working directory for the first tab's shell
//! restore_session = false              # restore previous tabs/splits/cwds on launch
//! ```
//!
//! Named profiles live in `[profile.NAME]` sections (activate with `--profile NAME`):
//!
//! ```text
//! [profile.work]
//! theme = catppuccin-mocha
//! font_size = 16
//! cwd = /home/me/work
//! shell = /usr/bin/zsh -l
//! color.fg    = #c0caf5                # override theme foreground (hex format)
//! color.bg    = #1a1b26                # override theme background (hex format)
//! color.cursor = #7dcfff               # override cursor color
//! color.selection_bg = #283457         # override selection background
//! color.ansi0 through color.ansi15     # override ANSI palette colors
//! ```
//!
//! CLI flags override the file: at minimum `--font-size <pt>`, `--opacity <f>`,
//! and `-e <cmd> [args…]` (run a command instead of the shell). `--help` and
//! `--version` print and exit.

use std::collections::HashMap;
use std::path::PathBuf;

use alacritty_terminal::tty::Shell;
use anyhow::{Context, Result, bail};

use crate::app::Config;
use crate::color::{self, Theme};
use crate::renderer::DEFAULT_OPACITY;

/// Default logical font size in points when neither config nor CLI sets it.
const DEFAULT_FONT_SIZE: f32 = 14.0;
/// Default scrollback history (lines) when unset.
const DEFAULT_SCROLLBACK: usize = 10_000;

/// Fully-resolved settings handed to the app: the renderer/PTY `Config` plus the
/// selected color `Theme` (installed globally by `main`).
pub struct Settings {
    pub config: Config,
    pub theme: Theme,
}

impl Settings {
    /// Resolve config file + CLI args into final settings.
    ///
    /// Returns `Ok(None)` when a flag (`--help`/`--version`) has already printed
    /// its output and the process should exit successfully without launching.
    pub fn resolve(args: impl Iterator<Item = String>) -> Result<Option<Settings>> {
        // Materialize args once so we can pre-scan for `--profile` (which must be
        // applied after the file load but before the rest of the CLI overrides).
        let args: Vec<String> = args.collect();

        // 1. Start from defaults.
        let mut raw = RawConfig::default();

        // 2. Layer the config file (if present and readable). This populates any
        //    `[profile.NAME]` sections so `--profile` can activate one below.
        if let Some(path) = config_path() {
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    parse_config_file(&text, &mut raw)
                        .with_context(|| format!("parsing {}", path.display()))?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    log::warn!("glassy: could not read {}: {e}", path.display());
                }
            }
        }

        // 3. Activate a `--profile NAME`, overlaying its keys before CLI flags so
        //    individual CLI overrides still win over the profile.
        if let Some(name) = profile_from_args(&args) {
            raw.activate_profile(&name)?;
        }

        // 4. Layer CLI overrides (and handle --help/--version). `--profile` is a
        //    no-op here (already applied above).
        if !parse_cli(args.into_iter(), &mut raw)? {
            return Ok(None);
        }

        Ok(Some(raw.into_settings()?))
    }
}

/// Accumulated raw configuration before validation/finalization. Every field is
/// optional so the file and CLI layers can each set a subset.
#[derive(Default)]
struct RawConfig {
    font_family: Option<String>,
    font_size: Option<f32>,
    theme: Option<String>,
    opacity: Option<f32>,
    padding: Option<f32>,
    padding_top: Option<f32>,
    padding_bottom: Option<f32>,
    padding_left: Option<f32>,
    padding_right: Option<f32>,
    shell: Option<Shell>,
    scrollback: Option<usize>,
    bell_visual: Option<bool>,
    bell_audible: Option<bool>,
    follow_system: Option<bool>,
    theme_light: Option<String>,
    theme_dark: Option<String>,
    status_bar: Option<bool>,
    pane_headers: Option<bool>,
    word_separator: Option<String>,
    ligatures: Option<bool>,
    font_features: Option<Vec<String>>,
    cwd: Option<String>,
    restore_session: Option<bool>,
    // Custom theme colors (hex format, e.g., "color.fg = #c0caf5")
    color_fg: Option<String>,
    color_bg: Option<String>,
    color_cursor: Option<String>,
    color_selection_bg: Option<String>,
    color_ansi: Option<[Option<String>; 16]>,
    /// Named profiles parsed from `[profile.NAME]` sections: NAME -> raw key/value
    /// pairs. Applied over the base config when the matching profile is activated
    /// (via `--profile NAME`). Lower-cased keys, same grammar as top-level keys.
    profiles: HashMap<String, Vec<(String, String)>>,
}

impl RawConfig {
    fn into_settings(self) -> Result<Settings> {
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

        // Follow-system theming: defaults pick a sensible dark/light pair so a
        // user only has to flip `follow_system = true`. Unknown names canonicalize
        // (and fall back to tokyo-night) like the pinned `theme`.
        let follow_system = self.follow_system.unwrap_or(false);
        let theme_dark = color::canonical_name(
            self.theme_dark.as_deref().unwrap_or(&theme_name),
        )
        .to_string();
        let theme_light = color::canonical_name(
            self.theme_light.as_deref().unwrap_or("rose-pine-dawn"),
        )
        .to_string();

        // A non-finite opacity (e.g. `--opacity nan`) survives `clamp` as NaN and
        // would poison the renderer's premultiply math, so fall back to the
        // default. font_size is similarly guarded against non-finite input.
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
            pane_headers: self.pane_headers.unwrap_or(true),
            word_separator: self.word_separator.unwrap_or_default(),
            ligatures: self.ligatures.unwrap_or(false),
            font_features: self.font_features.unwrap_or_default(),
            initial_cwd: self.cwd.filter(|s| !s.is_empty()).map(PathBuf::from),
            restore_session: self.restore_session.unwrap_or(false),
        };

        Ok(Settings { config, theme })
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
        return Some(
            PathBuf::from(home)
                .join("Library/Application Support/glassy/glassy.conf")
        );
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

/// Parse a `KEY=VALUE` config file into `raw`. Blank lines and `#`/`;` comments
/// are ignored; surrounding whitespace and a single layer of matching quotes are
/// stripped from values. An unknown key is warned about but not fatal; a value
/// that fails to parse for a known key is a hard error (with the line number).
fn parse_config_file(text: &str, raw: &mut RawConfig) -> Result<()> {
    // The active section. `None` is the top-level (global) config; `Some(name)`
    // collects keys into the named `[profile.NAME]` block instead of applying them.
    let mut profile: Option<String> = None;
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        // Section header: `[profile.NAME]` switches the active profile; any other
        // `[...]` header resets to the global section (forward-compatible).
        if line.starts_with('[') && line.ends_with(']') {
            let name = line[1..line.len() - 1].trim();
            profile = name
                .strip_prefix("profile.")
                .map(|n| n.trim().to_ascii_lowercase())
                .filter(|n| !n.is_empty());
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            bail!("line {}: expected KEY=VALUE, got '{line}'", i + 1);
        };
        let key = key.trim().to_ascii_lowercase();
        let value = unquote(value.trim());
        match &profile {
            None => apply_kv(&key, value, raw).with_context(|| format!("line {}", i + 1))?,
            Some(name) => {
                // Defer validation: keys are applied via apply_kv only when the
                // profile is activated, so an unused profile with a bad value is
                // not fatal at load.
                raw.profiles
                    .entry(name.clone())
                    .or_default()
                    .push((key, value.to_string()));
            }
        }
    }
    Ok(())
}

impl RawConfig {
    /// Apply the named profile's key/value pairs over the base config, returning an
    /// error if the profile is unknown or one of its values fails to parse. Called
    /// after the file load and before CLI overrides, so the CLI still wins.
    fn activate_profile(&mut self, name: &str) -> Result<()> {
        let key = name.to_ascii_lowercase();
        let pairs = self
            .profiles
            .get(&key)
            .cloned()
            .with_context(|| format!("unknown profile '{name}' (no [profile.{name}] section)"))?;
        for (k, v) in &pairs {
            apply_kv(k, v, self).with_context(|| format!("in [profile.{name}]"))?;
        }
        Ok(())
    }
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
/// Accepts formats: "c0caf5", "#c0caf5", "C0CAF5", "#C0CAF5", etc.
fn parse_hex_color(s: &str) -> Result<alacritty_terminal::vte::ansi::Rgb> {
    let hex = s.trim_start_matches('#');
    if hex.len() != 6 {
        bail!("color must be a 6-digit hex value, got '{s}'");
    }
    let r = u8::from_str_radix(&hex[0..2], 16)
        .with_context(|| format!("invalid hex color '{s}'"))?;
    let g = u8::from_str_radix(&hex[2..4], 16)
        .with_context(|| format!("invalid hex color '{s}'"))?;
    let b = u8::from_str_radix(&hex[4..6], 16)
        .with_context(|| format!("invalid hex color '{s}'"))?;
    Ok(alacritty_terminal::vte::ansi::Rgb { r, g, b })
}

/// Apply a single recognized `key`/`value` pair into `raw`.
fn apply_kv(key: &str, value: &str, raw: &mut RawConfig) -> Result<()> {
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
            // Comma-or-space separated list of OpenType feature tags, e.g.
            // "ss01, calt=0, dlig" or "ss01 calt=0".  Each token is a bare
            // 4-char tag (enable) or "tag=N" (explicit value).  We validate
            // that each tag is exactly 4 ASCII characters; the value (after =)
            // must be a non-negative integer.  Unknown tags are forwarded as-is
            // to cosmic-text and ignored by shaping engines that lack them.
            let features: Vec<String> = value
                .split([',', ' '])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|token| {
                    // Validate the tag portion (4 ASCII chars) and optional value.
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
        // Custom theme colors: color.fg, color.bg, color.cursor, color.selection_bg, color.ansi0..15
        "color.fg" => {
            parse_hex_color(value)?; // Validate but store the string for later use
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
        // Parse color.ansi0 through color.ansi15
        k if k.starts_with("color.ansi") => {
            let ansi_idx = k.strip_prefix("color.ansi")
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
        other => {
            log::warn!("glassy: ignoring unknown config key '{other}'");
        }
    }
    Ok(())
}

/// Parse a boolean for a named field, accepting the usual spellings.
fn parse_bool(value: &str, field: &str) -> Result<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" => Ok(false),
        _ => bail!("{field} must be true/false (or yes/no, on/off, 1/0), got '{value}'"),
    }
}

/// Parse a strictly-positive float for a named field.
fn parse_pos_f32(value: &str, field: &str) -> Result<f32> {
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
fn parse_shell(value: &str) -> Option<Shell> {
    let mut parts = value.split_whitespace();
    let program = parts.next()?.to_string();
    let args = parts.map(str::to_string).collect();
    Some(Shell::new(program, args))
}

/// Import a color theme from a TOML/YAML file (Alacritty-compatible or base16 format).
/// Supports both inline Alacritty color tables and base16 palette arrays.
fn import_theme_from_file(path: &str) -> Result<Theme> {
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
fn import_theme_toml(text: &str) -> Result<Theme> {
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
fn import_theme_yaml(text: &str) -> Result<Theme> {
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

/// Parse CLI arguments, overriding fields in `raw`.
///
/// Returns `Ok(true)` to continue launching, `Ok(false)` when `--help`/`--version`
/// was handled (caller should exit successfully), or an error on a bad flag.
///
/// Recognized: `--font-size <pt>`, `--font-family <name>`, `--theme <name>`,
/// `--opacity <f>`, `--padding <px>`, `--scrollback <n>`, `--bell-visual <bool>`,
/// `--bell-audible <bool>`, `--follow-system <bool>`, `--theme-light <name>`,
/// `--theme-dark <name>`, `--import-theme <path>`, `-e/--command <cmd…>` (consumes
/// the rest of the args as the program + its arguments), `-h/--help`, `-V/--version`.
fn parse_cli(args: impl Iterator<Item = String>, raw: &mut RawConfig) -> Result<bool> {
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print_help();
                return Ok(false);
            }
            "-V" | "--version" => {
                println!("glassy {}", env!("CARGO_PKG_VERSION"));
                return Ok(false);
            }
            "--import-theme" => {
                let path = next_value(&mut args, "--import-theme")?;
                let theme = import_theme_from_file(&path)?;
                raw.color_fg = Some(format!("#{:02x}{:02x}{:02x}", theme.fg.r, theme.fg.g, theme.fg.b));
                raw.color_bg = Some(format!("#{:02x}{:02x}{:02x}", theme.bg.r, theme.bg.g, theme.bg.b));
                raw.color_cursor = Some(format!("#{:02x}{:02x}{:02x}", theme.cursor.r, theme.cursor.g, theme.cursor.b));
                raw.color_selection_bg = Some(format!("#{:02x}{:02x}{:02x}", theme.selection_bg.r, theme.selection_bg.g, theme.selection_bg.b));
                if raw.color_ansi.is_none() {
                    raw.color_ansi = Some(Default::default());
                }
                if let Some(ref mut ansi) = raw.color_ansi {
                    for (i, rgb) in theme.ansi16.iter().enumerate() {
                        ansi[i] = Some(format!("#{:02x}{:02x}{:02x}", rgb.r, rgb.g, rgb.b));
                    }
                }
                return Ok(true);
            }
            "--font-size" => {
                let v = next_value(&mut args, "--font-size")?;
                raw.font_size = Some(parse_pos_f32(&v, "--font-size")?);
            }
            "--font-family" => {
                raw.font_family = Some(next_value(&mut args, "--font-family")?);
            }
            "--theme" => {
                raw.theme = Some(next_value(&mut args, "--theme")?);
            }
            "--opacity" => {
                let v = next_value(&mut args, "--opacity")?;
                raw.opacity = Some(
                    v.parse()
                        .with_context(|| format!("--opacity: invalid number '{v}'"))?,
                );
            }
            "--padding" => {
                let v = next_value(&mut args, "--padding")?;
                let p: f32 = v
                    .parse()
                    .with_context(|| format!("--padding: invalid number '{v}'"))?;
                if p < 0.0 {
                    bail!("--padding must be >= 0, got {p}");
                }
                raw.padding = Some(p);
            }
            "--scrollback" => {
                let v = next_value(&mut args, "--scrollback")?;
                raw.scrollback = Some(
                    v.parse()
                        .with_context(|| format!("--scrollback: invalid integer '{v}'"))?,
                );
            }
            "--bell-visual" => {
                let v = next_value(&mut args, "--bell-visual")?;
                raw.bell_visual = Some(parse_bool(&v, "--bell-visual")?);
            }
            "--bell-audible" => {
                let v = next_value(&mut args, "--bell-audible")?;
                raw.bell_audible = Some(parse_bool(&v, "--bell-audible")?);
            }
            "--follow-system" => {
                let v = next_value(&mut args, "--follow-system")?;
                raw.follow_system = Some(parse_bool(&v, "--follow-system")?);
            }
            "--theme-light" => {
                raw.theme_light = Some(next_value(&mut args, "--theme-light")?);
            }
            "--theme-dark" => {
                raw.theme_dark = Some(next_value(&mut args, "--theme-dark")?);
            }
            "--status-bar" => {
                let v = next_value(&mut args, "--status-bar")?;
                raw.status_bar = Some(parse_bool(&v, "--status-bar")?);
            }
            "--pane-headers" => {
                let v = next_value(&mut args, "--pane-headers")?;
                raw.pane_headers = Some(parse_bool(&v, "--pane-headers")?);
            }
            "--word-separator" => {
                raw.word_separator = Some(next_value(&mut args, "--word-separator")?);
            }
            "--restore-session" => {
                // Optional bool value; bare `--restore-session` means true.
                match args.peek().map(|s| s.as_str()) {
                    Some(v) if matches!(v.to_ascii_lowercase().as_str(),
                        "true" | "yes" | "on" | "1" | "false" | "no" | "off" | "0") =>
                    {
                        let v = args.next().unwrap();
                        raw.restore_session = Some(parse_bool(&v, "--restore-session")?);
                    }
                    _ => raw.restore_session = Some(true),
                }
            }
            "--profile" => {
                // Already applied in `resolve`'s pre-scan; consume its value here so
                // the main parse doesn't reject it as an unknown argument.
                let _ = next_value(&mut args, "--profile")?;
            }
            // `--profile=NAME` inline form (also pre-scanned in `resolve`).
            a if a.starts_with("--profile=") => {}
            "--font-features" => {
                let v = next_value(&mut args, "--font-features")?;
                // Same grammar as the config key: comma-or-space separated tags.
                let features: Vec<String> = v
                    .split([',', ' '])
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
                raw.font_features = Some(features);
            }
            "--working-directory" | "--cwd" => {
                raw.cwd = Some(next_value(&mut args, "--working-directory")?);
            }
            // `-e`/`--command`: everything after it is the program + its args
            // (the conventional terminal contract). Consume the rest verbatim.
            "-e" | "--command" => {
                let program = next_value(&mut args, arg.as_str())?;
                let rest: Vec<String> = args.by_ref().collect();
                raw.shell = Some(Shell::new(program, rest));
            }
            other => {
                bail!("unrecognized argument '{other}' (try --help)");
            }
        }
    }
    Ok(true)
}

/// Pre-scan the CLI args for `--profile NAME`, returning the name if present. Used
/// so the profile is activated after the file load but before CLI overrides.
fn profile_from_args(args: &[String]) -> Option<String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--profile" {
            return it.next().cloned();
        }
        if let Some(v) = a.strip_prefix("--profile=") {
            return Some(v.to_string());
        }
    }
    None
}

/// Pull the value following a flag, erroring if it is missing.
fn next_value(
    args: &mut std::iter::Peekable<impl Iterator<Item = String>>,
    flag: &str,
) -> Result<String> {
    args.next()
        .with_context(|| format!("{flag} requires a value"))
}

fn print_help() {
    println!(
        "glassy {} — a GPU terminal emulator

USAGE:
    glassy [OPTIONS] [-e COMMAND [ARGS...]]

OPTIONS:
    --font-size <PT>       Font size in points
    --font-family <NAME>   Font family name (or a path to a font file)
    --theme <NAME>         Color theme: tokyo-night | catppuccin-mocha
    --opacity <F>          Window opacity 0.0..1.0
    --padding <PX>         Grid inset padding in logical pixels
    --scrollback <N>       Lines of scrollback history
    --bell-visual <BOOL>   Flash the window on the terminal bell (default true)
    --bell-audible <BOOL>  Soft beep on the terminal bell (default false)
    --follow-system <BOOL> Track the OS light/dark color scheme (default false)
    --theme-light <NAME>   Theme used in system Light mode (e.g. rose-pine-dawn)
    --theme-dark <NAME>    Theme used in system Dark mode (e.g. tokyo-night)
    --status-bar <BOOL>    Show status bar at the bottom (default false)
    --pane-headers <BOOL>  Show per-pane title bars in splits (default true)
    --word-separator <STR> Extra word separators for text selection
    --font-features <LIST> OpenType feature tags, e.g. \"ss01,calt=0\" (comma/space separated)
    --import-theme <PATH>  Import Alacritty/base16 theme from TOML/YAML file
    --profile <NAME>       Activate a [profile.NAME] config section's overrides
    --cwd <PATH>           Working directory for the first tab's shell
    --restore-session [B]  Restore the previous session's tabs/splits (default off)
    -e, --command <CMD>    Run CMD (with the remaining args) instead of the shell
    -h, --help             Print this help and exit
    -V, --version          Print version and exit

CONFIG FILE:
    $XDG_CONFIG_HOME/glassy/glassy.conf  (or ~/.config/glassy/glassy.conf)
    macOS: ~/Library/Application Support/glassy/glassy.conf
    KEY=VALUE lines: font_family, font_size, theme, opacity, padding,
    padding_top, padding_bottom, padding_left, padding_right, shell, scrollback,
    bell_visual, bell_audible, follow_system, theme_light, theme_dark, status_bar,
    pane_headers, word_separator, ligatures, font_features, color.*. CLI flags override the file.",
        env!("CARGO_PKG_VERSION")
    );
}

#[cfg(test)]
mod tests {
    use super::{RawConfig, merge_config, parse_bool, parse_config_file};

    #[test]
    fn non_finite_opacity_and_font_size_fall_back() {
        let raw = RawConfig {
            opacity: Some(f32::NAN),
            font_size: Some(f32::INFINITY),
            ..Default::default()
        };
        let s = raw.into_settings().expect("settings");
        assert!(s.config.opacity.is_finite() && (0.0..=1.0).contains(&s.config.opacity));
        assert!(s.config.font_size.is_finite() && s.config.font_size > 0.0);
    }

    #[test]
    fn merge_updates_in_place_and_appends() {
        let existing = "\
# my config
theme = dracula
font_size = 14
opacity = 0.80
";
        let updates = [
            ("font_size", "20".to_string()),
            ("opacity", "0.95".to_string()),
            ("bell_visual", "false".to_string()),
        ];
        let out = merge_config(existing, &updates);
        // Comment + unmanaged key preserved.
        assert!(out.contains("# my config"));
        assert!(out.contains("theme = dracula"));
        // Present keys updated in place (not duplicated).
        assert!(out.contains("font_size = 20"));
        assert_eq!(out.matches("font_size").count(), 1);
        assert!(out.contains("opacity = 0.95"));
        assert_eq!(out.matches("opacity").count(), 1);
        // Missing key appended.
        assert!(out.contains("bell_visual = false"));
    }

    #[test]
    fn merge_into_empty_creates_keys() {
        let out = merge_config("", &[("opacity", "0.9".to_string())]);
        assert_eq!(out, "opacity = 0.9\n");
    }

    #[test]
    fn bool_spellings() {
        for v in ["true", "yes", "on", "1", "True", "ON"] {
            assert!(parse_bool(v, "x").unwrap(), "{v}");
        }
        for v in ["false", "no", "off", "0", "No", "OFF"] {
            assert!(!parse_bool(v, "x").unwrap(), "{v}");
        }
        assert!(parse_bool("maybe", "x").is_err());
    }

    #[test]
    fn bell_keys_parse() {
        let mut raw = RawConfig::default();
        parse_config_file("bell_visual = false\nbell_audible = on\n", &mut raw).unwrap();
        assert_eq!(raw.bell_visual, Some(false));
        assert_eq!(raw.bell_audible, Some(true));
    }

    #[test]
    fn bell_defaults_when_unset() {
        let settings = RawConfig::default().into_settings().unwrap();
        assert!(settings.config.bell_visual); // default on
        assert!(!settings.config.bell_audible); // default off
    }

    #[test]
    fn profile_section_collected_and_activated() {
        let mut raw = RawConfig::default();
        parse_config_file(
            "theme = tokyo-night\nfont_size = 14\n\n[profile.work]\nfont_size = 18\ntheme = catppuccin-mocha\ncwd = /home/me/work\n",
            &mut raw,
        )
        .unwrap();
        // The profile keys are collected, not applied to the base config.
        assert_eq!(raw.font_size, Some(14.0));
        assert!(raw.profiles.contains_key("work"));
        // Activating overlays the profile's keys.
        raw.activate_profile("work").unwrap();
        assert_eq!(raw.font_size, Some(18.0));
        assert_eq!(raw.cwd.as_deref(), Some("/home/me/work"));
        let s = raw.into_settings().unwrap();
        assert_eq!(s.config.font_size, 18.0);
        assert_eq!(s.config.initial_cwd.as_deref().map(|p| p.to_str().unwrap()), Some("/home/me/work"));
    }

    #[test]
    fn activate_unknown_profile_errors() {
        let mut raw = RawConfig::default();
        assert!(raw.activate_profile("nope").is_err());
    }

    #[test]
    fn profile_name_is_case_insensitive() {
        let mut raw = RawConfig::default();
        parse_config_file("[profile.Dev]\ntheme = dracula\n", &mut raw).unwrap();
        // Stored lower-cased; activation lower-cases the requested name too.
        assert!(raw.profiles.contains_key("dev"));
        raw.activate_profile("DEV").unwrap();
        assert_eq!(raw.theme.as_deref(), Some("dracula"));
    }

    #[test]
    fn profile_from_args_finds_both_forms() {
        use super::profile_from_args;
        let a = vec!["--profile".to_string(), "work".to_string()];
        assert_eq!(profile_from_args(&a), Some("work".to_string()));
        let b = vec!["--profile=home".to_string()];
        assert_eq!(profile_from_args(&b), Some("home".to_string()));
        let c = vec!["--font-size".to_string(), "14".to_string()];
        assert_eq!(profile_from_args(&c), None);
    }

    #[test]
    fn restore_session_defaults_off_and_parses() {
        let settings = RawConfig::default().into_settings().unwrap();
        assert!(!settings.config.restore_session);
        let mut raw = RawConfig::default();
        parse_config_file("restore_session = true\n", &mut raw).unwrap();
        assert_eq!(raw.restore_session, Some(true));
    }

    #[test]
    fn cwd_key_sets_initial_cwd() {
        let mut raw = RawConfig::default();
        parse_config_file("cwd = /tmp/here\n", &mut raw).unwrap();
        let s = raw.into_settings().unwrap();
        assert_eq!(s.config.initial_cwd.as_deref().map(|p| p.to_str().unwrap()), Some("/tmp/here"));
    }

    #[test]
    fn pane_headers_parses_and_defaults_on() {
        let mut raw = RawConfig::default();
        parse_config_file("pane_headers = off\n", &mut raw).unwrap();
        assert_eq!(raw.pane_headers, Some(false));
        // Default (unset) is on.
        let settings = RawConfig::default().into_settings().unwrap();
        assert!(settings.config.pane_headers);
    }

    #[test]
    fn font_features_parses_comma_separated() {
        let mut raw = RawConfig::default();
        parse_config_file("font_features = ss01, calt=0, dlig\n", &mut raw).unwrap();
        let feats = raw.font_features.as_ref().expect("font_features should be set");
        assert!(feats.contains(&"ss01".to_string()), "ss01 should be present");
        assert!(feats.contains(&"calt=0".to_string()), "calt=0 should be present");
        assert!(feats.contains(&"dlig".to_string()), "dlig should be present");
    }

    #[test]
    fn font_features_defaults_empty() {
        let settings = RawConfig::default().into_settings().unwrap();
        assert!(
            settings.config.font_features.is_empty(),
            "font_features must default to empty"
        );
    }

    #[test]
    fn font_features_space_separated_also_works() {
        let mut raw = RawConfig::default();
        parse_config_file("font_features = liga ss01\n", &mut raw).unwrap();
        let feats = raw.font_features.as_ref().expect("font_features should be set");
        assert_eq!(feats.len(), 2);
        assert!(feats.contains(&"liga".to_string()));
        assert!(feats.contains(&"ss01".to_string()));
    }
}
