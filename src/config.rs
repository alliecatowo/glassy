//! Configuration: a hand-rolled `KEY=VALUE` config file parser plus a small CLI
//! argument parser layered on top (CLI overrides the file).
//!
//! The config file lives at `$XDG_CONFIG_HOME/glassy/glassy.conf` (falling back
//! to `~/.config/glassy/glassy.conf`). Recognized keys:
//!
//! ```text
//! font_family = FiraCode Nerd Font Mono
//! font_size   = 14
//! theme       = tokyo-night            # or: catppuccin-mocha
//! opacity     = 0.92                   # 0.0 (clear) .. 1.0 (opaque)
//! padding     = 6                      # logical px grid inset
//! shell       = /usr/bin/zsh -l        # program + args
//! scrollback  = 10000                  # lines of history
//! bell_visual = true                   # flash the window on bell
//! bell_audible= false                  # soft beep on bell (needs bell-audio build)
//! ```
//!
//! CLI flags override the file: at minimum `--font-size <pt>`, `--opacity <f>`,
//! and `-e <cmd> [args…]` (run a command instead of the shell). `--help` and
//! `--version` print and exit.

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
        // 1. Start from defaults.
        let mut raw = RawConfig::default();

        // 2. Layer the config file (if present and readable).
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

        // 3. Layer CLI overrides (and handle --help/--version).
        if !parse_cli(args, &mut raw)? {
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
    shell: Option<Shell>,
    scrollback: Option<usize>,
    bell_visual: Option<bool>,
    bell_audible: Option<bool>,
}

impl RawConfig {
    fn into_settings(self) -> Result<Settings> {
        let theme = match self.theme {
            Some(name) => color::theme_by_name(&name).unwrap_or_else(|| {
                log::warn!("glassy: unknown theme '{name}'; using Tokyo Night");
                color::theme_by_name("tokyo-night").expect("default theme exists")
            }),
            None => color::theme_by_name("tokyo-night").expect("default theme exists"),
        };

        let config = Config {
            font_family: self.font_family,
            font_size: self.font_size.unwrap_or(DEFAULT_FONT_SIZE),
            opacity: self.opacity.unwrap_or(DEFAULT_OPACITY).clamp(0.0, 1.0),
            padding: self.padding,
            scrollback: self.scrollback.unwrap_or(DEFAULT_SCROLLBACK),
            shell: self.shell,
            bell_visual: self.bell_visual.unwrap_or(true),
            bell_audible: self.bell_audible.unwrap_or(false),
        };

        Ok(Settings { config, theme })
    }
}

/// The resolved config file path, honoring `$XDG_CONFIG_HOME` then `$HOME`.
fn config_path() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return Some(PathBuf::from(xdg).join("glassy/glassy.conf"));
        }
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config/glassy/glassy.conf"))
}

/// Parse a `KEY=VALUE` config file into `raw`. Blank lines and `#`/`;` comments
/// are ignored; surrounding whitespace and a single layer of matching quotes are
/// stripped from values. An unknown key is warned about but not fatal; a value
/// that fails to parse for a known key is a hard error (with the line number).
fn parse_config_file(text: &str, raw: &mut RawConfig) -> Result<()> {
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            bail!("line {}: expected KEY=VALUE, got '{line}'", i + 1);
        };
        let key = key.trim().to_ascii_lowercase();
        let value = unquote(value.trim());
        apply_kv(&key, value, raw).with_context(|| format!("line {}", i + 1))?;
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
        "shell" => {
            if let Some(shell) = parse_shell(value) {
                raw.shell = Some(shell);
            }
        }
        "scrollback" => {
            let n: usize = value
                .parse()
                .with_context(|| format!("scrollback: invalid integer '{value}'"))?;
            raw.scrollback = Some(n);
        }
        "bell_visual" => {
            raw.bell_visual = Some(parse_bool(value, "bell_visual")?);
        }
        "bell_audible" => {
            raw.bell_audible = Some(parse_bool(value, "bell_audible")?);
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

/// Parse CLI arguments, overriding fields in `raw`.
///
/// Returns `Ok(true)` to continue launching, `Ok(false)` when `--help`/`--version`
/// was handled (caller should exit successfully), or an error on a bad flag.
///
/// Recognized: `--font-size <pt>`, `--font-family <name>`, `--theme <name>`,
/// `--opacity <f>`, `--padding <px>`, `--scrollback <n>`, `--bell-visual <bool>`,
/// `--bell-audible <bool>`, `-e/--command <cmd…>` (consumes the rest of the args
/// as the program + its arguments), `-h/--help`, `-V/--version`.
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
    -e, --command <CMD>    Run CMD (with the remaining args) instead of the shell
    -h, --help             Print this help and exit
    -V, --version          Print version and exit

CONFIG FILE:
    $XDG_CONFIG_HOME/glassy/glassy.conf  (or ~/.config/glassy/glassy.conf)
    KEY=VALUE lines: font_family, font_size, theme, opacity, padding,
    shell, scrollback, bell_visual, bell_audible. CLI flags override the file.",
        env!("CARGO_PKG_VERSION")
    );
}

#[cfg(test)]
mod tests {
    use super::{RawConfig, parse_bool, parse_config_file};

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
}
