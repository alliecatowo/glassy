//! CLI argument parsing.

use anyhow::{Context, Result, bail};

use super::parse::{
    QUAKE_ANIMATION_MS_MAX, QUAKE_HEIGHT_MAX, QUAKE_HEIGHT_MIN, RawConfig, parse_bool,
    parse_pos_f32,
};
use super::theme_import::import_theme_from_file;

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
pub(super) fn parse_cli(args: impl Iterator<Item = String>, raw: &mut RawConfig) -> Result<bool> {
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
                raw.color_fg = Some(format!(
                    "#{:02x}{:02x}{:02x}",
                    theme.fg.r, theme.fg.g, theme.fg.b
                ));
                raw.color_bg = Some(format!(
                    "#{:02x}{:02x}{:02x}",
                    theme.bg.r, theme.bg.g, theme.bg.b
                ));
                raw.color_cursor = Some(format!(
                    "#{:02x}{:02x}{:02x}",
                    theme.cursor.r, theme.cursor.g, theme.cursor.b
                ));
                raw.color_selection_bg = Some(format!(
                    "#{:02x}{:02x}{:02x}",
                    theme.selection_bg.r, theme.selection_bg.g, theme.selection_bg.b
                ));
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
            "--quake" => {
                // Optional bool value; bare `--quake` means true.
                match args.peek().map(|s| s.as_str()) {
                    Some(v)
                        if matches!(
                            v.to_ascii_lowercase().as_str(),
                            "true" | "yes" | "on" | "1" | "false" | "no" | "off" | "0"
                        ) =>
                    {
                        let v = args.next().unwrap();
                        raw.quake = Some(parse_bool(&v, "--quake")?);
                    }
                    _ => raw.quake = Some(true),
                }
            }
            "--quake-height" => {
                let v = next_value(&mut args, "--quake-height")?;
                let h: f32 = v
                    .parse()
                    .with_context(|| format!("--quake-height: invalid number '{v}'"))?;
                if !(h.is_finite() && (QUAKE_HEIGHT_MIN..=QUAKE_HEIGHT_MAX).contains(&h)) {
                    bail!(
                        "--quake-height must be between {QUAKE_HEIGHT_MIN} and {QUAKE_HEIGHT_MAX}, got {h}"
                    );
                }
                raw.quake_height = Some(h);
            }
            "--quake-animation-ms" => {
                let v = next_value(&mut args, "--quake-animation-ms")?;
                let ms: u64 = v
                    .parse()
                    .with_context(|| format!("--quake-animation-ms: invalid integer '{v}'"))?;
                raw.quake_animation_ms = Some(ms.min(QUAKE_ANIMATION_MS_MAX));
            }
            "--restore-session" => {
                // Optional bool value; bare `--restore-session` means true.
                match args.peek().map(|s| s.as_str()) {
                    Some(v)
                        if matches!(
                            v.to_ascii_lowercase().as_str(),
                            "true" | "yes" | "on" | "1" | "false" | "no" | "off" | "0"
                        ) =>
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
                use alacritty_terminal::tty::Shell;
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
pub(super) fn profile_from_args(args: &[String]) -> Option<String> {
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
    glassy toggle | show | hide        Signal a running instance (quake mode)
    glassy @ <CMD> [ARGS...]           Remote-control a running instance

CONTROL SUBCOMMANDS (for compositor hotkeys — see docs/quake-mode.md):
    toggle, show, hide     Slide the running quake window in/out. Bind one of
                           these to a key in YOUR compositor (Wayland has no
                           portable global hotkey). Also accepted as
                           --toggle / --show / --hide.

REMOTE CONTROL (kitty-style; `glassy msg` is a synonym — see docs/plugins.md):
    ls, open-tab, split [v|h], send-text <TEXT>, set-theme <NAME>,
    focus-tab <N>, list-themes, reload-config, run-action <NAME>,
    get-config <KEY>, set-config <KEY> <VALUE>
                           Drive the running instance over its Unix socket and
                           print the one-line OK/ERR reply (exit 0/1).

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
    --pane-headers <BOOL>  Show per-pane title bars in splits (default false)
    --word-separator <STR> Extra word separators for text selection
    --font-features <LIST> OpenType feature tags, e.g. \"ss01,calt=0\" (comma/space separated)
    --import-theme <PATH>  Import Alacritty/base16 theme from TOML/YAML file
    --profile <NAME>       Activate a [profile.NAME] config section's overrides
    --cwd <PATH>           Working directory for the first tab's shell
    --restore-session [B]  Restore the previous session's tabs/splits (default off)
    --quake [B]            Quake/dropdown mode: borderless top-anchored slide-down
                           window (default off). Toggle with the in-app keybind or
                           bind 'glassy toggle' to a compositor hotkey.
    --quake-height <F>     Quake window height as a fraction of the monitor (0.1..1.0,
                           default 0.5)
    --quake-animation-ms <N> Quake slide duration in ms (0 = instant, default 180)
    -e, --command <CMD>    Run CMD (with the remaining args) instead of the shell
    -h, --help             Print this help and exit
    -V, --version          Print version and exit

CONFIG FILE:
    $XDG_CONFIG_HOME/glassy/glassy.conf  (or ~/.config/glassy/glassy.conf)
    macOS: ~/Library/Application Support/glassy/glassy.conf
    KEY=VALUE lines: font_family, font_size, theme, opacity, padding,
    padding_top, padding_bottom, padding_left, padding_right, shell, scrollback,
    bell_visual, bell_audible, follow_system, theme_light, theme_dark, status_bar,
    pane_headers, word_separator, ligatures, font_features, color.*
    Add a [keybindings] section to remap or disable chords:
        ctrl+shift+n = new_tab
        f11          = none    (disable a built-in bind)
    Actions: new_tab, close_pane, next_tab, prev_tab, split_vertical,
    split_horizontal, toggle_fullscreen, toggle_maximize, settings, help,
    search, command_palette, copy, paste, toggle_status_bar, font_increase,
    font_decrease, font_reset, scroll_up, scroll_down, scroll_top, scroll_bottom,
    quake_toggle",
        env!("CARGO_PKG_VERSION")
    );
}
