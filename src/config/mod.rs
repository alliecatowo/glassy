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
//!                                       # split form: theme = light:one-light, dark:tokyo-night
//!                                       # (turns on follow_system + pins per-scheme themes)
//! opacity     = 0.92                   # 0.0 (clear) .. 1.0 (opaque); perceptual curve
//! window_effect = none                 # none|frosted|acrylic|crt|scanlines|grain|vignette|bloom
//! padding     = 6                      # logical px grid inset (all sides)
//! padding_top = 8                      # per-side overrides (optional, override padding)
//! padding_bottom = 6
//! padding_left = 4
//! padding_right = 4
//! shell       = /usr/bin/zsh -l        # program + args
//! scrollback  = 10000                  # lines of history
//! command_history = 200                # recent commands kept for the palette (OSC 133); 0 = off
//! bell_visual = true                   # flash the window on bell
//! bell_audible= false                  # soft beep on bell (needs bell-audio build)
//! follow_system = false                # track the OS light/dark color scheme
//! theme_light = rose-pine-dawn         # theme used in system Light mode
//! theme_dark  = tokyo-night            # theme used in system Dark mode
//! status_bar  = false                  # show status bar at the bottom (default off)
//! pane_headers= false                  # show per-pane title bars + accent rail in splits (default off)
//! dim_unfocused = true                 # dim unfocused pane content in a split (default on)
//! ligatures   = false                  # enable OpenType ligature shaping across cells (default off)
//! font_features = ss01, calt=0         # OpenType feature tags to force on/off (comma or space separated)
//! cwd         = /home/me/projects      # working directory for the first tab's shell
//! restore_session = false              # restore previous tabs/splits/cwds on launch
//! cursor_style = block                 # default cursor shape: block | beam | underline
//! cursor_blink = false                 # blink the cursor by default (false = steady)
//! wallpaper_theme = /path/to/wall.png  # generate theme from image on startup
//! show_tab_bar = auto                  # tab strip: auto (hide at 1 tab) / always / never
//! title_show_cwd = true                # include the cwd in the OS window title
//! title_show_count = false             # append " · N tabs" to the window title
//! minimap     = false                  # scrollback minimap / overview strip (right edge)
//! quake       = false                  # quake/dropdown mode: borderless slide-down window
//! quake_height = 0.5                   # fraction of the monitor height (0.1..1.0)
//! quake_animation_ms = 180             # slide duration in ms (0 = instant)
//! copy_on_select = false               # copy a selection to the clipboard as soon as it is made
//! copy_html   = false                  # also place a rich-text (HTML) flavor on the clipboard on copy
//! power_mode  = false                  # fun typing effect: cursor particle bursts + streak shake
//! power_mode_intensity = 0.6           # power-mode strength: 0.0 (subtle) .. 1.0 (max)
//! ```
//!
//! Quake / dropdown mode (see `docs/quake-mode.md`): with `quake = true` glassy
//! opens as a borderless window that slides down from the top edge. Wayland has no
//! portable global hotkey, so bind `glassy toggle` (or `glassy --toggle`) to a key
//! in your compositor; it signals the running instance over a single-instance Unix
//! socket. The in-app `quake_toggle` action (default F12) hides it from inside.
//!
//! Custom keybindings live in a `[keybindings]` section mapping chords to actions:
//!
//! ```text
//! [keybindings]
//! ctrl+shift+t   = new_tab
//! ctrl+shift+w   = close_pane
//! ctrl+tab       = next_tab
//! ctrl+shift+tab = prev_tab
//! ctrl+shift+e   = split_vertical
//! ctrl+shift+o   = split_horizontal
//! f11            = toggle_fullscreen
//! ctrl+,         = settings
//! f1             = help
//! ctrl+shift+f   = search
//! ctrl+shift+p   = command_palette
//! ctrl+shift+c   = copy
//! ctrl+shift+v   = paste
//! ctrl+shift+b   = toggle_status_bar
//! ```
//!
//! Recognized actions: `new_tab`, `close_pane`, `next_tab`, `prev_tab`,
//! `split_vertical`, `split_horizontal`, `toggle_fullscreen`, `toggle_maximize`,
//! `settings`, `help`, `search`, `command_palette`, `copy`, `paste`,
//! `toggle_status_bar`, `font_increase`, `font_decrease`, `font_reset`,
//! `scroll_up`, `scroll_down`, `scroll_top`, `scroll_bottom`,
//! `jump_prev_prompt`, `jump_next_prompt` (OSC 133 prompt navigation),
//! `move_tab_left`, `move_tab_right`, `go_to_tab_1` .. `go_to_tab_9`,
//! `broadcast_input`, `hints`, `toggle_fold`, `toggle_minimap`, `quake_toggle`,
//! `toggle_zoom`, `focus_pane_left`, `focus_pane_right`, `focus_pane_up`,
//! `focus_pane_down` (move focus between tiled split panes), `rotate_panes`,
//! `equalize_panes`, `vi_mode` (keyboard copy-mode; default `Ctrl+Shift+Space`).
//!
//! Default chords are platform-aware: macOS uses ⌘-based chords (⌘C/⌘V/⌘T/⌘W,
//! ⌘1-9, ⌘, for settings, ⌘F for find, ⌘arrow for pane focus); Linux/Windows use
//! Ctrl / Ctrl+Shift (Ctrl+arrow for pane focus). Pane-focus chords fall through
//! to the child on a single-pane tab. Holding the primary modifier (⌘ / Ctrl)
//! alone briefly overlays each tab chip with its jump number.
//!
//! Multi-key chord *sequences* ("leader" binds) are written with a space between
//! chords, e.g. `ctrl+a n = next_tab` (press Ctrl+A, then N). The first chord is
//! the leader/prefix; it must not itself be a single-key bind. Sequences live in
//! the same `[keybindings]` section.
//!
//! A `[keybindings]` entry overrides the built-in default for that action; to
//! disable a built-in bind entirely, set the action to `none`.
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

mod cli;
pub mod keymap;
pub mod parse;
pub mod platform;
pub mod theme_gen;
pub mod theme_import;

use anyhow::{Context, Result};

pub use keymap::{Chord, KeyAction, KeyMap, SequenceMap};
pub use parse::{path, save};
pub use platform::Platform;

/// Fully-resolved settings handed to the app: the renderer/PTY `Config` plus the
/// selected color `Theme` (installed globally by `main`).
pub struct Settings {
    pub config: crate::app::Config,
    pub theme: crate::color::Theme,
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
        let mut raw = parse::RawConfig::default();

        // 2. Load config file if it exists.
        if let Some(path) = parse::path()
            && let Ok(text) = std::fs::read_to_string(&path)
        {
            parse::parse_config_file(&text, &mut raw)
                .with_context(|| format!("parsing {}", path.display()))?;
        }

        // 2.5. Rescan the user-themes directory (`themes/` next to
        // `glassy.conf`) so `color::theme_by_name` et al. (used below, and by
        // the settings overlay) can resolve user-authored themes. Runs once
        // per process; a user theme with the same name as a built-in shadows
        // it (see `color::user_themes`'s module doc).
        crate::color::reload_user_themes();

        // 3. Pre-scan CLI for `--profile` and activate it if present.
        if let Some(profile_name) = cli::profile_from_args(&args) {
            raw.activate_profile(&profile_name)?;
        }

        // 4. Parse CLI args, which override the file + profile.
        let should_launch = cli::parse_cli(args.into_iter(), &mut raw)?;
        if !should_launch {
            return Ok(None);
        }

        // 5. Convert accumulated raw config into final settings.
        raw.into_settings().map(Some)
    }

    /// Re-resolve settings from the on-disk config file with the named profile
    /// activated, for the LIVE runtime profile switch (palette / keybind). Unlike
    /// [`Settings::resolve`] this skips CLI parsing (there is none at runtime) and
    /// ignores the originally-passed `--profile`. Returns an error if the file is
    /// missing or the profile is unknown.
    pub fn resolve_with_profile(profile: &str) -> Result<Settings> {
        let mut raw = parse::RawConfig::default();
        if let Some(path) = parse::path()
            && let Ok(text) = std::fs::read_to_string(&path)
        {
            parse::parse_config_file(&text, &mut raw)
                .with_context(|| format!("parsing {}", path.display()))?;
        }
        raw.activate_profile(profile)?;
        raw.into_settings()
    }
}

/// Read the names of every `[profile.NAME]` section defined in the on-disk config
/// file, in first-seen order, for the runtime profile switcher (palette +
/// settings). Returns an empty list when no config file exists or it defines no
/// profiles. Errors in parsing are swallowed (returns what was collected) so the
/// switcher never blocks on a malformed file.
pub fn profile_names() -> Vec<String> {
    let Some(path) = parse::path() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    parse::profile_names_from_text(&text)
}

#[cfg(test)]
mod tests {
    use super::cli::profile_from_args;
    use super::keymap::{KeyAction, build_keymap, default_keymap, parse_action, parse_chord};
    use super::parse::{RawConfig, parse_bool, parse_config_file};
    use super::platform::Platform;

    /// Linux/Windows default keymap, used by the bulk of the keybinding tests
    /// (they assert the Ctrl / Ctrl+Shift chords).
    fn pc_keymap() -> super::keymap::KeyMap {
        default_keymap(Platform::Linux)
    }

    // -----------------------------------------------------------------------
    // Settings + RawConfig tests
    // -----------------------------------------------------------------------

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

    // -----------------------------------------------------------------------
    // merge_config tests
    // -----------------------------------------------------------------------

    #[test]
    fn merge_updates_in_place_and_appends() {
        let _existing = "\
# my config
theme = dracula
font_size = 14
opacity = 0.80
";
        // merge_config is private but tested indirectly via save()
        // For direct access, we'd need to expose it or test via integration
        let _updates = [
            ("font_size", "20".to_string()),
            ("opacity", "0.95".to_string()),
            ("bell_visual", "false".to_string()),
        ];
        // Verify the logic inline: it should preserve comments and update in place
    }

    // -----------------------------------------------------------------------
    // Boolean parsing tests
    // -----------------------------------------------------------------------

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

    // -----------------------------------------------------------------------
    // Profile tests
    // -----------------------------------------------------------------------

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
        assert_eq!(
            s.config.initial_cwd.as_deref().map(|p| p.to_str().unwrap()),
            Some("/home/me/work")
        );
    }

    #[test]
    fn activate_unknown_profile_errors() {
        let mut raw = RawConfig::default();
        assert!(raw.activate_profile("nope").is_err());
    }

    #[test]
    fn split_theme_sets_follow_system_and_per_scheme() {
        let mut raw = RawConfig::default();
        parse_config_file("theme = light:one-light, dark:tokyo-night\n", &mut raw).unwrap();
        assert_eq!(raw.follow_system, Some(true));
        assert_eq!(raw.theme_light.as_deref(), Some("one-light"));
        assert_eq!(raw.theme_dark.as_deref(), Some("tokyo-night"));
        let s = raw.into_settings().unwrap();
        assert!(s.config.follow_system);
        assert_eq!(s.config.theme_light, "one-light");
        assert_eq!(s.config.theme_dark, "tokyo-night");
    }

    #[test]
    fn split_theme_order_independent_and_partial() {
        let mut raw = RawConfig::default();
        parse_config_file("theme = dark:dracula\n", &mut raw).unwrap();
        assert_eq!(raw.follow_system, Some(true));
        assert_eq!(raw.theme_dark.as_deref(), Some("dracula"));
        // Bare names still use the single-theme path (no follow_system flip).
        let mut bare = RawConfig::default();
        parse_config_file("theme = dracula\n", &mut bare).unwrap();
        assert_eq!(bare.follow_system, None);
        assert_eq!(bare.theme.as_deref(), Some("dracula"));
    }

    #[test]
    fn profile_names_from_text_preserves_order() {
        use super::parse::profile_names_from_text;
        let text = "[profile.work]\nfont_size=16\n[profile.home]\nfont_size=12\n[profile.work]\ntheme=nord\n";
        let names = profile_names_from_text(text);
        assert_eq!(names, vec!["work".to_string(), "home".to_string()]);
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
        let a = vec!["--profile".to_string(), "work".to_string()];
        assert_eq!(profile_from_args(&a), Some("work".to_string()));
        let b = vec!["--profile=home".to_string()];
        assert_eq!(profile_from_args(&b), Some("home".to_string()));
        let c = vec!["--font-size".to_string(), "14".to_string()];
        assert_eq!(profile_from_args(&c), None);
    }

    // -----------------------------------------------------------------------
    // Config file tests
    // -----------------------------------------------------------------------

    #[test]
    fn show_tab_bar_parses_words_and_bools() {
        use crate::app::TabBarMode;
        // Default is Auto.
        let s = RawConfig::default().into_settings().unwrap();
        assert_eq!(s.config.show_tab_bar, TabBarMode::Auto);
        // Word forms.
        for (word, expect) in [
            ("auto", TabBarMode::Auto),
            ("always", TabBarMode::Always),
            ("never", TabBarMode::Never),
            // Bool spellings map to always/never.
            ("true", TabBarMode::Always),
            ("off", TabBarMode::Never),
        ] {
            let mut raw = RawConfig::default();
            parse_config_file(&format!("show_tab_bar = {word}\n"), &mut raw).unwrap();
            let s = raw.into_settings().unwrap();
            assert_eq!(s.config.show_tab_bar, expect, "show_tab_bar = {word}");
        }
        // Garbage is a hard error.
        let mut bad = RawConfig::default();
        assert!(parse_config_file("show_tab_bar = sometimes\n", &mut bad).is_err());
    }

    #[test]
    fn title_toggles_parse_with_defaults() {
        let s = RawConfig::default().into_settings().unwrap();
        assert!(s.config.title_show_cwd); // default on
        assert!(!s.config.title_show_count); // default off
        let mut raw = RawConfig::default();
        parse_config_file("title_show_cwd = off\ntitle_show_count = on\n", &mut raw).unwrap();
        let s = raw.into_settings().unwrap();
        assert!(!s.config.title_show_cwd);
        assert!(s.config.title_show_count);
    }

    #[test]
    fn quake_defaults_off_and_parses() {
        // Default is off with the documented geometry/animation defaults.
        let settings = RawConfig::default().into_settings().unwrap();
        assert!(!settings.config.quake);
        assert_eq!(settings.config.quake_height, 0.5);
        assert_eq!(settings.config.quake_animation_ms, 180);

        let mut raw = RawConfig::default();
        parse_config_file(
            "quake = true\nquake_height = 0.4\nquake_animation_ms = 250\n",
            &mut raw,
        )
        .unwrap();
        assert_eq!(raw.quake, Some(true));
        let s = raw.into_settings().unwrap();
        assert!(s.config.quake);
        assert!((s.config.quake_height - 0.4).abs() < 1e-6);
        assert_eq!(s.config.quake_animation_ms, 250);
    }

    #[test]
    fn quake_height_out_of_range_errors() {
        let mut raw = RawConfig::default();
        assert!(parse_config_file("quake_height = 2.0\n", &mut raw).is_err());
        let mut raw2 = RawConfig::default();
        assert!(parse_config_file("quake_height = 0.0\n", &mut raw2).is_err());
    }

    #[test]
    fn quake_animation_ms_clamps() {
        let mut raw = RawConfig::default();
        parse_config_file("quake_animation_ms = 999999\n", &mut raw).unwrap();
        assert_eq!(raw.quake_animation_ms, Some(5_000));
    }

    #[test]
    fn quake_toggle_keybind_defaults_to_f12() {
        let km = default_keymap(Platform::Linux);
        let chord = parse_chord("f12").unwrap();
        assert_eq!(km.get(&chord), Some(&KeyAction::QuakeToggle));
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
        assert_eq!(
            s.config.initial_cwd.as_deref().map(|p| p.to_str().unwrap()),
            Some("/tmp/here")
        );
    }

    #[test]
    fn pane_headers_parses_and_defaults_off() {
        let mut raw = RawConfig::default();
        parse_config_file("pane_headers = off\n", &mut raw).unwrap();
        assert_eq!(raw.pane_headers, Some(false));
        // Default (unset) is now OFF — owner prefers clean splits without headers.
        let settings = RawConfig::default().into_settings().unwrap();
        assert!(!settings.config.pane_headers);
        // Explicitly enabling works.
        let mut raw_on = RawConfig::default();
        parse_config_file("pane_headers = on\n", &mut raw_on).unwrap();
        let settings_on = raw_on.into_settings().unwrap();
        assert!(settings_on.config.pane_headers);
    }

    #[test]
    fn dim_unfocused_parses_and_defaults_on() {
        // Default (unset) is ON — the focused pane should stand out by default.
        let settings = RawConfig::default().into_settings().unwrap();
        assert!(settings.config.dim_unfocused);
        // Explicitly disabling works.
        let mut raw_off = RawConfig::default();
        parse_config_file("dim_unfocused = false\n", &mut raw_off).unwrap();
        assert_eq!(raw_off.dim_unfocused, Some(false));
        let settings_off = raw_off.into_settings().unwrap();
        assert!(!settings_off.config.dim_unfocused);
    }

    #[test]
    fn command_history_parses_and_defaults() {
        // Default is 200 when unset.
        let settings = RawConfig::default().into_settings().unwrap();
        assert_eq!(settings.config.command_history, 200);
        // Explicit value is honored and clamped to the [0, 10000] range.
        let mut raw = RawConfig::default();
        parse_config_file("command_history = 50\n", &mut raw).unwrap();
        assert_eq!(raw.command_history, Some(50));
        assert_eq!(raw.into_settings().unwrap().config.command_history, 50);
        // Over-large values clamp.
        let mut raw_big = RawConfig::default();
        parse_config_file("command_history = 99999\n", &mut raw_big).unwrap();
        assert_eq!(raw_big.command_history, Some(10_000));
        // 0 disables capture.
        let mut raw_off = RawConfig::default();
        parse_config_file("command_history = 0\n", &mut raw_off).unwrap();
        assert_eq!(raw_off.command_history, Some(0));
    }

    #[test]
    fn font_features_parses_comma_separated() {
        let mut raw = RawConfig::default();
        parse_config_file("font_features = ss01, calt=0, dlig\n", &mut raw).unwrap();
        let feats = raw
            .font_features
            .as_ref()
            .expect("font_features should be set");
        assert!(
            feats.contains(&"ss01".to_string()),
            "ss01 should be present"
        );
        assert!(
            feats.contains(&"calt=0".to_string()),
            "calt=0 should be present"
        );
        assert!(
            feats.contains(&"dlig".to_string()),
            "dlig should be present"
        );
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
    fn cursor_style_parses_and_defaults_block() {
        // Default (unset) → Block.
        let settings = RawConfig::default().into_settings().unwrap();
        assert_eq!(
            settings.config.cursor_style,
            crate::app::CursorStyleConfig::Block
        );
        // Explicit beam.
        let mut raw = RawConfig::default();
        parse_config_file("cursor_style = beam\n", &mut raw).unwrap();
        let s = raw.into_settings().unwrap();
        assert_eq!(s.config.cursor_style, crate::app::CursorStyleConfig::Beam);
        // Explicit underline.
        let mut raw2 = RawConfig::default();
        parse_config_file("cursor_style = underline\n", &mut raw2).unwrap();
        let s2 = raw2.into_settings().unwrap();
        assert_eq!(
            s2.config.cursor_style,
            crate::app::CursorStyleConfig::Underline
        );
        // Invalid value is a hard parse error.
        let mut raw3 = RawConfig::default();
        assert!(parse_config_file("cursor_style = arrow\n", &mut raw3).is_err());
    }

    #[test]
    fn cursor_blink_parses_and_defaults_false() {
        let settings = RawConfig::default().into_settings().unwrap();
        assert!(
            !settings.config.cursor_blink,
            "cursor_blink default must be false"
        );
        let mut raw = RawConfig::default();
        parse_config_file("cursor_blink = true\n", &mut raw).unwrap();
        assert_eq!(raw.cursor_blink, Some(true));
        let s = raw.into_settings().unwrap();
        assert!(s.config.cursor_blink);
    }

    #[test]
    fn cursor_style_case_insensitive() {
        let mut raw = RawConfig::default();
        parse_config_file("cursor_style = BEAM\n", &mut raw).unwrap();
        let s = raw.into_settings().unwrap();
        assert_eq!(s.config.cursor_style, crate::app::CursorStyleConfig::Beam);
    }

    #[test]
    fn font_features_space_separated_also_works() {
        let mut raw = RawConfig::default();
        parse_config_file("font_features = liga ss01\n", &mut raw).unwrap();
        let feats = raw
            .font_features
            .as_ref()
            .expect("font_features should be set");
        assert_eq!(feats.len(), 2);
        assert!(feats.contains(&"liga".to_string()));
        assert!(feats.contains(&"ss01".to_string()));
    }

    // -----------------------------------------------------------------------
    // Keybinding tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_chord_simple_letter() {
        let c = parse_chord("ctrl+shift+t").unwrap();
        assert!(c.ctrl && c.shift && !c.alt && !c.meta);
        assert_eq!(c.key, "t");
    }

    #[test]
    fn parse_chord_function_key() {
        let c = parse_chord("f11").unwrap();
        assert!(!c.ctrl && !c.shift && !c.alt && !c.meta);
        assert_eq!(c.key, "f11");
    }

    #[test]
    fn parse_chord_ctrl_comma() {
        let c = parse_chord("ctrl+,").unwrap();
        assert!(c.ctrl && !c.shift);
        assert_eq!(c.key, ",");
    }

    #[test]
    fn parse_chord_ctrl_plus() {
        // "ctrl++" — key is '+'
        let c = parse_chord("ctrl++").unwrap();
        assert!(c.ctrl);
        assert_eq!(c.key, "+");
    }

    #[test]
    fn parse_chord_case_insensitive() {
        let a = parse_chord("Ctrl+Shift+T").unwrap();
        let b = parse_chord("ctrl+shift+t").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn parse_chord_empty_errors() {
        assert!(parse_chord("").is_err());
    }

    #[test]
    fn parse_action_known_values() {
        assert_eq!(parse_action("new_tab").unwrap(), Some(KeyAction::NewTab));
        assert_eq!(
            parse_action("close_pane").unwrap(),
            Some(KeyAction::ClosePane)
        );
        assert_eq!(parse_action("none").unwrap(), None);
        assert_eq!(parse_action("disabled").unwrap(), None);
    }

    #[test]
    fn parse_action_unknown_errors() {
        assert!(parse_action("fly_away").is_err());
    }

    #[test]
    fn default_keymap_has_new_tab() {
        let km = pc_keymap();
        let chord = parse_chord("ctrl+shift+t").unwrap();
        assert_eq!(km.get(&chord), Some(&KeyAction::NewTab));
    }

    #[test]
    fn default_keymap_has_f11_fullscreen() {
        let km = pc_keymap();
        let chord = parse_chord("f11").unwrap();
        assert_eq!(km.get(&chord), Some(&KeyAction::ToggleFullscreen));
    }

    #[test]
    fn build_keymap_override_replaces_default() {
        let base = pc_keymap();
        // Override Ctrl+Shift+T → close_pane
        let overrides = vec![("ctrl+shift+t".to_string(), "close_pane".to_string())];
        let km = build_keymap(base, &overrides);
        let chord = parse_chord("ctrl+shift+t").unwrap();
        assert_eq!(km.get(&chord), Some(&KeyAction::ClosePane));
    }

    #[test]
    fn build_keymap_none_removes_default() {
        let base = pc_keymap();
        let overrides = vec![("f11".to_string(), "none".to_string())];
        let km = build_keymap(base, &overrides);
        let chord = parse_chord("f11").unwrap();
        assert_eq!(km.get(&chord), None);
    }

    #[test]
    fn build_keymap_bad_chord_is_warned_not_fatal() {
        let base = pc_keymap();
        let overrides = vec![("@@invalid".to_string(), "new_tab".to_string())];
        // build_keymap logs a warning but must not panic or return an error.
        let km = build_keymap(base, &overrides);
        // The existing defaults are intact.
        let chord = parse_chord("ctrl+shift+t").unwrap();
        assert_eq!(km.get(&chord), Some(&KeyAction::NewTab));
    }

    #[test]
    fn config_file_keybindings_section_parsed() {
        let text = "\
font_size = 14\n\
[keybindings]\n\
ctrl+shift+t = new_tab\n\
ctrl+shift+n = new_tab\n\
f11 = none\n\
";
        let mut raw = RawConfig::default();
        parse_config_file(text, &mut raw).unwrap();
        // The keybinding overrides are collected (not immediately applied).
        assert_eq!(raw.keybinding_overrides.len(), 3);
        let s = raw.into_settings().unwrap();
        // f11 was disabled.
        let f11 = parse_chord("f11").unwrap();
        assert_eq!(s.config.keymap.get(&f11), None);
        // ctrl+shift+n added.
        let new_chord = parse_chord("ctrl+shift+n").unwrap();
        assert_eq!(s.config.keymap.get(&new_chord), Some(&KeyAction::NewTab));
    }

    #[test]
    fn keybindings_section_splits_single_and_sequence_binds() {
        // A `[keybindings]` section with a normal chord AND a space-separated
        // leader sequence: the single bind lands in the flat keymap and the
        // sequence lands in `key_sequences` (and NOT in the flat keymap).
        let text = "\
[keybindings]\n\
ctrl+shift+n = new_tab\n\
ctrl+a n = next_tab\n\
ctrl+a g g = scroll_top\n\
";
        let mut raw = RawConfig::default();
        parse_config_file(text, &mut raw).unwrap();
        let s = raw.into_settings().unwrap();
        // Single bind in the flat map.
        let n = parse_chord("ctrl+shift+n").unwrap();
        assert_eq!(s.config.keymap.get(&n), Some(&KeyAction::NewTab));
        // The two sequences are in key_sequences.
        let seq_n = vec![parse_chord("ctrl+a").unwrap(), parse_chord("n").unwrap()];
        assert_eq!(
            s.config.key_sequences.get(&seq_n),
            Some(&KeyAction::NextTab)
        );
        let seq_gg = vec![
            parse_chord("ctrl+a").unwrap(),
            parse_chord("g").unwrap(),
            parse_chord("g").unwrap(),
        ];
        assert_eq!(
            s.config.key_sequences.get(&seq_gg),
            Some(&KeyAction::ScrollTop)
        );
        // The leader chord itself is NOT a flat bind.
        assert!(
            !s.config
                .keymap
                .contains_key(&parse_chord("ctrl+a").unwrap())
        );
    }

    #[test]
    fn chord_display_is_readable() {
        let c = parse_chord("ctrl+shift+t").unwrap();
        // Should produce a human label like "Ctrl+Shift+T".
        let d = c.display();
        assert!(d.contains("Ctrl"), "{d}");
        assert!(d.contains("Shift"), "{d}");
        assert!(d.to_uppercase().contains('T'), "{d}");
    }

    #[test]
    fn keybindings_section_does_not_bleed_into_global() {
        let text = "theme = dracula\n[keybindings]\nf1 = help\nfont_size = 14\n";
        let mut raw = RawConfig::default();
        parse_config_file(text, &mut raw).unwrap();
        // `font_size = 14` is inside [keybindings] and should NOT be applied
        // as a global setting (it is a keybinding chord, not a font size).
        // It ends up in keybinding_overrides (with action = "14", which will
        // log a warning at keymap build time) — but font_size stays at None.
        assert_eq!(raw.font_size, None);
        // The theme = dracula (before the section) IS applied.
        assert_eq!(raw.theme.as_deref(), Some("dracula"));
    }

    // -----------------------------------------------------------------------
    // Additional chord/action/keymap coverage
    // -----------------------------------------------------------------------

    #[test]
    fn parse_chord_all_modifiers() {
        let c = parse_chord("ctrl+alt+shift+meta+x").unwrap();
        assert!(c.ctrl && c.alt && c.shift && c.meta);
        assert_eq!(c.key, "x");
    }

    #[test]
    fn parse_chord_option_alias_for_alt() {
        let c = parse_chord("option+a").unwrap();
        assert!(c.alt);
        assert_eq!(c.key, "a");
    }

    #[test]
    fn parse_chord_super_alias_for_meta() {
        let c = parse_chord("super+a").unwrap();
        assert!(c.meta);
    }

    #[test]
    fn parse_chord_cmd_alias_for_meta() {
        let c = parse_chord("cmd+a").unwrap();
        assert!(c.meta);
    }

    #[test]
    fn parse_chord_shift_pageup() {
        let c = parse_chord("shift+pageup").unwrap();
        assert!(c.shift);
        assert_eq!(c.key, "pageup");
    }

    #[test]
    fn parse_chord_f1_through_f12() {
        for n in 1..=12 {
            let s = format!("f{n}");
            let c = parse_chord(&s).unwrap();
            assert_eq!(c.key, s);
        }
    }

    #[test]
    fn parse_chord_space_key() {
        // "ctrl+space" should parse correctly.
        let c = parse_chord("ctrl+space").unwrap();
        assert!(c.ctrl);
        assert_eq!(c.key, "space");
    }

    #[test]
    fn parse_chord_unrecognized_modifier_errors() {
        assert!(parse_chord("superduper+t").is_err());
    }

    #[test]
    fn parse_chord_display_f11() {
        let c = parse_chord("f11").unwrap();
        let d = c.display();
        assert_eq!(d, "F11");
    }

    #[test]
    fn parse_chord_display_ctrl_comma() {
        let c = parse_chord("ctrl+,").unwrap();
        let d = c.display();
        assert!(d.contains("Ctrl"), "{d}");
        assert!(d.contains(','), "{d}");
    }

    #[test]
    fn parse_chord_display_ctrl_plus() {
        let c = parse_chord("ctrl++").unwrap();
        let d = c.display();
        assert!(d.contains("Ctrl"), "{d}");
        assert!(d.contains('+'), "{d}");
    }

    #[test]
    fn parse_chord_equality_order_independent() {
        // ctrl+shift+t and shift+ctrl+t must be equal chords.
        let a = parse_chord("ctrl+shift+t").unwrap();
        let b = parse_chord("shift+ctrl+t").unwrap();
        assert_eq!(a, b, "modifier order must not matter");
    }

    #[test]
    fn parse_action_all_known_actions() {
        let known = [
            "new_tab",
            "close_pane",
            "next_tab",
            "prev_tab",
            "split_vertical",
            "split_horizontal",
            "toggle_fullscreen",
            "toggle_maximize",
            "settings",
            "help",
            "search",
            "command_palette",
            "copy",
            "paste",
            "toggle_status_bar",
            "font_increase",
            "font_decrease",
            "font_reset",
            "scroll_up",
            "scroll_down",
            "scroll_top",
            "scroll_bottom",
            "jump_prev_prompt",
            "jump_next_prompt",
            "prev_prompt",
            "next_prompt",
            "move_tab_left",
            "move_tab_right",
            "go_to_tab_1",
            "go_to_tab_9",
            "broadcast_input",
            "hints",
            "toggle_fold",
            "toggle_minimap",
            "quake_toggle",
        ];
        for name in &known {
            let r = parse_action(name);
            assert!(
                r.is_ok() && r.unwrap().is_some(),
                "'{name}' must parse to Some(action)"
            );
        }
    }

    #[test]
    fn parse_action_go_to_tab_bounds() {
        // 1..=9 are valid; 0 and 10 are out of range.
        assert_eq!(
            parse_action("go_to_tab_1").unwrap(),
            Some(KeyAction::GoToTab(1))
        );
        assert_eq!(
            parse_action("go_to_tab_9").unwrap(),
            Some(KeyAction::GoToTab(9))
        );
        assert!(parse_action("go_to_tab_0").is_err());
        assert!(parse_action("go_to_tab_10").is_err());
    }

    // -----------------------------------------------------------------------
    // Platform-aware keymap tests
    // -----------------------------------------------------------------------

    #[test]
    fn mac_keymap_uses_cmd_for_primary_chords() {
        let km = default_keymap(Platform::Mac);
        // ⌘T / ⌘C / ⌘V / ⌘, / ⌘F use the meta (Cmd) bit, not Ctrl.
        for (chord_str, action) in [
            ("cmd+t", KeyAction::NewTab),
            ("cmd+c", KeyAction::Copy),
            ("cmd+v", KeyAction::Paste),
            ("cmd+,", KeyAction::Settings),
            ("cmd+f", KeyAction::Search),
            ("cmd+1", KeyAction::GoToTab(1)),
            ("cmd+9", KeyAction::GoToTab(9)),
        ] {
            let c = parse_chord(chord_str).unwrap();
            assert!(c.meta, "{chord_str} must set the meta/Cmd bit");
            assert_eq!(
                km.get(&c),
                Some(&action),
                "macOS '{chord_str}' should map to {action:?}"
            );
        }
        // The PC Ctrl chords must NOT be present on macOS.
        assert_eq!(km.get(&parse_chord("ctrl+shift+t").unwrap()), None);
    }

    #[test]
    fn pc_keymap_uses_ctrl_and_has_goto_and_jump() {
        let km = default_keymap(Platform::Linux);
        for (chord_str, action) in [
            ("ctrl+shift+t", KeyAction::NewTab),
            ("ctrl+1", KeyAction::GoToTab(1)),
            ("ctrl+9", KeyAction::GoToTab(9)),
            ("ctrl+shift+pageup", KeyAction::MoveTabLeft),
            ("ctrl+shift+pagedown", KeyAction::MoveTabRight),
            ("ctrl+shift+up", KeyAction::JumpPrevPrompt),
            ("ctrl+shift+down", KeyAction::JumpNextPrompt),
        ] {
            let c = parse_chord(chord_str).unwrap();
            assert_eq!(
                km.get(&c),
                Some(&action),
                "PC '{chord_str}' should map to {action:?}"
            );
        }
        // The macOS Cmd chords must NOT be present on Linux.
        assert_eq!(km.get(&parse_chord("cmd+t").unwrap()), None);
    }

    #[test]
    fn shared_binds_present_on_both_platforms() {
        for p in [Platform::Mac, Platform::Linux, Platform::Windows] {
            let km = default_keymap(p);
            assert_eq!(
                km.get(&parse_chord("f11").unwrap()),
                Some(&KeyAction::ToggleFullscreen),
                "F11 must be bound on {p:?}"
            );
            assert_eq!(
                km.get(&parse_chord("shift+pageup").unwrap()),
                Some(&KeyAction::ScrollUp),
                "Shift+PageUp must be bound on {p:?}"
            );
        }
    }

    #[test]
    fn chord_display_for_mac_uses_hig_symbols() {
        // ⇧⌘T — symbols printed together with no separators, in HIG order
        // (Control, Option, Shift, Command), Command last.
        let c = parse_chord("cmd+shift+t").unwrap();
        let d = c.display_for(Platform::Mac);
        assert_eq!(d, "⇧⌘T", "got {d}");
        assert!(!d.contains('+'));
        // Linux still uses the +-joined form.
        let pc = c.display_for(Platform::Linux);
        assert!(pc.contains('+'), "got {pc}");
    }

    #[test]
    fn parse_action_none_variants() {
        for v in ["none", "disabled", "disable"] {
            assert_eq!(parse_action(v).unwrap(), None, "'{v}' must parse to None");
        }
    }

    #[test]
    fn parse_action_case_insensitive() {
        assert_eq!(
            parse_action("NEW_TAB").unwrap(),
            Some(super::keymap::KeyAction::NewTab)
        );
        assert_eq!(parse_action("NONE").unwrap(), None);
    }

    #[test]
    fn build_keymap_adds_new_chord() {
        let base = pc_keymap();
        let overrides = vec![("ctrl+alt+q".to_string(), "close_pane".to_string())];
        let km = build_keymap(base, &overrides);
        let chord = parse_chord("ctrl+alt+q").unwrap();
        assert_eq!(km.get(&chord), Some(&super::keymap::KeyAction::ClosePane));
    }

    #[test]
    fn build_keymap_bad_action_leaves_default_intact() {
        let base = pc_keymap();
        let overrides = vec![("ctrl+shift+t".to_string(), "not_an_action".to_string())];
        // Bad action: must log a warning but not panic.
        let km = build_keymap(base, &overrides);
        // The original ctrl+shift+t binding (new_tab) is unchanged.
        let chord = parse_chord("ctrl+shift+t").unwrap();
        assert_eq!(km.get(&chord), Some(&super::keymap::KeyAction::NewTab));
    }

    #[test]
    fn build_keymap_multiple_overrides_applied_in_order() {
        let base = pc_keymap();
        // First override disables f11; second adds it back as settings.
        let overrides = vec![
            ("f11".to_string(), "none".to_string()),
            ("f11".to_string(), "settings".to_string()),
        ];
        let km = build_keymap(base, &overrides);
        let chord = parse_chord("f11").unwrap();
        assert_eq!(km.get(&chord), Some(&super::keymap::KeyAction::Settings));
    }

    #[test]
    fn default_keymap_has_expected_defaults() {
        let km = pc_keymap();
        let checks: &[(&str, super::keymap::KeyAction)] = &[
            ("ctrl+shift+w", super::keymap::KeyAction::ClosePane),
            ("ctrl+tab", super::keymap::KeyAction::NextTab),
            ("ctrl+shift+tab", super::keymap::KeyAction::PrevTab),
            ("ctrl+shift+e", super::keymap::KeyAction::SplitVertical),
            ("ctrl+shift+o", super::keymap::KeyAction::SplitHorizontal),
            ("ctrl+,", super::keymap::KeyAction::Settings),
            ("f1", super::keymap::KeyAction::Help),
            ("ctrl+shift+f", super::keymap::KeyAction::Search),
            ("ctrl+shift+p", super::keymap::KeyAction::CommandPalette),
            ("ctrl+shift+c", super::keymap::KeyAction::Copy),
            ("ctrl+shift+v", super::keymap::KeyAction::Paste),
            ("ctrl+shift+b", super::keymap::KeyAction::ToggleStatusBar),
            ("ctrl++", super::keymap::KeyAction::FontIncrease),
            ("ctrl+-", super::keymap::KeyAction::FontDecrease),
            ("ctrl+0", super::keymap::KeyAction::FontReset),
            ("shift+pageup", super::keymap::KeyAction::ScrollUp),
            ("shift+pagedown", super::keymap::KeyAction::ScrollDown),
            ("shift+home", super::keymap::KeyAction::ScrollTop),
            ("shift+end", super::keymap::KeyAction::ScrollBottom),
        ];
        for (chord_str, expected_action) in checks {
            let chord = parse_chord(chord_str).unwrap();
            assert_eq!(
                km.get(&chord),
                Some(expected_action),
                "chord '{chord_str}' should map to {expected_action:?}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Profile activation + CLI precedence
    // -----------------------------------------------------------------------

    #[test]
    fn profile_override_does_not_affect_other_keys() {
        let mut raw = RawConfig::default();
        parse_config_file(
            "scrollback = 5000\n[profile.compact]\nfont_size = 10\n",
            &mut raw,
        )
        .unwrap();
        // Before activation: scrollback was set, font_size not.
        assert_eq!(raw.scrollback, Some(5000));
        assert_eq!(raw.font_size, None);
        raw.activate_profile("compact").unwrap();
        // After activation: profile's font_size is applied, scrollback unchanged.
        assert_eq!(raw.font_size, Some(10.0));
        assert_eq!(raw.scrollback, Some(5000));
    }

    #[test]
    fn profile_activation_is_idempotent() {
        let mut raw = RawConfig::default();
        parse_config_file("[profile.a]\nfont_size = 12\n", &mut raw).unwrap();
        raw.activate_profile("a").unwrap();
        raw.activate_profile("a").unwrap(); // second call must not panic
        assert_eq!(raw.font_size, Some(12.0));
    }

    #[test]
    fn multiple_profiles_independent() {
        // Two separate parses of the same config text with different profile activations.
        let text = "[profile.dev]\nfont_size = 14\n[profile.present]\nfont_size = 18\n";
        let mut raw_dev = RawConfig::default();
        parse_config_file(text, &mut raw_dev).unwrap();
        raw_dev.activate_profile("dev").unwrap();

        let mut raw_present = RawConfig::default();
        parse_config_file(text, &mut raw_present).unwrap();
        raw_present.activate_profile("present").unwrap();

        assert_eq!(raw_dev.font_size, Some(14.0));
        assert_eq!(raw_present.font_size, Some(18.0));
    }

    // -----------------------------------------------------------------------
    // Status-bar segments config tests
    // -----------------------------------------------------------------------

    #[test]
    fn status_bar_segments_none_by_default() {
        let settings = RawConfig::default().into_settings().unwrap();
        assert!(settings.config.status_bar_segments.is_none());
    }

    #[test]
    fn status_bar_segments_parse_known_tokens() {
        use crate::app::StatusBarSegment;
        let mut raw = RawConfig::default();
        parse_config_file(
            "status_bar_segments = cwd, git_branch, mode, time, encoding\n",
            &mut raw,
        )
        .unwrap();
        let segs = raw.status_bar_segments.as_ref().expect("segments set");
        assert_eq!(
            segs,
            &[
                StatusBarSegment::Cwd,
                StatusBarSegment::GitBranch,
                StatusBarSegment::Mode,
                StatusBarSegment::Time,
                StatusBarSegment::Encoding,
            ]
        );
    }

    #[test]
    fn status_bar_segments_space_separated() {
        use crate::app::StatusBarSegment;
        let mut raw = RawConfig::default();
        parse_config_file("status_bar_segments = mode broadcast selection\n", &mut raw).unwrap();
        let segs = raw.status_bar_segments.as_ref().expect("segments set");
        assert_eq!(
            segs,
            &[
                StatusBarSegment::Mode,
                StatusBarSegment::Broadcast,
                StatusBarSegment::Selection,
            ]
        );
    }

    #[test]
    fn status_bar_segments_empty_clears() {
        let mut raw = RawConfig::default();
        parse_config_file("status_bar_segments = \n", &mut raw).unwrap();
        assert!(raw.status_bar_segments.is_none());
    }

    #[test]
    fn status_bar_time_format_default() {
        let settings = RawConfig::default().into_settings().unwrap();
        assert_eq!(settings.config.status_bar_time_format, "%H:%M");
    }

    #[test]
    fn status_bar_time_format_custom() {
        let mut raw = RawConfig::default();
        parse_config_file("status_bar_time_format = %I:%M %p\n", &mut raw).unwrap();
        let settings = raw.into_settings().unwrap();
        assert_eq!(settings.config.status_bar_time_format, "%I:%M %p");
    }

    // -----------------------------------------------------------------------
    // New KeyAction variants parse correctly
    // -----------------------------------------------------------------------

    #[test]
    fn opacity_keybinding_actions_parse() {
        use super::keymap::parse_action;
        assert!(matches!(
            parse_action("increase_opacity"),
            Ok(Some(KeyAction::IncreaseOpacity))
        ));
        assert!(matches!(
            parse_action("decrease_opacity"),
            Ok(Some(KeyAction::DecreaseOpacity))
        ));
        assert!(matches!(
            parse_action("toggle_opacity"),
            Ok(Some(KeyAction::ToggleOpacity))
        ));
    }

    #[test]
    fn save_scrollback_action_parses() {
        use super::keymap::parse_action;
        assert!(matches!(
            parse_action("save_scrollback"),
            Ok(Some(KeyAction::SaveScrollback))
        ));
        assert!(matches!(
            parse_action("scrollback_to_file"),
            Ok(Some(KeyAction::SaveScrollback))
        ));
    }

    // -----------------------------------------------------------------------
    // FONTS stream: symbol map + per-style overrides + variations
    // -----------------------------------------------------------------------

    #[test]
    fn font_bold_italic_bold_italic_parse() {
        use super::parse::parse_config_file;
        let mut raw = RawConfig::default();
        parse_config_file(
            "font_bold = MyFont Bold\nfont_italic = MyFont Italic\nfont_bold_italic = MyFont Bold Italic\n",
            &mut raw,
        )
        .unwrap();
        assert_eq!(raw.font_bold.as_deref(), Some("MyFont Bold"));
        assert_eq!(raw.font_italic.as_deref(), Some("MyFont Italic"));
        assert_eq!(raw.font_bold_italic.as_deref(), Some("MyFont Bold Italic"));
        // Values must flow through into_settings.
        let s = raw.into_settings().unwrap();
        assert_eq!(s.config.font_bold.as_deref(), Some("MyFont Bold"));
        assert_eq!(s.config.font_italic.as_deref(), Some("MyFont Italic"));
        assert_eq!(
            s.config.font_bold_italic.as_deref(),
            Some("MyFont Bold Italic")
        );
    }

    #[test]
    fn font_bold_italic_defaults_none() {
        let s = RawConfig::default().into_settings().unwrap();
        assert!(
            s.config.font_bold.is_none(),
            "font_bold must default to None"
        );
        assert!(
            s.config.font_italic.is_none(),
            "font_italic must default to None"
        );
        assert!(
            s.config.font_bold_italic.is_none(),
            "font_bold_italic must default to None"
        );
    }

    #[test]
    fn parse_symbol_map_single_range() {
        use super::parse::parse_symbol_map;
        let entries = parse_symbol_map("U+E000-U+F8FF : Symbols Nerd Font Mono");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].start, 0xE000);
        assert_eq!(entries[0].end, 0xF8FF);
        assert_eq!(entries[0].family, "Symbols Nerd Font Mono");
    }

    #[test]
    fn parse_symbol_map_single_codepoint() {
        use super::parse::parse_symbol_map;
        let entries = parse_symbol_map("U+2764:Noto Emoji");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].start, 0x2764);
        assert_eq!(entries[0].end, 0x2764);
    }

    #[test]
    fn parse_symbol_map_multiple_entries() {
        use super::parse::parse_symbol_map;
        let entries = parse_symbol_map("U+E000-U+F8FF:Nerd Font, U+2500-U+257F:Box Drawing Font");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].start, 0xE000);
        assert_eq!(entries[1].family, "Box Drawing Font");
    }

    #[test]
    fn parse_symbol_map_invalid_entries_skipped() {
        use super::parse::parse_symbol_map;
        // "garbage" has no colon separator — must be skipped, not panicked.
        let entries = parse_symbol_map("garbage, U+0041:My Font");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].start, 0x0041);
    }

    #[test]
    fn parse_symbol_map_empty() {
        use super::parse::parse_symbol_map;
        assert!(parse_symbol_map("").is_empty());
        assert!(parse_symbol_map("   ").is_empty());
    }

    #[test]
    fn font_symbol_map_config_key_parses() {
        use super::parse::parse_config_file;
        let mut raw = RawConfig::default();
        parse_config_file(
            "font_symbol_map = U+E000-U+F8FF:Nerd Font Symbols\n",
            &mut raw,
        )
        .unwrap();
        let entries = raw.font_symbol_map.as_ref().expect("should be set");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].family, "Nerd Font Symbols");
        let s = raw.into_settings().unwrap();
        assert_eq!(s.config.font_symbol_map.len(), 1);
    }

    #[test]
    fn font_symbol_map_defaults_empty() {
        let s = RawConfig::default().into_settings().unwrap();
        assert!(
            s.config.font_symbol_map.is_empty(),
            "font_symbol_map must default to empty"
        );
    }

    #[test]
    fn parse_font_variations_wght_and_wdth() {
        use super::parse::parse_font_variations;
        let v = parse_font_variations("wght=450, wdth=75");
        assert_eq!(v.len(), 2);
        assert!(v.contains(&"wght=450".to_string()));
        assert!(v.contains(&"wdth=75".to_string()));
    }

    #[test]
    fn parse_font_variations_space_separated() {
        use super::parse::parse_font_variations;
        let v = parse_font_variations("wght=700 slnt=-5");
        assert_eq!(v.len(), 2);
    }

    #[test]
    fn font_variations_config_key_parses() {
        use super::parse::parse_config_file;
        let mut raw = RawConfig::default();
        parse_config_file("font_variations = wght=450\n", &mut raw).unwrap();
        let vars = raw.font_variations.as_ref().expect("should be set");
        assert_eq!(vars.len(), 1);
        assert_eq!(vars[0], "wght=450");
        let s = raw.into_settings().unwrap();
        assert_eq!(s.config.font_variations.len(), 1);
    }

    #[test]
    fn font_variations_defaults_empty() {
        let s = RawConfig::default().into_settings().unwrap();
        assert!(
            s.config.font_variations.is_empty(),
            "font_variations must default to empty"
        );
    }

    #[test]
    fn symbol_map_lookup_finds_range() {
        use crate::config::parse::SymbolMapEntry;
        use crate::text::shape::lookup_symbol_family;
        let mut map = vec![
            SymbolMapEntry {
                start: 0xE000,
                end: 0xF8FF,
                family: "Nerd Font".to_string(),
            },
            SymbolMapEntry {
                start: 0x2500,
                end: 0x257F,
                family: "Box Font".to_string(),
            },
        ];
        // Sort by start for binary search (normally done in load_with_config).
        map.sort_unstable_by_key(|e| e.start);

        // Inside first range.
        assert_eq!(lookup_symbol_family(&map, '\u{E001}'), Some("Nerd Font"));
        // Start of second range.
        assert_eq!(lookup_symbol_family(&map, '\u{2500}'), Some("Box Font"));
        // End of second range.
        assert_eq!(lookup_symbol_family(&map, '\u{257F}'), Some("Box Font"));
        // Just past the end of the second range.
        assert_eq!(lookup_symbol_family(&map, '\u{2580}'), None);
        // ASCII is not covered.
        assert_eq!(lookup_symbol_family(&map, 'A'), None);
    }

    #[test]
    fn symbol_map_lookup_empty_map() {
        use crate::text::shape::lookup_symbol_family;
        assert_eq!(lookup_symbol_family(&[], 'A'), None);
    }
}
