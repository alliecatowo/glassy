//! Configuration file parsing: RawConfig accumulation, file I/O, and value parsing.

use alacritty_terminal::tty::Shell;
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use crate::app::Config;
use crate::color;
use crate::renderer::DEFAULT_OPACITY;

use super::keymap::{build_keymap, default_keymap};
use super::platform::Platform;

const DEFAULT_FONT_SIZE: f32 = 14.0;
const DEFAULT_SCROLLBACK: usize = 10_000;
/// Upper clamp for the `scrollback` config value (lines). Lowered from an
/// earlier 1,000,000 per the w15 alacritty-surface audit: alacritty's scrollback
/// storage is an uncompressed `Vec<Row<Cell>>` at an estimated ~24 bytes/cell
/// with no disk overflow, so 1,000,000 lines at a common 200-column width was a
/// worst case of ~4.8 GB resident for a single pane. 200,000 lines (~960 MB
/// worst case at 200 cols) is still generous — most terminals default in the
/// low thousands — while capping the accidental-gigabytes-per-pane failure mode.
/// Does not affect `DEFAULT_SCROLLBACK` (10,000); only configs that explicitly
/// requested a very large value are affected.
const SCROLLBACK_MAX: usize = 200_000;
/// Default for `scrollback_background_cap`: `0` disables the background-
/// scrollback-bounding policy outright (see `crate::pty::ScrollbackBackgroundPolicy`),
/// so out of the box every pane keeps its full configured `scrollback` resident
/// regardless of focus — this Phase-1 rec must not reduce anyone's default
/// visible scrollback.
const DEFAULT_SCROLLBACK_BACKGROUND_CAP: usize = 0;
/// Default for `scrollback_background_idle_secs`: how long (seconds) a pane must
/// be idle/backgrounded before `scrollback_background_cap` takes effect, once
/// that cap is non-zero. 15 minutes — long enough that a pane the user merely
/// glanced away from briefly is never trimmed.
const DEFAULT_SCROLLBACK_BACKGROUND_IDLE_SECS: u64 = 900;
/// Default number of recently-run commands retained for the command palette's
/// history source (OSC 133 `B`..`C` capture). 0 disables it.
const DEFAULT_COMMAND_HISTORY: usize = 200;
/// Default minimum command duration (ms) that triggers a command-finish desktop
/// notification when the window is unfocused. 10 s avoids spamming for quick
/// commands while still catching long builds/tests.
const DEFAULT_NOTIFY_COMMAND_THRESHOLD_MS: u64 = 10_000;
/// Clamp bounds for `quake_height` (fraction of the monitor height the quake
/// window occupies). Shared by `apply_kv`, `RawConfig::into_settings`'s default
/// fallback, the CLI flag parser (`config::cli`), and the settings-form slider
/// (`gui::settings_panel`) so all four enforce the identical range.
pub(crate) const QUAKE_HEIGHT_MIN: f32 = 0.1;
pub(crate) const QUAKE_HEIGHT_MAX: f32 = 1.0;
/// Clamp bound for `quake_animation_ms` (the quake slide duration). Shared by
/// `apply_kv`, `RawConfig::into_settings`'s default fallback, the CLI flag
/// parser, and the settings-form stepper.
pub(crate) const QUAKE_ANIMATION_MS_MAX: u64 = 5_000;
/// Clamp bound for `notify_command_threshold_ms` (24 hours). Shared by
/// `apply_kv` and the settings-form stepper so both enforce the identical cap.
pub(crate) const NOTIFY_COMMAND_THRESHOLD_MS_MAX: u64 = 86_400_000;

/// A single entry in the `font_symbol_map` config key: a Unicode range mapped
/// to a specific font family. The shaper routes codepoints in `[start, end]`
/// (inclusive) to the named family instead of the primary font.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolMapEntry {
    /// First codepoint of the range (inclusive).
    pub start: u32,
    /// Last codepoint of the range (inclusive; equal to `start` for a single
    /// codepoint mapping).
    pub end: u32,
    /// Font family name (or absolute file path) to route this range to.
    pub family: String,
}

/// Parse the `font_symbol_map` value into a `Vec<SymbolMapEntry>`.
///
/// Each entry is `"RANGE:Family"` where RANGE is `U+XXXX` or `U+XXXX-U+YYYY`
/// (hex codepoints). Multiple entries are separated by commas. Entries that
/// cannot be parsed are logged at warn level and skipped.
///
/// Examples:
/// ```text
/// U+E000-U+F8FF : Symbols Nerd Font Mono
/// U+2500-U+257F : FiraCode Nerd Font Mono, U+1F600-U+1F64F : Noto Color Emoji
/// ```
pub fn parse_symbol_map(value: &str) -> Vec<SymbolMapEntry> {
    let mut out = Vec::new();
    // Split on commas first (entries), then parse each.
    for entry in value.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        // The colon separator between range and family. Use rfind so a family
        // name containing ':' (unlikely but safe) doesn't confuse the split.
        // Actually we need the FIRST colon that appears after a codepoint range.
        // Ranges look like "U+XXXX" or "U+XXXX-U+YYYY" — find the colon after
        // the range token.
        //
        // Strategy: split on ':' and reconstruct family from tail parts.
        let parts: Vec<&str> = entry.splitn(2, ':').collect();
        if parts.len() != 2 {
            log::warn!("glassy: font_symbol_map: expected 'RANGE:Family', got '{entry}'; skipping");
            continue;
        }
        let range_str = parts[0].trim();
        let family = parts[1].trim().to_string();
        if family.is_empty() {
            log::warn!("glassy: font_symbol_map: empty family in '{entry}'; skipping");
            continue;
        }
        // Parse "U+XXXX" or "U+XXXX-U+YYYY".
        let (start, end) = if let Some((lo, hi)) = range_str.split_once('-') {
            // "U+XXXX-U+YYYY" — note the split_once('-') will split at the
            // first dash, which could be the dash between U+XXXX and U+YYYY.
            // But "U+E000-U+F8FF" splits as lo="U+E000" hi="U+F8FF". Correct.
            let s = parse_codepoint(lo.trim());
            let e = parse_codepoint(hi.trim());
            match (s, e) {
                (Some(s), Some(e)) => (s, e.max(s)),
                _ => {
                    log::warn!("glassy: font_symbol_map: invalid range '{range_str}'; skipping");
                    continue;
                }
            }
        } else {
            match parse_codepoint(range_str) {
                Some(cp) => (cp, cp),
                None => {
                    log::warn!(
                        "glassy: font_symbol_map: invalid codepoint '{range_str}'; skipping"
                    );
                    continue;
                }
            }
        };
        out.push(SymbolMapEntry { start, end, family });
    }
    out
}

/// Parse a `U+XXXX` (or bare `XXXX` hex) codepoint string to a `u32`.
fn parse_codepoint(s: &str) -> Option<u32> {
    let hex = s
        .strip_prefix("U+")
        .or_else(|| s.strip_prefix("u+"))
        .unwrap_or(s);
    u32::from_str_radix(hex.trim(), 16).ok()
}

/// Parse a `font_variations` value into a `Vec<String>` of `"axis=value"` entries.
///
/// Accepts comma or space separation. Each token is either:
///   - `"axis=value"` (e.g. `"wght=450"`, `"wdth=75"`)
///   - A bare 4-char tag (treated as "enable", same as feature tags) — not
///     meaningful for axes; warned and kept for forward-compat.
pub fn parse_font_variations(value: &str) -> Vec<String> {
    value
        .split([',', ' '])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|token| {
            let tag = token.split('=').next().unwrap_or(token).trim();
            if tag.len() != 4 || !tag.is_ascii() {
                log::warn!(
                    "glassy: font_variations: axis tag '{tag}' is not 4 ASCII chars; skipping"
                );
            }
            token.to_string()
        })
        .collect()
}

/// Parse a `status_bar_segments` value: a comma- or space-separated list of
/// segment tokens. Unknown tokens are warned about and dropped (never a hard
/// error — a stray typo shouldn't block startup). Shared by `apply_kv` (the
/// config-file path) and the live settings-form "Status bar segments" text
/// field (`App::commit_settings_field` in `settings_fields.rs`) so both apply
/// the identical token set — this function IS the authoritative parser both
/// callers of.
pub(crate) fn parse_status_bar_segments(value: &str) -> Vec<crate::app::StatusBarSegment> {
    use crate::app::StatusBarSegment;
    value
        .split([',', ' '])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| match s.to_ascii_lowercase().as_str() {
            "cwd" => Some(StatusBarSegment::Cwd),
            "git_branch" | "git" => Some(StatusBarSegment::GitBranch),
            "process" | "fg_process" => Some(StatusBarSegment::Process),
            "time" | "clock" => Some(StatusBarSegment::Time),
            "mode" => Some(StatusBarSegment::Mode),
            "broadcast" | "bcast" => Some(StatusBarSegment::Broadcast),
            "selection" | "sel" => Some(StatusBarSegment::Selection),
            "scroll" => Some(StatusBarSegment::Scroll),
            "encoding" | "enc" => Some(StatusBarSegment::Encoding),
            "progress" => Some(StatusBarSegment::Progress),
            "exit_status" | "exit" => Some(StatusBarSegment::ExitStatus),
            "key_hints" | "hints" => Some(StatusBarSegment::KeyHints),
            // w15 additions — see status-bar.md recs 1/3/4.
            "tab_count" | "tabs" => Some(StatusBarSegment::TabCount),
            "zoom" => Some(StatusBarSegment::Zoom),
            "profile" => Some(StatusBarSegment::Profile),
            "busy" => Some(StatusBarSegment::Busy),
            "hostname" | "host" => Some(StatusBarSegment::Hostname),
            "custom" => Some(StatusBarSegment::Custom),
            other => {
                log::warn!("glassy: ignoring unknown status_bar_segments entry '{other}'");
                None
            }
        })
        .collect()
}

/// Normalize a raw `hints_chars` string to the filtered alphabet `apply_kv`'s
/// finalization step derives: only ASCII letters are kept, and an alphabet
/// shorter than 2 chars is rejected (falls back to the built-in default by
/// returning `None`). Shared by [`RawConfig::into_settings`] (the config-file
/// path) and the live settings-form "Hint chars" text field so both apply the
/// identical rule.
pub(crate) fn normalize_hints_chars(s: &str) -> Option<String> {
    let filtered: String = s.chars().filter(|c| c.is_ascii_alphabetic()).collect();
    if filtered.chars().count() >= 2 {
        Some(filtered)
    } else {
        None
    }
}

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
    pub hints_chars: Option<String>,
    pub command_badges: Option<bool>,
    pub color_fg: Option<String>,
    pub color_bg: Option<String>,
    pub color_cursor: Option<String>,
    pub color_selection_bg: Option<String>,
    pub color_ansi: Option<[Option<String>; 16]>,
    pub profiles: HashMap<String, Vec<(String, String)>>,
    pub keybinding_overrides: Vec<(String, String)>,
    // Cursor defaults (new in cursor-cfg stream)
    pub cursor_style: Option<String>,
    pub cursor_blink: Option<bool>,
    /// Path to an image file from which the theme should be auto-generated on
    /// startup (via `theme_gen`). When set, the generated theme overrides any
    /// `theme = …` setting and any `color.*` overrides.
    pub wallpaper_theme: Option<String>,
    pub cursor_trail: Option<bool>,
    pub crt_effect: Option<bool>,
    /// Window post-process effect mode (none|frosted|acrylic|crt|scanlines|grain|
    /// vignette|bloom). Supersedes the legacy `crt_effect` bool when present.
    pub window_effect: Option<String>,
    pub show_tab_bar: Option<String>,
    pub title_show_cwd: Option<bool>,
    pub title_show_count: Option<bool>,
    pub minimap: Option<bool>,
    pub quake: Option<bool>,
    pub quake_height: Option<f32>,
    pub quake_animation_ms: Option<u64>,
    pub command_history: Option<usize>,
    pub dim_unfocused: Option<bool>,
    pub unfocused_dim: Option<f32>,
    pub opacity_text: Option<bool>,
    /// Also place a rich-text (HTML) flavor on the clipboard alongside the plain
    /// text on copy, so apps that prefer HTML get a monospace-preserving paste.
    pub copy_html: Option<bool>,
    pub status_bar_segments: Option<Vec<crate::app::StatusBarSegment>>,
    pub status_bar_time_format: Option<String>,
    // --- FONTS stream additions ---
    /// Per-style font family overrides. When set, the named family is used
    /// for bold / italic / bold-italic text instead of synthesizing from the
    /// primary family. Value is a font family name or an absolute file path.
    pub font_bold: Option<String>,
    pub font_italic: Option<String>,
    pub font_bold_italic: Option<String>,
    /// Codepoint / Unicode-range → font-family routing map.
    /// Each entry is `"RANGE:Family"` where RANGE is one of:
    ///   - A single scalar:  `U+E000`
    ///   - An inclusive range: `U+E000-U+F8FF`
    ///     Example: `"U+E000-U+F8FF:Symbols Nerd Font Mono"`.
    ///     Multiple entries are separated by commas or newlines.
    pub font_symbol_map: Option<Vec<SymbolMapEntry>>,
    /// OpenType variable-font axis settings, e.g. `["wght=450", "wdth=75"]`.
    /// `wght` maps to `Weight` in cosmic-text; `wdth` maps to `Stretch`.
    /// Other axis tags are accepted in config but are currently no-ops
    /// (cosmic-text 0.19 does not expose arbitrary axis APIs at the Attrs
    /// level — they log a warning). Comma or space separated.
    pub font_variations: Option<Vec<String>>,
    pub notify_command_finish: Option<bool>,
    pub notify_command_threshold_ms: Option<u64>,
    pub command_fold: Option<bool>,
    pub power_mode: Option<bool>,
    pub power_mode_intensity: Option<f32>,
    /// Custom window-effect channel intensities (0..1), one field per
    /// `gui::settings_panel::CUSTOM_FX_SLIDERS` entry. Only meaningful when
    /// `window_effect = custom`; unset channels fall back to the built-in
    /// "pleasant retro-glass" defaults in [`RawConfig::into_settings`].
    pub fx_curvature: Option<f32>,
    pub fx_scanline: Option<f32>,
    pub fx_glow: Option<f32>,
    pub fx_vignette: Option<f32>,
    pub fx_grain: Option<f32>,
    pub fx_tint: Option<f32>,
    /// Opt-in command-block visual chrome level: `off` | `badges` | `cards`.
    /// Validated in `apply_kv`; resolved to [`crate::app::CommandBlocksMode`] by
    /// [`parse_command_blocks_mode`] at finalization.
    pub command_blocks: Option<String>,
    // --- w15: scrollback memory bounding (Phase 1) ---
    /// Lines of scrollback retained for a backgrounded/idle pane once trimmed;
    /// `0` disables the policy. See `crate::pty::ScrollbackBackgroundPolicy`.
    pub scrollback_background_cap: Option<usize>,
    /// Seconds a pane must be idle/backgrounded before `scrollback_background_cap`
    /// applies. Only meaningful when the cap is non-zero.
    pub scrollback_background_idle_secs: Option<u64>,
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
            color::canonical_name(self.theme_light.as_deref().unwrap_or("one-light")).to_string();

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
        // Split the `[keybindings]` overrides into single-chord binds (merged onto
        // the flat keymap) and multi-chord "leader" sequences (their own map).
        let (single_binds, key_sequences) =
            super::keymap::split_overrides(&self.keybinding_overrides);
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
            keymap: build_keymap(default_keymap(Platform::current()), &single_binds),
            key_sequences,
            cursor_style: parse_cursor_style_config(self.cursor_style.as_deref()),
            cursor_blink: self.cursor_blink.unwrap_or(false),
            wallpaper_theme: self
                .wallpaper_theme
                .filter(|s| !s.is_empty())
                .map(PathBuf::from),
            cursor_trail: self.cursor_trail.unwrap_or(false),
            crt_effect: self.crt_effect.unwrap_or(false),
            // Custom-effect channel intensities [curvature, scanline, glow,
            // vignette, grain, tint]. A pleasant retro-glass default; the
            // Appearance → Custom sliders tune it live and `save_settings`
            // persists it via the `fx_*` keys below.
            custom_effect: [
                self.fx_curvature.unwrap_or(0.12),
                self.fx_scanline.unwrap_or(0.35),
                self.fx_glow.unwrap_or(0.22),
                self.fx_vignette.unwrap_or(0.30),
                self.fx_grain.unwrap_or(0.15),
                self.fx_tint.unwrap_or(0.25),
            ],
            // Resolve the window effect: an explicit `window_effect` wins; else the
            // legacy `crt_effect = true` maps to the CRT mode; else None. This keeps
            // old configs working while exposing the full mode set.
            window_effect: match self.window_effect.as_deref() {
                Some(s) => crate::renderer::WindowEffect::parse(s),
                None => {
                    if self.crt_effect == Some(true) {
                        crate::renderer::WindowEffect::Crt
                    } else {
                        crate::renderer::WindowEffect::None
                    }
                }
            },
            show_tab_bar: parse_tab_bar_mode(self.show_tab_bar.as_deref()),
            title_show_cwd: self.title_show_cwd.unwrap_or(true),
            title_show_count: self.title_show_count.unwrap_or(false),
            hints_chars: self.hints_chars.and_then(|s| normalize_hints_chars(&s)),
            command_badges: self.command_badges.unwrap_or(true),
            minimap: self.minimap.unwrap_or(false),
            quake: self.quake.unwrap_or(false),
            quake_height: {
                let h = self.quake_height.unwrap_or(0.5);
                if h.is_finite() && (QUAKE_HEIGHT_MIN..=QUAKE_HEIGHT_MAX).contains(&h) {
                    h
                } else {
                    0.5
                }
            },
            quake_animation_ms: self
                .quake_animation_ms
                .unwrap_or(180)
                .min(QUAKE_ANIMATION_MS_MAX),
            command_history: self.command_history.unwrap_or(DEFAULT_COMMAND_HISTORY),
            dim_unfocused: self.dim_unfocused.unwrap_or(true),
            unfocused_dim: {
                // Mirror the opacity guard: non-finite falls back to the default,
                // and the 0.9 ceiling keeps a pane from ever being blacked out.
                let d = self
                    .unfocused_dim
                    .unwrap_or(crate::renderer::DEFAULT_PANE_DIM);
                if d.is_finite() {
                    d.clamp(0.0, 0.9)
                } else {
                    crate::renderer::DEFAULT_PANE_DIM
                }
            },
            opacity_text: self.opacity_text.unwrap_or(false),
            copy_html: self.copy_html.unwrap_or(false),
            status_bar_segments: self.status_bar_segments,
            status_bar_time_format: self
                .status_bar_time_format
                .unwrap_or_else(|| "%H:%M".to_string()),
            // FONTS stream
            font_bold: self.font_bold,
            font_italic: self.font_italic,
            font_bold_italic: self.font_bold_italic,
            font_symbol_map: self.font_symbol_map.unwrap_or_default(),
            font_variations: self.font_variations.unwrap_or_default(),
            notify_command_finish: self.notify_command_finish.unwrap_or(true),
            notify_command_threshold_ms: self
                .notify_command_threshold_ms
                .unwrap_or(DEFAULT_NOTIFY_COMMAND_THRESHOLD_MS),
            command_fold: self.command_fold.unwrap_or(true),
            power_mode: self.power_mode.unwrap_or(false),
            power_mode_intensity: {
                let i = self.power_mode_intensity.unwrap_or(0.6);
                if i.is_finite() {
                    i.clamp(0.0, 1.0)
                } else {
                    0.6
                }
            },
            command_blocks: parse_command_blocks_mode(self.command_blocks.as_deref()),
        };

        // Resolve + validate the w15 scrollback-background policy at parse time,
        // so a bad value is caught here rather than silently ignored. NOTE: this
        // does not yet reach live `Pty` sessions — that requires threading the
        // resolved policy through `crate::app::Config` and every `Pty::spawn`
        // call site (`src/app/{panes,event_loop}.rs`, `src/app/tabs/{mod,session}.rs`),
        // which is out of scope for the change that introduced this policy (see
        // `crate::pty::ScrollbackBackgroundPolicy`). Logged at debug level so it
        // is easy to confirm a configured value round-tripped through parsing
        // without implying the feature is already active.
        let scrollback_background_policy = crate::pty::ScrollbackBackgroundPolicy::new(
            self.scrollback_background_cap
                .unwrap_or(DEFAULT_SCROLLBACK_BACKGROUND_CAP),
            self.scrollback_background_idle_secs
                .unwrap_or(DEFAULT_SCROLLBACK_BACKGROUND_IDLE_SECS),
        );
        log::debug!(
            "glassy: scrollback_background_policy resolved to {scrollback_background_policy:?}; \
             effective_cap against this session's scrollback ({}) would be {} \
             (parsed only; not yet wired to live sessions)",
            config.scrollback,
            scrollback_background_policy.effective_cap(config.scrollback),
        );

        // The activated profile name (if any) is attached by the caller
        // (`Settings::resolve` / `resolve_with_profile`) — `RawConfig` itself
        // doesn't track which profile (if any) was applied to it.
        Ok(super::Settings {
            config,
            theme,
            active_profile: None,
        })
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

/// The resolved config DIRECTORY (the parent of `glassy.conf`), honoring the
/// same platform + `$XDG_CONFIG_HOME` resolution as [`path`]. `pub(crate)` so
/// the user-themes loader can find its `themes/` subdirectory without
/// duplicating the platform resolution logic.
pub(crate) fn config_dir() -> Option<PathBuf> {
    config_path().and_then(|p| p.parent().map(PathBuf::from))
}

/// The full on-disk config text as of the last successful [`parse_config_file`]
/// call (startup load, live-reload, or a runtime profile switch) — i.e. what
/// the in-memory `Config` currently reflects. `save`/`save_into_section`
/// compare a fresh read against this right before writing to detect an
/// external edit (a manual `glassy.conf` tweak) that landed inside the
/// config-watcher's ~500ms debounce window, so a Settings-panel Save — which
/// always writes every `SAVED_KEYS` value, not just the ones the user actually
/// changed — doesn't silently stomp that edit. See [`protect_against_external_edit`].
static LAST_LOADED_TEXT: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn last_loaded_slot() -> &'static Mutex<Option<String>> {
    LAST_LOADED_TEXT.get_or_init(|| Mutex::new(None))
}

/// Stash `text` as the most recent successfully-parsed config snapshot.
/// Called once from [`parse_config_file`]; a poisoned lock (impossible short of
/// a panic mid-assignment, which never happens here) just leaves the previous
/// snapshot in place rather than propagating.
fn record_loaded_snapshot(text: &str) {
    if let Ok(mut slot) = last_loaded_slot().lock() {
        *slot = Some(text.to_string());
    }
}

/// A cheap clone of the last recorded load snapshot, or `None` if the process
/// hasn't loaded a config file yet (in which case there is nothing to compare
/// against, so the external-edit guard is a no-op).
fn peek_loaded_snapshot() -> Option<String> {
    last_loaded_slot().lock().ok().and_then(|g| g.clone())
}

/// Persist `updates` (`(key, value)` pairs) into the config file's TOP-LEVEL
/// scope, preserving all other lines, comments, and ordering. A key already
/// present is updated in place; a missing key is appended before the first
/// `[section]` header (or at the end of the file when there are none). Creates
/// the parent directory and file if needed. Used by the live settings overlay so
/// changes survive a restart.
pub fn save(updates: &[(&str, String)]) -> Result<()> {
    let path = config_path().context("no config path (HOME/XDG unset)")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating config dir {}", dir.display()))?;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let protected = protect_against_external_edit(&existing, None, updates);
    let out = merge_config(&existing, &protected);
    std::fs::write(&path, &out).with_context(|| format!("writing config {}", path.display()))?;
    record_loaded_snapshot(&out);
    Ok(())
}

/// Persist `updates` into a specific `[profile.NAME]` section of the config file
/// (`section = Some(name)`), or the TOP-LEVEL scope (`section = None`, same
/// behavior as [`save`]) — preserving everything else: comments, blank lines,
/// other sections, and key ordering within the target range.
///
/// A key already present within the target section is updated in place; a
/// missing one is appended at the END of that section's line range (just before
/// the next `[section]` header, or end of file). When `section = Some(name)` and
/// no matching `[profile.name]` header exists yet, a brand-new section is
/// appended at the end of the file (with a blank-line separator when the file
/// has non-blank trailing content).
///
/// Header matching mirrors [`parse_config_file`]'s rules exactly (so a write
/// always targets the section a subsequent read would resolve): the literal
/// `profile.` prefix is case-SENSITIVE, but the name after it is matched
/// case-INsensitively. When multiple `[profile.name]` headers exist in the file
/// (a malformed/hand-edited config), only the FIRST is touched — mirroring
/// [`profile_names_from_text`]'s first-seen identity.
///
/// Creates the parent directory and file if needed.
pub fn save_into_section(section: Option<&str>, updates: &[(&str, String)]) -> Result<()> {
    let path = config_path().context("no config path (HOME/XDG unset)")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating config dir {}", dir.display()))?;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let protected = protect_against_external_edit(&existing, section, updates);
    let out = merge_into_section(&existing, section, &protected);
    std::fs::write(&path, &out).with_context(|| format!("writing config {}", path.display()))?;
    record_loaded_snapshot(&out);
    Ok(())
}

/// Merge `updates` into the text of a config file's TOP-LEVEL scope: a present
/// key is updated in place (preserving its position), a missing one is appended
/// BEFORE the first `[section]` header (never inside one); comments, blank
/// lines, unmanaged keys, and ordering are preserved. Pure for unit testing.
fn merge_config(existing: &str, updates: &[(&str, String)]) -> String {
    merge_into_section(existing, None, updates)
}

/// Whether a trimmed line is a `[...]` section header of any kind.
fn is_section_header(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('[') && t.ends_with(']')
}

/// Parse a trimmed section-header line's profile name (`"work"` from
/// `"[profile.work]"`), if it is a `[profile.NAME]` header — matching the exact
/// case rules [`parse_config_file`] uses: the `profile.` prefix is
/// case-sensitive, the name after it is lower-cased for comparison.
fn profile_header_name(line: &str) -> Option<String> {
    let line = line.trim();
    if !(line.starts_with('[') && line.ends_with(']')) {
        return None;
    }
    let inner = line[1..line.len() - 1].trim();
    let name = inner.strip_prefix("profile.")?.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_ascii_lowercase())
    }
}

/// Resolve the (start, end) half-open line-index range for `section`'s body —
/// the TOP-LEVEL scope (`section = None`, bounded by the first `[section]`
/// header) or a specific `[profile.NAME]` section's body (bounded by the next
/// `[section]` header after its own) — WITHOUT mutating `lines`. Returns `None`
/// only when `section = Some(name)` and no matching header exists yet (a
/// lookup-only caller has nothing to find; [`merge_into_section`] is the one
/// place that creates a missing section on write).
fn find_range(lines: &[String], section: Option<&str>) -> Option<(usize, usize)> {
    match section {
        None => {
            let end = lines
                .iter()
                .position(|l| is_section_header(l))
                .unwrap_or(lines.len());
            Some((0, end))
        }
        Some(name) => {
            let key = name.to_ascii_lowercase();
            let header_idx = lines
                .iter()
                .position(|l| profile_header_name(l).as_deref() == Some(key.as_str()))?;
            let end = lines[header_idx + 1..]
                .iter()
                .position(|l| is_section_header(l))
                .map(|off| header_idx + 1 + off)
                .unwrap_or(lines.len());
            Some((header_idx + 1, end))
        }
    }
}

/// Merge `updates` into `existing`'s target range: the TOP-LEVEL scope
/// (`section = None`, bounded by the first `[section]` header) or a specific
/// `[profile.NAME]` section's body (`section = Some(name)`, bounded by the next
/// `[section]` header after its own). See [`save_into_section`] for the full
/// contract. Pure for unit testing.
fn merge_into_section(existing: &str, section: Option<&str>, updates: &[(&str, String)]) -> String {
    let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();

    let range: (usize, usize) = match find_range(&lines, section) {
        Some(r) => r,
        None => {
            // `section = Some(name)` and no existing header: append a brand-new
            // one at the end of the file (separated by a blank line unless the
            // file already ends in blank/empty content).
            let name = section.expect("find_range only returns None when section is Some");
            if lines.last().is_some_and(|l| !l.is_empty()) {
                lines.push(String::new());
            }
            lines.push(format!("[profile.{name}]"));
            let start = lines.len();
            (start, start)
        }
    };

    apply_updates_in_range(&mut lines, range, updates);

    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Locate the first unquoted `#` or `;` in `s` — scanning a config value
/// substring for a trailing inline comment. A marker inside a matched
/// `'...'`/`"..."` span is part of the value, not a comment, and is skipped;
/// an unterminated quote runs to the end of the string (so any `#`/`;` after
/// an opening quote with no matching close is treated as still-quoted).
fn find_inline_comment_start(s: &str) -> Option<usize> {
    let mut in_single = false;
    let mut in_double = false;
    for (idx, ch) in s.char_indices() {
        match ch {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '#' | ';' if !in_single && !in_double => return Some(idx),
            _ => {}
        }
    }
    None
}

/// Split a raw, unquoted-scan value substring (the text after a config line's
/// `=`) into `(value, trailing_comment)`. `trailing_comment` is `Some` (right-
/// trimmed, marker included) when an unquoted `#`/`;` was found; `value` still
/// carries its own surrounding whitespace — callers trim as needed.
fn split_value_and_comment(raw_value: &str) -> (&str, Option<&str>) {
    match find_inline_comment_start(raw_value) {
        Some(idx) => (&raw_value[..idx], Some(raw_value[idx..].trim_end())),
        None => (raw_value, None),
    }
}

/// Look up `key`'s current value within `lines[range.0..range.1]`, the same
/// way [`apply_updates_in_range`] finds a matching line — but read-only, and
/// with any inline trailing comment stripped. Used by
/// [`protect_against_external_edit`] to compare a baseline snapshot's value
/// for a key against the value about to be written.
fn find_key_value_in_range(lines: &[String], range: (usize, usize), key: &str) -> Option<String> {
    let (start, end) = range;
    let end = end.min(lines.len());
    if start >= end {
        return None;
    }
    for line in &lines[start..end] {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        let Some((k, raw_value)) = trimmed.split_once('=') else {
            continue;
        };
        if k.trim().eq_ignore_ascii_case(key) {
            let (value, _) = split_value_and_comment(raw_value);
            return Some(value.trim().to_string());
        }
    }
    None
}

/// Filter `updates` down to the keys that genuinely changed since the last
/// successful config load, guarding against clobbering a fresh external edit.
///
/// `save`/`save_into_section` always forward the settings UI's FULL live
/// snapshot (every `SAVED_KEYS` value, via `App::save_settings`), not a diff —
/// so if a manual `glassy.conf` edit lands inside the config-watcher's ~500ms
/// debounce window (after the in-memory `Config` was last loaded but before a
/// Settings-panel Save fires), a naive merge would silently overwrite that
/// edit with the stale in-memory value for every key the UI didn't actually
/// touch. Comparing `existing` (freshly re-read right before this call) against
/// the snapshot recorded at the last load detects that window: when they
/// differ, a key is only forwarded if its new value differs from what was
/// loaded (a real settings-UI change — last-writer-wins for that narrow same-
/// key race is acceptable); an update whose value merely matches the stale
/// load is dropped so it doesn't stomp whatever the external edit just wrote.
///
/// When nothing has been loaded yet, the file is unchanged since the last
/// load, or the target section doesn't exist in the loaded baseline (e.g. a
/// brand-new profile), every update is forwarded unmodified — today's
/// behavior.
fn protect_against_external_edit<'a>(
    existing: &str,
    section: Option<&str>,
    updates: &'a [(&'a str, String)],
) -> Vec<(&'a str, String)> {
    let Some(baseline) = peek_loaded_snapshot() else {
        return updates.to_vec();
    };
    filter_updates_against_baseline(&baseline, existing, section, updates)
}

/// The pure filtering logic behind [`protect_against_external_edit`], taking
/// the baseline snapshot as a plain argument instead of reading the process-
/// global [`LAST_LOADED_TEXT`] — split out so tests can drive it directly
/// without contending over shared global state with other tests running in
/// parallel. See [`protect_against_external_edit`] for the full contract.
fn filter_updates_against_baseline<'a>(
    baseline: &str,
    existing: &str,
    section: Option<&str>,
    updates: &'a [(&'a str, String)],
) -> Vec<(&'a str, String)> {
    if baseline == existing {
        return updates.to_vec();
    }
    let baseline_lines: Vec<String> = baseline.lines().map(str::to_string).collect();
    let Some(range) = find_range(&baseline_lines, section) else {
        return updates.to_vec();
    };
    updates
        .iter()
        .filter(
            |(k, v)| match find_key_value_in_range(&baseline_lines, range, k) {
                Some(old) => old != *v,
                None => true,
            },
        )
        .cloned()
        .collect()
}

/// Update-in-place (or append-at-range-end) the `(key, value)` pairs within
/// `lines[range.0..range.1]`. A key found in the range (skipping comments/blank
/// lines) is rewritten in place, preserving any inline trailing `#`/`;` comment
/// on that line (a `#`/`;` inside a quoted value doesn't count — see
/// [`find_inline_comment_start`]); a key not found is appended just before any
/// TRAILING blank lines at the end of the range (so a blank-line separator
/// before the next section, or the file's end, stays trailing instead of
/// getting sandwiched between old and newly-appended content), preserving
/// `updates`' relative order.
fn apply_updates_in_range(
    lines: &mut Vec<String>,
    range: (usize, usize),
    updates: &[(&str, String)],
) {
    let (start, end) = range;
    let mut written = vec![false; updates.len()];

    for line in &mut lines[start..end] {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        let Some((key, raw_value)) = trimmed.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        // Computed (and owned) BEFORE the `*line` reassignment below so the
        // borrow of `line` via `trimmed`/`raw_value` ends here, not at the
        // assignment (which would otherwise conflict under NLL).
        let comment: Option<String> = split_value_and_comment(raw_value).1.map(str::to_string);
        for (i, (k, v)) in updates.iter().enumerate() {
            if !written[i] && key == *k {
                // Strip newlines and carriage returns from value to prevent injection.
                let clean_v = v.replace(['\n', '\r'], "");
                *line = match &comment {
                    Some(c) => format!("{k} = {clean_v}  {c}"),
                    None => format!("{k} = {clean_v}"),
                };
                written[i] = true;
            }
        }
    }

    let insertions: Vec<String> = updates
        .iter()
        .zip(written.iter())
        .filter(|&(_, &w)| !w)
        .map(|((k, v), _)| {
            // Strip newlines and carriage returns from value to prevent injection.
            let clean_v = v.replace(['\n', '\r'], "");
            format!("{k} = {clean_v}")
        })
        .collect();
    if insertions.is_empty() {
        return;
    }
    // Back up over trailing blank lines within the range so the insertion point
    // sits right after the last real content line, not after a blank separator.
    let mut insert_at = end;
    while insert_at > start && lines[insert_at - 1].trim().is_empty() {
        insert_at -= 1;
    }
    for (offset, line) in insertions.into_iter().enumerate() {
        lines.insert(insert_at + offset, line);
    }
}

/// The resolved config file path, honoring `$XDG_CONFIG_HOME` then `$HOME`.
/// On macOS, uses ~/Library/Application Support/glassy/glassy.conf.
/// On other platforms, honors $XDG_CONFIG_HOME then ~/.config/glassy/glassy.conf.
fn config_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join("Library/Application Support/glassy/glassy.conf"))
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
    // Only stash the snapshot on a successful parse: every real load path
    // (`Settings::resolve`, `resolve_with_profile`, `resolve_base`) routes
    // through here, so this is the one place that sees exactly what the
    // in-memory `Config` now reflects. A failed parse leaves the app's config
    // unchanged, so it must not overwrite the snapshot either — see
    // `save`/`save_into_section`'s external-edit guard below.
    record_loaded_snapshot(text);
    Ok(())
}

/// Extract the `[profile.NAME]` section names from raw config text, lower-cased and
/// in first-seen order (the parser's `HashMap` doesn't preserve order, and the
/// runtime switcher wants a stable list). Duplicates are de-duplicated.
pub(super) fn profile_names_from_text(text: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            let inner = line[1..line.len() - 1].trim();
            if let Some(profile_name) = inner.strip_prefix("profile.") {
                let n = profile_name.trim().to_ascii_lowercase();
                if !n.is_empty() && !names.contains(&n) {
                    names.push(n);
                }
            }
        }
    }
    names
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

/// Parse a hex color string (with or without leading #) to an Rgb. `pub(crate)`
/// so the theme-file importer (`config::theme_import`) and the user-themes
/// loader (`color::user_themes`) share this one parser instead of each
/// hand-rolling their own.
pub(crate) fn parse_hex_color(s: &str) -> Result<alacritty_terminal::vte::ansi::Rgb> {
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

/// Dry-run validate a single `key = value` pair through the exact parser the
/// config file loader uses ([`apply_kv`]), without touching any live state.
/// Used by `ipc::control`'s `set-config` remote-control verb to reject a hard
/// parse failure (e.g. a non-numeric `opacity`, an out-of-range
/// `cursor_style`) before ever writing the value to disk.
///
/// `Ok(())` means `apply_kv` accepted the value — it does NOT mean the value
/// survives unclamped: several numeric keys (`opacity`, `power_mode_intensity`,
/// the `fx_*` channels) are silently clamped later, in
/// [`RawConfig::into_settings`], rather than rejected here. An unrecognized
/// key is intentionally not an error from `apply_kv` itself (it just logs a
/// warning and no-ops); callers that need to reject unknown keys (like
/// `set-config`) check that separately (see
/// `ipc::control::is_known_config_key`) before ever calling this.
pub fn validate_kv(key: &str, value: &str) -> Result<()> {
    let mut raw = RawConfig::default();
    apply_kv(key, value, &mut raw)
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
                // Split-theme syntax: `theme = light:X, dark:Y` turns on
                // follow_system and pins the per-scheme themes. A bare name keeps
                // the legacy single-theme behaviour. Either half may be omitted.
                if let Some((light, dark)) = parse_split_theme(value) {
                    raw.follow_system = Some(true);
                    if let Some(l) = light {
                        raw.theme_light = Some(l);
                    }
                    if let Some(d) = dark {
                        raw.theme_dark = Some(d.clone());
                        // Seed the active theme to the dark half so a first paint
                        // before the OS scheme is known is sensible.
                        raw.theme = Some(d);
                    }
                } else {
                    raw.theme = Some(value.to_string());
                }
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
            raw.scrollback = Some(n.clamp(0, SCROLLBACK_MAX));
        }
        "command_history" => {
            let n: usize = value
                .parse()
                .with_context(|| format!("command_history: invalid integer '{value}'"))?;
            raw.command_history = Some(n.clamp(0, 10_000));
        }
        "notify_command_finish" => {
            raw.notify_command_finish = Some(parse_bool(value, "notify_command_finish")?);
        }
        "notify_command_threshold_ms" => {
            let ms: u64 = value.parse().with_context(|| {
                format!("notify_command_threshold_ms: invalid integer '{value}'")
            })?;
            raw.notify_command_threshold_ms = Some(ms.min(NOTIFY_COMMAND_THRESHOLD_MS_MAX));
        }
        "command_fold" => {
            raw.command_fold = Some(parse_bool(value, "command_fold")?);
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
        "dim_unfocused" => {
            raw.dim_unfocused = Some(parse_bool(value, "dim_unfocused")?);
        }
        "unfocused_dim" => {
            let d: f32 = value
                .parse()
                .with_context(|| format!("unfocused_dim: invalid number '{value}'"))?;
            raw.unfocused_dim = Some(d);
        }
        "opacity_scope" => {
            raw.opacity_text = Some(match value {
                "background" => false,
                "text" => true,
                other => bail!("opacity_scope must be 'background' or 'text', got '{other}'"),
            });
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
        "copy_html" => {
            raw.copy_html = Some(parse_bool(value, "copy_html")?);
        }
        "cursor_trail" => {
            raw.cursor_trail = Some(parse_bool(value, "cursor_trail")?);
        }
        "crt_effect" => {
            raw.crt_effect = Some(parse_bool(value, "crt_effect")?);
        }
        "window_effect" => {
            // Accept ANY value here and let `WindowEffect::parse` resolve it at
            // finalization (unknown → `none`). Per this key's long-standing
            // contract, a typo — or a mode word this build predates — must never
            // abort config load; validating with a hardcoded allowlist here was a
            // latent bug (it rejected `custom` and killed startup). The single
            // source of truth for the mode set is `WindowEffect::parse`.
            raw.window_effect = Some(value.to_ascii_lowercase());
        }
        "show_tab_bar" => {
            // Accepts the three policy words plus the usual bool spellings
            // (true→always, false→never) so a boolean reads naturally too.
            let v = value.to_ascii_lowercase();
            match v.as_str() {
                "auto" | "always" | "never" => raw.show_tab_bar = Some(v),
                "true" | "yes" | "on" | "1" => raw.show_tab_bar = Some("always".into()),
                "false" | "no" | "off" | "0" => raw.show_tab_bar = Some("never".into()),
                _ => bail!("show_tab_bar must be auto/always/never, got '{value}'"),
            }
        }
        "title_show_cwd" => {
            raw.title_show_cwd = Some(parse_bool(value, "title_show_cwd")?);
        }
        "title_show_count" => {
            raw.title_show_count = Some(parse_bool(value, "title_show_count")?);
        }
        "hints_chars" => {
            // The label alphabet for hints mode (home-row-first letters). Only the
            // ASCII letters are kept; an alphabet shorter than 2 chars is ignored.
            raw.hints_chars = Some(value.to_string());
        }
        "command_badges" => {
            raw.command_badges = Some(parse_bool(value, "command_badges")?);
        }
        "minimap" => {
            raw.minimap = Some(parse_bool(value, "minimap")?);
        }
        "quake" => {
            raw.quake = Some(parse_bool(value, "quake")?);
        }
        "quake_height" => {
            let h: f32 = value
                .parse()
                .with_context(|| format!("quake_height: invalid number '{value}'"))?;
            if !(h.is_finite() && (QUAKE_HEIGHT_MIN..=QUAKE_HEIGHT_MAX).contains(&h)) {
                bail!(
                    "quake_height must be between {QUAKE_HEIGHT_MIN} and {QUAKE_HEIGHT_MAX}, got {h}"
                );
            }
            raw.quake_height = Some(h);
        }
        "quake_animation_ms" => {
            let ms: u64 = value
                .parse()
                .with_context(|| format!("quake_animation_ms: invalid integer '{value}'"))?;
            raw.quake_animation_ms = Some(ms.min(QUAKE_ANIMATION_MS_MAX));
        }
        "power_mode" => {
            raw.power_mode = Some(parse_bool(value, "power_mode")?);
        }
        "power_mode_intensity" => {
            let i: f32 = value
                .parse()
                .with_context(|| format!("power_mode_intensity: invalid number '{value}'"))?;
            if !(i.is_finite() && (0.0..=1.0).contains(&i)) {
                bail!("power_mode_intensity must be between 0.0 and 1.0, got {i}");
            }
            raw.power_mode_intensity = Some(i);
        }
        "fx_curvature" => {
            raw.fx_curvature = Some(parse_unit_f32(value, "fx_curvature")?);
        }
        "fx_scanline" => {
            raw.fx_scanline = Some(parse_unit_f32(value, "fx_scanline")?);
        }
        "fx_glow" => {
            raw.fx_glow = Some(parse_unit_f32(value, "fx_glow")?);
        }
        "fx_vignette" => {
            raw.fx_vignette = Some(parse_unit_f32(value, "fx_vignette")?);
        }
        "fx_grain" => {
            raw.fx_grain = Some(parse_unit_f32(value, "fx_grain")?);
        }
        "fx_tint" => {
            raw.fx_tint = Some(parse_unit_f32(value, "fx_tint")?);
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
        "cursor_style" => {
            let lower = value.to_ascii_lowercase();
            match lower.as_str() {
                "block" | "beam" | "underline" => {
                    raw.cursor_style = Some(lower);
                }
                _ => {
                    bail!("cursor_style must be block, beam, or underline; got '{value}'");
                }
            }
        }
        "cursor_blink" => {
            raw.cursor_blink = Some(parse_bool(value, "cursor_blink")?);
        }
        "wallpaper_theme" => {
            if !value.is_empty() {
                raw.wallpaper_theme = Some(value.to_string());
            }
        }
        "status_bar_segments" => {
            if value.is_empty() {
                raw.status_bar_segments = None;
            } else {
                raw.status_bar_segments = Some(parse_status_bar_segments(value));
            }
        }
        "status_bar_time_format" => {
            if !value.is_empty() {
                raw.status_bar_time_format = Some(value.to_string());
            }
        }
        // --- FONTS stream ---
        "font_bold" => {
            if !value.is_empty() {
                raw.font_bold = Some(value.to_string());
            }
        }
        "font_italic" => {
            if !value.is_empty() {
                raw.font_italic = Some(value.to_string());
            }
        }
        "font_bold_italic" => {
            if !value.is_empty() {
                raw.font_bold_italic = Some(value.to_string());
            }
        }
        "font_symbol_map" => {
            if !value.is_empty() {
                raw.font_symbol_map = Some(parse_symbol_map(value));
            }
        }
        "font_variations" => {
            if !value.is_empty() {
                raw.font_variations = Some(parse_font_variations(value));
            }
        }
        "command_blocks" => {
            let v = value.to_ascii_lowercase();
            match v.as_str() {
                "off" | "badges" | "cards" => raw.command_blocks = Some(v),
                _ => bail!("command_blocks must be off, badges, or cards; got '{value}'"),
            }
        // --- w15: scrollback memory bounding (Phase 1) ---
        "scrollback_background_cap" => {
            let n: usize = value
                .parse()
                .with_context(|| format!("scrollback_background_cap: invalid integer '{value}'"))?;
            raw.scrollback_background_cap = Some(n.clamp(0, SCROLLBACK_MAX));
        }
        "scrollback_background_idle_secs" => {
            let s: u64 = value.parse().with_context(|| {
                format!("scrollback_background_idle_secs: invalid integer '{value}'")
            })?;
            raw.scrollback_background_idle_secs = Some(s);
        }
        other => {
            log::warn!("glassy: ignoring unknown config key '{other}'");
        }
    }
    Ok(())
}

/// Parse a cursor style string into the app config enum (block is the default).
pub(crate) fn parse_cursor_style_config(s: Option<&str>) -> crate::app::CursorStyleConfig {
    match s {
        Some("beam") => crate::app::CursorStyleConfig::Beam,
        Some("underline") => crate::app::CursorStyleConfig::Underline,
        _ => crate::app::CursorStyleConfig::Block,
    }
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

/// Parse a float clamped to `[0.0, 1.0]` for a named field (the Custom-effect
/// channel intensities and other unit-range config knobs).
fn parse_unit_f32(value: &str, field: &str) -> Result<f32> {
    let v: f32 = value
        .parse()
        .with_context(|| format!("{field}: invalid number '{value}'"))?;
    if !(v.is_finite() && (0.0..=1.0).contains(&v)) {
        bail!("{field} must be between 0.0 and 1.0, got {value}");
    }
    Ok(v)
}

/// Map a (validated) `show_tab_bar` token to the [`TabBarMode`] enum, defaulting
/// to `Auto` when unset. The validation already happened in [`apply_kv`].
pub(super) fn parse_tab_bar_mode(value: Option<&str>) -> crate::app::TabBarMode {
    use crate::app::TabBarMode;
    match value {
        Some("always") => TabBarMode::Always,
        Some("never") => TabBarMode::Never,
        _ => TabBarMode::Auto,
    }
}

/// Map a (validated) `command_blocks` token to the [`crate::app::CommandBlocksMode`]
/// enum, defaulting to `Badges` (today's appearance) when unset. The validation
/// already happened in [`apply_kv`].
pub(super) fn parse_command_blocks_mode(value: Option<&str>) -> crate::app::CommandBlocksMode {
    use crate::app::CommandBlocksMode;
    match value {
        Some("off") => CommandBlocksMode::Off,
        Some("cards") => CommandBlocksMode::Cards,
        _ => CommandBlocksMode::Badges,
    }
}

/// Parse the split-theme syntax `light:NAME, dark:NAME` (either half optional,
/// order-independent, comma-separated). Returns `Some((light, dark))` only when at
/// least one `light:`/`dark:` token is present; a bare theme name returns `None`
/// so the caller falls back to the single-theme path. Whitespace around tokens and
/// names is tolerated.
pub(super) fn parse_split_theme(value: &str) -> Option<(Option<String>, Option<String>)> {
    let mut light = None;
    let mut dark = None;
    let mut saw_tag = false;
    for part in value.split(',') {
        let part = part.trim();
        if let Some(rest) = part
            .strip_prefix("light:")
            .or_else(|| part.strip_prefix("Light:"))
        {
            let n = rest.trim();
            if !n.is_empty() {
                light = Some(n.to_string());
            }
            saw_tag = true;
        } else if let Some(rest) = part
            .strip_prefix("dark:")
            .or_else(|| part.strip_prefix("Dark:"))
        {
            let n = rest.trim();
            if !n.is_empty() {
                dark = Some(n.to_string());
            }
            saw_tag = true;
        }
    }
    saw_tag.then_some((light, dark))
}

/// Split a `shell` value (a whitespace-separated program + args) into a `Shell`.
/// Returns `None` for an empty value.
pub(super) fn parse_shell(value: &str) -> Option<Shell> {
    let mut parts = value.split_whitespace();
    let program = parts.next()?.to_string();
    let args = parts.map(str::to_string).collect();
    Some(Shell::new(program, args))
}

#[cfg(test)]
mod merge_tests {
    //! Exhaustive coverage for `merge_config` / `save_into_section`'s underlying
    //! `merge_into_section`: a bug here silently corrupts a user's config file on
    //! the next Save, so every boundary (empty file, no sections, mid-file
    //! section, trailing section, duplicate headers, comments, trailing
    //! newlines) gets its own focused test rather than one broad one.
    use super::*;

    fn upd<'a>(pairs: &[(&'a str, &str)]) -> Vec<(&'a str, String)> {
        pairs.iter().map(|(k, v)| (*k, v.to_string())).collect()
    }

    // -----------------------------------------------------------------------
    // merge_config (top-level scope)
    // -----------------------------------------------------------------------

    #[test]
    fn merge_config_empty_file_appends_all() {
        let out = merge_config("", &upd(&[("theme", "dracula"), ("opacity", "0.9")]));
        assert_eq!(out, "theme = dracula\nopacity = 0.9\n");
    }

    #[test]
    fn merge_config_updates_in_place_preserves_position_and_comments() {
        let existing = "\
# my config
theme = tokyo-night
font_size = 14
opacity = 0.80
";
        let out = merge_config(existing, &upd(&[("font_size", "20"), ("opacity", "0.95")]));
        assert_eq!(
            out,
            "\
# my config
theme = tokyo-night
font_size = 20
opacity = 0.95
"
        );
    }

    #[test]
    fn merge_config_appends_missing_key_when_no_sections_exist() {
        let existing = "theme = dracula\nfont_size = 14\n";
        let out = merge_config(existing, &upd(&[("scrollback", "5000")]));
        assert_eq!(out, "theme = dracula\nfont_size = 14\nscrollback = 5000\n");
    }

    /// REGRESSION for the bug this branch fixes: `merge_config` used to append a
    /// missing top-level key at the absolute END of the file with no regard for
    /// section boundaries. When the file's last section was a `[profile.NAME]`
    /// (or `[keybindings]`), the appended line landed INSIDE that section instead
    /// of at the top level — e.g. saving `scrollback` when `[profile.work]` was
    /// the last thing in the file silently turned it into a profile-only
    /// override, invisible to the base config. The fix bounds the top-level
    /// range to end at the first `[section]` header; a missing key is inserted
    /// there instead.
    #[test]
    fn merge_config_top_level_append_lands_before_first_section_not_inside_it() {
        let existing = "\
theme = dracula

[profile.work]
font_size = 18
";
        let out = merge_config(existing, &upd(&[("scrollback", "5000")]));
        assert_eq!(
            out,
            "\
theme = dracula
scrollback = 5000

[profile.work]
font_size = 18
"
        );
        // The appended line must be BEFORE the section header, not after/inside it.
        let section_line = out.lines().position(|l| l == "[profile.work]").unwrap();
        let appended_line = out.lines().position(|l| l == "scrollback = 5000").unwrap();
        assert!(
            appended_line < section_line,
            "top-level append must precede the first section header"
        );
    }

    /// Same regression, but with `[keybindings]` as the last section — the same
    /// unconditional end-of-file append bug would have corrupted a keybinding
    /// override into a nonsense top-level-looking line inside `[keybindings]`.
    #[test]
    fn merge_config_top_level_append_lands_before_keybindings_section() {
        let existing = "theme = dracula\n\n[keybindings]\nctrl+t = new_tab\n";
        let out = merge_config(existing, &upd(&[("scrollback", "5000")]));
        let section_line = out.lines().position(|l| l == "[keybindings]").unwrap();
        let appended_line = out.lines().position(|l| l == "scrollback = 5000").unwrap();
        assert!(appended_line < section_line);
    }

    /// A key with the same name that exists ONLY inside a `[profile.*]` section
    /// must not be mistaken for an existing top-level key: saving it at the top
    /// level appends a NEW top-level line, and the profile-scoped one is left
    /// completely untouched.
    #[test]
    fn merge_config_key_inside_section_is_not_mistaken_for_top_level() {
        let existing = "theme = dracula\n\n[profile.compact]\nfont_size = 10\n";
        let out = merge_config(existing, &upd(&[("font_size", "16")]));
        assert_eq!(
            out,
            "theme = dracula\nfont_size = 16\n\n[profile.compact]\nfont_size = 10\n"
        );
    }

    #[test]
    fn merge_config_no_trailing_newline_in_source_still_normalizes() {
        let existing = "theme = dracula";
        let out = merge_config(existing, &upd(&[("opacity", "0.9")]));
        assert_eq!(out, "theme = dracula\nopacity = 0.9\n");
        assert!(out.ends_with('\n') && !out.ends_with("\n\n"));
    }

    // -----------------------------------------------------------------------
    // save_into_section (Some(name) — profile-scoped)
    // -----------------------------------------------------------------------

    #[test]
    fn section_creates_new_section_in_empty_file() {
        let out = merge_into_section("", Some("work"), &upd(&[("theme", "nord")]));
        assert_eq!(out, "[profile.work]\ntheme = nord\n");
    }

    #[test]
    fn section_creates_new_section_with_only_top_level_keys_present() {
        let existing = "theme = dracula\nfont_size = 14\n";
        let out = merge_into_section(existing, Some("work"), &upd(&[("theme", "nord")]));
        assert_eq!(
            out,
            "theme = dracula\nfont_size = 14\n\n[profile.work]\ntheme = nord\n"
        );
    }

    #[test]
    fn section_new_section_no_double_blank_line_when_file_already_ends_blank() {
        let existing = "theme = dracula\n\n";
        let out = merge_into_section(existing, Some("work"), &upd(&[("theme", "nord")]));
        // Exactly one blank line separates the existing content from the new section.
        assert_eq!(out, "theme = dracula\n\n[profile.work]\ntheme = nord\n");
    }

    #[test]
    fn section_updates_existing_section_mid_file_followed_by_another_section() {
        let existing = "\
theme = dracula

[profile.work]
font_size = 18
cwd = /tmp

[profile.home]
font_size = 12
";
        let out = merge_into_section(
            existing,
            Some("work"),
            &upd(&[("font_size", "20"), ("scrollback", "5000")]),
        );
        assert_eq!(
            out,
            "\
theme = dracula

[profile.work]
font_size = 20
cwd = /tmp
scrollback = 5000

[profile.home]
font_size = 12
"
        );
    }

    /// A key present at the TOP LEVEL (outside any section) but absent from the
    /// target `[profile.NAME]` section must NOT be touched — updating the
    /// profile must never leak into the base config.
    #[test]
    fn section_does_not_touch_top_level_key_of_same_name() {
        let existing = "scrollback = 5000\n\n[profile.compact]\nfont_size = 10\n";
        let out = merge_into_section(existing, Some("compact"), &upd(&[("scrollback", "999")]));
        assert_eq!(
            out,
            "scrollback = 5000\n\n[profile.compact]\nfont_size = 10\nscrollback = 999\n"
        );
        // The top-level scrollback is unchanged.
        assert!(out.lines().any(|l| l == "scrollback = 5000"));
    }

    /// A key present in a DIFFERENT profile section must not be touched either.
    #[test]
    fn section_does_not_touch_a_different_profiles_key() {
        let existing = "[profile.work]\nfont_size = 18\n\n[profile.home]\nfont_size = 12\n";
        let out = merge_into_section(existing, Some("home"), &upd(&[("font_size", "20")]));
        assert!(out.contains("[profile.work]\nfont_size = 18"));
        assert!(out.contains("[profile.home]\nfont_size = 20"));
    }

    /// When multiple `[profile.NAME]` headers exist in the file (a malformed or
    /// hand-edited config), only the FIRST is updated — mirroring
    /// `profile_names_from_text`'s first-seen identity. The second, duplicate
    /// block is left completely untouched.
    #[test]
    fn section_duplicate_headers_first_wins() {
        let existing = "\
[profile.work]
font_size = 16

[profile.work]
font_size = 99
";
        let out = merge_into_section(existing, Some("work"), &upd(&[("font_size", "20")]));
        assert_eq!(
            out,
            "\
[profile.work]
font_size = 20

[profile.work]
font_size = 99
"
        );
    }

    #[test]
    fn section_preserves_comments_and_unknown_lines_within_section() {
        let existing = "\
[profile.work]
# a comment inside the profile
font_size = 18
; another comment style
unknown_future_key = something
";
        let out = merge_into_section(existing, Some("work"), &upd(&[("font_size", "20")]));
        assert_eq!(
            out,
            "\
[profile.work]
# a comment inside the profile
font_size = 20
; another comment style
unknown_future_key = something
"
        );
    }

    #[test]
    fn section_preserves_comments_and_other_sections_outside_target() {
        let existing = "\
# top comment
theme = dracula

[profile.work]
font_size = 18
";
        let out = merge_into_section(existing, Some("work"), &upd(&[("cwd", "/home/me/work")]));
        assert_eq!(
            out,
            "\
# top comment
theme = dracula

[profile.work]
font_size = 18
cwd = /home/me/work
"
        );
    }

    #[test]
    fn section_trailing_newline_always_exactly_one() {
        let existing = "[profile.work]\nfont_size = 18";
        let out = merge_into_section(existing, Some("work"), &upd(&[("scrollback", "5000")]));
        assert!(out.ends_with('\n') && !out.ends_with("\n\n"));
    }

    #[test]
    fn section_name_match_is_case_insensitive() {
        let existing = "[profile.Work]\nfont_size = 18\n";
        let out = merge_into_section(existing, Some("WORK"), &upd(&[("font_size", "20")]));
        // The existing (differently-cased) header is matched and updated in place,
        // not duplicated as a second section.
        assert_eq!(out, "[profile.Work]\nfont_size = 20\n");
        assert_eq!(out.matches("[profile.Work]").count(), 1);
    }

    #[test]
    fn section_none_behaves_identically_to_merge_config() {
        let existing = "theme = dracula\n\n[profile.work]\nfont_size = 18\n";
        let updates = upd(&[("scrollback", "5000")]);
        assert_eq!(
            merge_into_section(existing, None, &updates),
            merge_config(existing, &updates)
        );
    }

    #[test]
    fn section_appends_missing_keys_in_order_before_next_section() {
        let existing = "[profile.work]\nfont_size = 18\n\n[profile.home]\nfont_size = 12\n";
        let out = merge_into_section(
            existing,
            Some("work"),
            &upd(&[("a_key", "1"), ("b_key", "2")]),
        );
        let a_idx = out.lines().position(|l| l == "a_key = 1").unwrap();
        let b_idx = out.lines().position(|l| l == "b_key = 2").unwrap();
        let home_idx = out.lines().position(|l| l == "[profile.home]").unwrap();
        assert!(a_idx < b_idx, "insertion order must match `updates` order");
        assert!(
            b_idx < home_idx,
            "insertions must land before the next section"
        );
    }

    // -----------------------------------------------------------------------
    // Inline trailing comment preservation (apply_updates_in_range)
    // -----------------------------------------------------------------------

    #[test]
    fn merge_config_preserves_inline_hash_comment_on_rewritten_key() {
        let existing = "opacity = 0.5  # my note\n";
        let out = merge_config(existing, &upd(&[("opacity", "0.9")]));
        assert_eq!(out, "opacity = 0.9  # my note\n");
    }

    #[test]
    fn merge_config_preserves_inline_semicolon_comment_on_rewritten_key() {
        let existing = "opacity = 0.5  ; my note\n";
        let out = merge_config(existing, &upd(&[("opacity", "0.9")]));
        assert_eq!(out, "opacity = 0.9  ; my note\n");
    }

    #[test]
    fn merge_config_no_comment_present_behaves_identically_to_before() {
        let existing = "opacity = 0.5\n";
        let out = merge_config(existing, &upd(&[("opacity", "0.9")]));
        assert_eq!(out, "opacity = 0.9\n");
    }

    #[test]
    fn merge_config_hash_inside_single_quoted_value_is_not_a_comment() {
        // The '#' is part of the quoted value (a literal separator string), not
        // a trailing comment — nothing should be preserved/appended after the
        // rewritten value.
        let existing = "word_separator = '#separator#'\n";
        let out = merge_config(existing, &upd(&[("word_separator", "/,")]));
        assert_eq!(out, "word_separator = /,\n");
    }

    #[test]
    fn merge_config_hash_inside_double_quoted_value_is_not_a_comment() {
        let existing = "font_family = \"Comic # Sans\"\n";
        let out = merge_config(existing, &upd(&[("font_family", "Fira Code")]));
        assert_eq!(out, "font_family = Fira Code\n");
    }

    #[test]
    fn merge_config_preserves_real_comment_after_a_quoted_value_containing_hash() {
        // A '#' inside the quotes is part of the value; the SPACE-separated '#'
        // after the closing quote is the real trailing comment and must survive.
        let existing = "font_family = \"Comic # Sans\"  # actually use this one\n";
        let out = merge_config(existing, &upd(&[("font_family", "Fira Code")]));
        assert_eq!(out, "font_family = Fira Code  # actually use this one\n");
    }

    #[test]
    fn merge_config_semicolon_inside_quotes_is_not_a_comment() {
        let existing = "word_separator = ';sep;'\n";
        let out = merge_config(existing, &upd(&[("word_separator", "/,")]));
        assert_eq!(out, "word_separator = /,\n");
    }

    #[test]
    fn merge_config_appended_missing_key_never_gets_a_comment() {
        // A freshly-appended key has no prior line to read a comment from.
        let existing = "theme = dracula\n";
        let out = merge_config(existing, &upd(&[("opacity", "0.9")]));
        assert_eq!(out, "theme = dracula\nopacity = 0.9\n");
    }

    #[test]
    fn section_preserves_inline_comment_within_profile_section() {
        let existing = "[profile.work]\nfont_size = 18  # laptop screen\n";
        let out = merge_into_section(existing, Some("work"), &upd(&[("font_size", "22")]));
        assert_eq!(out, "[profile.work]\nfont_size = 22  # laptop screen\n");
    }

    // -----------------------------------------------------------------------
    // External-edit guard (protect_against_external_edit /
    // filter_updates_against_baseline) — see that function's doc comment.
    //
    // `filter_updates_against_baseline` takes the "last loaded" baseline as a
    // plain argument rather than reading the process-global snapshot recorded
    // by `parse_config_file`, so these tests exercise the exact same filtering
    // logic `save`/`save_into_section` use without contending over shared
    // global state with other tests running in parallel — the same reason
    // `save`/`save_into_section` themselves (env/fs-dependent) are untested
    // directly elsewhere in this module, in favor of the pure `merge_config`/
    // `merge_into_section` they wrap.
    // -----------------------------------------------------------------------

    #[test]
    fn filter_forwards_everything_when_file_unchanged_since_load() {
        let baseline = "opacity = 0.5\nscrollback = 5000\n";
        let updates = upd(&[("opacity", "0.5"), ("scrollback", "5000")]);
        // `existing` (a fresh read right before save) is identical to the
        // baseline: no external edit happened, so every update is forwarded
        // even though none of them actually changed the value.
        let out = filter_updates_against_baseline(baseline, baseline, None, &updates);
        assert_eq!(out, updates);
    }

    #[test]
    fn filter_drops_stale_key_that_would_clobber_an_external_edit() {
        let baseline = "opacity = 0.5\n";
        // External edit: opacity is now 0.6 on disk, but the in-memory Config
        // (and therefore the settings-UI snapshot) still thinks it's 0.5.
        let existing = "opacity = 0.6\n";
        let updates = upd(&[("opacity", "0.5")]);
        let out = filter_updates_against_baseline(baseline, existing, None, &updates);
        assert!(
            out.is_empty(),
            "an update that only re-affirms the stale baseline value must be \
             dropped so it doesn't stomp the external edit"
        );
    }

    #[test]
    fn filter_keeps_a_genuine_settings_ui_change_even_if_file_diverged() {
        let baseline = "opacity = 0.5\n";
        // Someone hand-edited a DIFFERENT setting in the meantime; opacity
        // itself is untouched on disk...
        let existing = "opacity = 0.5\nscrollback = 9000\n";
        // ...but the settings UI genuinely changed opacity (0.5 -> 0.9): this
        // is a real change and must still be forwarded (last-writer-wins for
        // this narrow same-key case is acceptable).
        let updates = upd(&[("opacity", "0.9")]);
        let out = filter_updates_against_baseline(baseline, existing, None, &updates);
        assert_eq!(out, updates);
    }

    /// The scenario the guard exists for: a settings-panel Save always writes
    /// EVERY `SAVED_KEYS` value (see `App::save_settings`), not a diff. Prove
    /// that a concurrent external edit to a key the UI did NOT touch survives
    /// the save, while a key the UI DID touch is still written.
    #[test]
    fn concurrent_external_edit_to_a_different_key_survives_settings_save() {
        // What the in-memory Config was last loaded from.
        let baseline = "opacity = 0.5\nscrollback = 5000\n";
        // The file right before Save: an external hand-edit bumped scrollback
        // to 9000 inside the watcher's debounce window. opacity is untouched.
        let existing = "opacity = 0.5\nscrollback = 9000\n";
        // The settings UI's full live snapshot: opacity was actually changed
        // (0.5 -> 0.9); scrollback was NOT touched by the user, so it's still
        // the stale 5000 from the last load.
        let updates = upd(&[("opacity", "0.9"), ("scrollback", "5000")]);

        let filtered = filter_updates_against_baseline(baseline, existing, None, &updates);
        let out = merge_config(existing, &filtered);

        assert!(
            out.contains("opacity = 0.9"),
            "the genuine settings-UI change must still be written"
        );
        assert!(
            out.contains("scrollback = 9000"),
            "the external edit to a key the UI didn't touch must survive: {out}"
        );
        assert!(
            !out.contains("scrollback = 5000"),
            "the stale in-memory value must NOT clobber the external edit: {out}"
        );
    }

    #[test]
    fn filter_forwards_new_key_absent_from_baseline() {
        let baseline = "opacity = 0.5\n";
        let existing = "opacity = 0.5\n";
        // `minimap` wasn't a SAVED_KEYS entry (or wasn't set) at the last load —
        // nothing to compare against, so it's forwarded like any other update.
        let updates = upd(&[("minimap", "true")]);
        let out = filter_updates_against_baseline(baseline, existing, None, &updates);
        assert_eq!(out, updates);
    }

    #[test]
    fn filter_forwards_everything_when_target_section_absent_from_baseline() {
        // The baseline predates the `[profile.work]` section entirely (e.g. it
        // was just created via `create_profile_from_current`): there is nothing
        // to guard, so every update goes through.
        let baseline = "theme = dracula\n";
        let existing = "theme = dracula\n\n[profile.work]\nfont_size = 18\n";
        let updates = upd(&[("font_size", "20")]);
        let out = filter_updates_against_baseline(baseline, existing, Some("work"), &updates);
        assert_eq!(out, updates);
    }

    #[test]
    fn filter_within_profile_section_only_compares_that_sections_key() {
        let baseline = "scrollback = 5000\n\n[profile.work]\nfont_size = 18\n";
        // External edit changed the TOP-LEVEL scrollback, not the profile's.
        let existing = "scrollback = 9000\n\n[profile.work]\nfont_size = 18\n";
        // Saving into [profile.work]'s font_size — unrelated to the top-level
        // scrollback edit, so it must be forwarded regardless.
        let updates = upd(&[("font_size", "22")]);
        let out = filter_updates_against_baseline(baseline, existing, Some("work"), &updates);
        assert_eq!(out, updates);
    }

    // -----------------------------------------------------------------------
    // Helper unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn find_inline_comment_start_finds_unquoted_hash() {
        assert_eq!(find_inline_comment_start(" 0.5  # note"), Some(6));
        assert_eq!(find_inline_comment_start(" 0.5  ; note"), Some(6));
        assert_eq!(find_inline_comment_start(" 0.5"), None);
    }

    #[test]
    fn find_inline_comment_start_ignores_hash_inside_quotes() {
        assert_eq!(find_inline_comment_start(" '#not a comment'"), None);
        assert_eq!(find_inline_comment_start(" \"#not a comment\""), None);
        // Real comment AFTER the closing quote is still found.
        assert_eq!(
            find_inline_comment_start(" \"a#b\" # real"),
            Some(" \"a#b\" ".len())
        );
    }

    #[test]
    fn find_inline_comment_start_unterminated_quote_runs_to_end() {
        // No closing quote: the rest of the line (including any '#') is
        // treated as still inside the quote, so no comment is found.
        assert_eq!(
            find_inline_comment_start(" 'unterminated # not a comment"),
            None
        );
    }

    #[test]
    fn split_value_and_comment_splits_on_unquoted_marker() {
        assert_eq!(
            split_value_and_comment(" 0.5  # note"),
            (" 0.5  ", Some("# note"))
        );
        assert_eq!(split_value_and_comment(" 0.5"), (" 0.5", None));
    }

    #[test]
    fn find_key_value_in_range_strips_inline_comment_and_whitespace() {
        let lines: Vec<String> = "opacity = 0.5  # note\nfont_size = 14\n"
            .lines()
            .map(str::to_string)
            .collect();
        let range = (0, lines.len());
        assert_eq!(
            find_key_value_in_range(&lines, range, "opacity"),
            Some("0.5".to_string())
        );
        assert_eq!(
            find_key_value_in_range(&lines, range, "font_size"),
            Some("14".to_string())
        );
        assert_eq!(find_key_value_in_range(&lines, range, "missing"), None);
    }

    #[test]
    fn find_range_none_for_missing_section_some_for_top_level() {
        let lines: Vec<String> = "theme = dracula\n".lines().map(str::to_string).collect();
        assert_eq!(find_range(&lines, None), Some((0, 1)));
        assert_eq!(find_range(&lines, Some("work")), None);
    }

    #[test]
    fn profile_header_name_parses_only_the_profile_prefix() {
        assert_eq!(profile_header_name("[profile.work]"), Some("work".into()));
        assert_eq!(profile_header_name("[profile.Work]"), Some("work".into()));
        assert_eq!(
            profile_header_name("  [profile.work]  "),
            Some("work".into())
        );
        // Case-sensitive "profile." prefix, matching `parse_config_file`.
        assert_eq!(profile_header_name("[Profile.work]"), None);
        assert_eq!(profile_header_name("[keybindings]"), None);
        assert_eq!(profile_header_name("[profile.]"), None);
        assert_eq!(profile_header_name("not a header"), None);
    }

    #[test]
    fn is_section_header_detects_any_bracketed_line() {
        assert!(is_section_header("[profile.work]"));
        assert!(is_section_header("[keybindings]"));
        assert!(is_section_header("  [foo]  "));
        assert!(!is_section_header("theme = dracula"));
        assert!(!is_section_header(""));
        assert!(!is_section_header("[unterminated"));
    }
}

// ---- w15: scrollback memory bounding (Phase 1) — config parsing ------------

#[cfg(test)]
mod scrollback_background_tests {
    use super::*;

    #[test]
    fn scrollback_background_cap_defaults_to_disabled() {
        let raw = RawConfig::default();
        assert_eq!(raw.scrollback_background_cap, None);
        assert_eq!(raw.scrollback_background_idle_secs, None);
        // Unset in RawConfig resolves to the disabled policy (cap == 0) once
        // `into_settings` builds it — see `into_settings_leaves_disabled_policy_by_default`.
    }

    #[test]
    fn scrollback_background_cap_parses_and_clamps() {
        let mut raw = RawConfig::default();
        apply_kv("scrollback_background_cap", "5000", &mut raw).unwrap();
        assert_eq!(raw.scrollback_background_cap, Some(5000));

        // Clamped to SCROLLBACK_MAX, same ceiling as `scrollback` itself.
        apply_kv("scrollback_background_cap", "999999999", &mut raw).unwrap();
        assert_eq!(raw.scrollback_background_cap, Some(SCROLLBACK_MAX));
    }

    #[test]
    fn scrollback_background_cap_rejects_non_integer() {
        let mut raw = RawConfig::default();
        assert!(apply_kv("scrollback_background_cap", "not-a-number", &mut raw).is_err());
    }

    #[test]
    fn scrollback_background_idle_secs_parses_unclamped() {
        let mut raw = RawConfig::default();
        apply_kv("scrollback_background_idle_secs", "3600", &mut raw).unwrap();
        assert_eq!(raw.scrollback_background_idle_secs, Some(3600));
    }

    #[test]
    fn scrollback_max_clamp_lowered_from_legacy_one_million() {
        // The w15 audit lowered `SCROLLBACK_MAX` from a prior 1,000,000; a value
        // that would have survived the old clamp must now be capped lower, so a
        // future change growing it back toward 1,000,000 does so deliberately.
        let mut raw = RawConfig::default();
        apply_kv("scrollback", "999999", &mut raw).unwrap();
        assert_eq!(raw.scrollback, Some(SCROLLBACK_MAX));
    }

    #[test]
    fn into_settings_leaves_disabled_policy_by_default() {
        // `into_settings` resolves + logs the policy but does not (yet) surface
        // it on `Config` — this test just guards that parsing an unset config
        // doesn't panic or error, and implicitly exercises the default-cap path
        // of `ScrollbackBackgroundPolicy::effective_cap` via `into_settings`.
        let raw = RawConfig::default();
        assert!(raw.into_settings().is_ok());
#[cfg(test)]
mod segment_tests {
    //! `parse_status_bar_segments` coverage for the w15 additions (TabCount,
    //! Zoom, Profile, Busy, Hostname, Custom) plus their aliases. Pre-existing
    //! segment tokens are covered by `config::mod`'s `status_bar_segments_*`
    //! integration tests; these are unit tests scoped to this function.
    use super::*;
    use crate::app::StatusBarSegment;

    #[test]
    fn parses_w15_segment_tokens() {
        let segs = parse_status_bar_segments("tab_count zoom profile busy hostname custom");
        assert_eq!(
            segs,
            vec![
                StatusBarSegment::TabCount,
                StatusBarSegment::Zoom,
                StatusBarSegment::Profile,
                StatusBarSegment::Busy,
                StatusBarSegment::Hostname,
                StatusBarSegment::Custom,
            ]
        );
    }

    #[test]
    fn parses_w15_segment_aliases() {
        assert_eq!(
            parse_status_bar_segments("tabs"),
            vec![StatusBarSegment::TabCount]
        );
        assert_eq!(
            parse_status_bar_segments("host"),
            vec![StatusBarSegment::Hostname]
        );
    }

    #[test]
    fn every_segment_token_round_trips_through_display() {
        // Inverse-of-inverse: every canonical token this parser accepts must
        // parse back to the exact segment `token()` reports for it, the
        // invariant `status_bar_segments_display`/`settings_save::SAVED_KEYS`
        // depend on for a clean round trip through the settings form.
        for seg in [
            StatusBarSegment::Cwd,
            StatusBarSegment::GitBranch,
            StatusBarSegment::Process,
            StatusBarSegment::Time,
            StatusBarSegment::Mode,
            StatusBarSegment::Broadcast,
            StatusBarSegment::Selection,
            StatusBarSegment::Scroll,
            StatusBarSegment::Encoding,
            StatusBarSegment::Progress,
            StatusBarSegment::ExitStatus,
            StatusBarSegment::KeyHints,
            StatusBarSegment::TabCount,
            StatusBarSegment::Zoom,
            StatusBarSegment::Profile,
            StatusBarSegment::Busy,
            StatusBarSegment::Hostname,
            StatusBarSegment::Custom,
        ] {
            assert_eq!(parse_status_bar_segments(seg.token()), vec![seg]);
        }
    }
}
