//! The single source of truth mapping theme names/aliases to [`Theme`] values.
//!
//! [`BUILTIN_THEMES`] is the canonical list of the 18 themes glassy ships with:
//! each entry carries its canonical display name, the extra aliases it accepts,
//! whether it's a LIGHT theme, and the [`Theme`] color data itself. This
//! replaces what used to be four independently hand-maintained lists
//! (`theme_by_name`'s `match`, the `THEME_NAMES` const slice, `is_light`'s
//! `match`, and `canonical_name`'s `match`) that had to be kept in sync by hand
//! on every new theme.
//!
//! [`super::user_themes`] layers user-authored themes (loaded from the config
//! directory's `themes/` folder) on top: [`theme_by_name`], [`canonical_name`],
//! and [`is_light`] all check user themes FIRST, so a user theme can shadow a
//! built-in of the same name.

use super::Theme;
use super::builtin::*;

/// One theme's full identity: its canonical display name, accepted aliases,
/// light/dark flag, and color data.
///
/// `Copy` (all fields are `&'static` references, a `bool`, or the `Copy`
/// [`Theme`]) so a value can be handed out from behind the user-themes lock
/// (see [`super::user_themes::find`]/[`super::user_themes::snapshot`]) without
/// holding it — the caller gets an owned snapshot, not a borrow.
#[derive(Clone, Copy)]
pub struct ThemeEntry {
    /// Canonical display name — what gets stored in config and shown in the
    /// UI, e.g. `"tokyo-night"`.
    pub canonical: &'static str,
    /// Extra accepted names beyond the canonical one, already normalized
    /// (lowercase, alphanumeric-only — see [`normalize`]), e.g. `"mocha"` for
    /// Catppuccin Mocha. The canonical name's own normalized form always
    /// matches too, so it does not need to be repeated here.
    pub aliases: &'static [&'static str],
    /// True for a LIGHT theme (light background, dark text). Read by
    /// [`is_light`]; not yet consumed elsewhere (same forward-looking status as
    /// the pre-registry `is_light` had).
    #[allow(dead_code)]
    pub light: bool,
    /// The theme's color data.
    pub theme: Theme,
}

/// The 18 built-in themes, in the order they've always shipped in (this order
/// is user-visible: it's the settings dropdown / theme-cycle / palette order).
/// Aliases and light flags exactly match the pre-registry hand-written `match`
/// arms in the old `theme_by_name` / `canonical_name` / `is_light` — see
/// `every_builtin_name_and_alias_resolves` below, which encodes them as a test
/// so a future edit can't silently drop one.
pub const BUILTIN_THEMES: &[ThemeEntry] = &[
    ThemeEntry {
        canonical: "tokyo-night",
        aliases: &["tokyo"],
        light: false,
        theme: TOKYO_NIGHT,
    },
    ThemeEntry {
        canonical: "catppuccin-mocha",
        aliases: &["catppuccin", "mocha"],
        light: false,
        theme: CATPPUCCIN_MOCHA,
    },
    ThemeEntry {
        canonical: "catppuccin-macchiato",
        aliases: &["macchiato"],
        light: false,
        theme: CATPPUCCIN_MACCHIATO,
    },
    ThemeEntry {
        canonical: "gruvbox-dark",
        aliases: &["gruvbox"],
        light: false,
        theme: GRUVBOX_DARK,
    },
    ThemeEntry {
        canonical: "dracula",
        aliases: &[],
        light: false,
        theme: DRACULA,
    },
    ThemeEntry {
        canonical: "nord",
        aliases: &[],
        light: false,
        theme: NORD,
    },
    ThemeEntry {
        canonical: "solarized-dark",
        aliases: &["solarized"],
        light: false,
        theme: SOLARIZED_DARK,
    },
    ThemeEntry {
        canonical: "rose-pine",
        aliases: &["rose"],
        light: false,
        theme: ROSE_PINE,
    },
    ThemeEntry {
        canonical: "rose-pine-dawn",
        aliases: &["dawn"],
        light: true,
        theme: ROSE_PINE_DAWN,
    },
    ThemeEntry {
        canonical: "catppuccin-latte",
        aliases: &["latte"],
        light: true,
        theme: CATPPUCCIN_LATTE,
    },
    ThemeEntry {
        canonical: "everforest-dark",
        aliases: &["everforest"],
        light: false,
        theme: EVERFOREST_DARK,
    },
    ThemeEntry {
        canonical: "everforest-light",
        aliases: &[],
        light: true,
        theme: EVERFOREST_LIGHT,
    },
    ThemeEntry {
        canonical: "kanagawa",
        aliases: &["kanagawawave"],
        light: false,
        theme: KANAGAWA,
    },
    ThemeEntry {
        canonical: "one-dark",
        aliases: &["one"],
        light: false,
        theme: ONE_DARK,
    },
    ThemeEntry {
        canonical: "one-light",
        aliases: &[],
        light: true,
        theme: ONE_LIGHT,
    },
    ThemeEntry {
        canonical: "ayu-dark",
        aliases: &["ayu"],
        light: false,
        theme: AYU_DARK,
    },
    ThemeEntry {
        canonical: "ayu-light",
        aliases: &[],
        light: true,
        theme: AYU_LIGHT,
    },
    ThemeEntry {
        canonical: "gruvbox-light",
        aliases: &[],
        light: true,
        theme: GRUVBOX_LIGHT,
    },
];

/// Normalize a theme name for lookup: lowercase + strip everything but ASCII
/// alphanumerics, so `"Tokyo Night"`, `"tokyo-night"`, and `"tokyonight"` all
/// resolve to the same key. Shared by the built-in and user-theme lookup paths
/// so the two layers agree on what counts as "the same name".
pub(super) fn normalize(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn find_builtin(key: &str) -> Option<&'static ThemeEntry> {
    BUILTIN_THEMES
        .iter()
        .find(|e| normalize(e.canonical) == key || e.aliases.contains(&key))
}

/// Resolve a theme by (case-insensitive, separator-insensitive) name. User
/// themes are checked first (so one can shadow a built-in of the same name),
/// then the 18 built-ins. Returns `None` for an unknown name so the caller can
/// warn and keep the default.
pub fn theme_by_name(name: &str) -> Option<Theme> {
    let key = normalize(name);
    if let Some(entry) = super::user_themes::find(&key) {
        return Some(entry.theme);
    }
    find_builtin(&key).map(|e| e.theme)
}

/// Map any accepted theme name/alias to its canonical entry, defaulting to
/// `tokyo-night`. Lets the app store + cycle + save a stable name. Checks user
/// themes first, matching [`theme_by_name`]'s shadowing order.
pub fn canonical_name(input: &str) -> &'static str {
    let key = normalize(input);
    if let Some(entry) = super::user_themes::find(&key) {
        return entry.canonical;
    }
    find_builtin(&key)
        .map(|e| e.canonical)
        .unwrap_or("tokyo-night")
}

/// Whether a named theme is a LIGHT theme (light background, dark text). Used
/// to pick a sensible default when following the system color scheme. A name
/// that doesn't resolve to a user or built-in theme is treated as dark (every
/// built-in defaults to dark too via [`canonical_name`]'s `tokyo-night` fallback).
#[allow(dead_code)]
pub fn is_light(name: &str) -> bool {
    let key = normalize(name);
    if let Some(entry) = super::user_themes::find(&key) {
        return entry.light;
    }
    find_builtin(&key).map(|e| e.light).unwrap_or(false)
}

/// A snapshot of every available theme: the 18 built-ins plus any loaded user
/// themes, in display order — built-ins first (in [`BUILTIN_THEMES`] order),
/// then user themes (in load order; see [`super::user_themes`]). A user theme
/// whose canonical name matches a built-in's REPLACES it here (rather than
/// appearing twice), matching the shadowing rule [`theme_by_name`] applies.
///
/// Returns an owned `Vec` (not a `'static` slice) because the user-theme tail
/// can only be read out from behind a lock; the indices are stable for the
/// lifetime of the returned `Vec` (i.e. within one call / one frame), which is
/// all every call site needs.
pub fn theme_entries() -> Vec<ThemeEntry> {
    let user = super::user_themes::snapshot();
    let shadowed: std::collections::HashSet<String> =
        user.iter().map(|e| normalize(e.canonical)).collect();
    let mut out: Vec<ThemeEntry> = BUILTIN_THEMES
        .iter()
        .copied()
        .filter(|e| !shadowed.contains(&normalize(e.canonical)))
        .collect();
    out.extend(user);
    out
}

/// Canonical names in display order — see [`theme_entries`]. Replaces the old
/// `THEME_NAMES` const slice; call sites that used to index a `'static` const
/// slice now index this per-call `Vec` instead (indices stay stable within a
/// frame, which is all they need).
pub fn theme_names() -> Vec<&'static str> {
    theme_entries().into_iter().map(|e| e.canonical).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::vte::ansi::Rgb;

    /// Every canonical name and every alias resolves via `theme_by_name`, and
    /// `canonical_name`/`is_light` agree with the entry that owns it — catches
    /// a typo or a missing/duplicated arm the way the old hand-written `match`
    /// blocks could silently drift out of sync with each other.
    #[test]
    fn every_builtin_name_and_alias_resolves() {
        for entry in BUILTIN_THEMES {
            assert!(
                theme_by_name(entry.canonical).is_some(),
                "{} should resolve",
                entry.canonical
            );
            assert_eq!(canonical_name(entry.canonical), entry.canonical);
            assert_eq!(is_light(entry.canonical), entry.light);
            for alias in entry.aliases {
                assert!(
                    theme_by_name(alias).is_some(),
                    "alias {alias} should resolve"
                );
                assert_eq!(
                    canonical_name(alias),
                    entry.canonical,
                    "alias {alias} should canonicalize to {}",
                    entry.canonical
                );
                assert_eq!(
                    is_light(alias),
                    entry.light,
                    "alias {alias} light flag should match {}",
                    entry.canonical
                );
            }
        }
    }

    /// A few aliases named explicitly in the refactor spec, spelled out so a
    /// future edit that silently drops one fails loudly and specifically.
    #[test]
    fn known_aliases_match_spec_examples() {
        assert_eq!(canonical_name("mocha"), "catppuccin-mocha");
        assert_eq!(canonical_name("catppuccin"), "catppuccin-mocha");
        assert_eq!(canonical_name("tokyo"), "tokyo-night");
        assert_eq!(canonical_name("dawn"), "rose-pine-dawn");
        assert_eq!(canonical_name("latte"), "catppuccin-latte");
    }

    #[test]
    fn light_flags_match_known_light_themes() {
        let light_names = [
            "rose-pine-dawn",
            "dawn",
            "catppuccin-latte",
            "latte",
            "everforest-light",
            "one-light",
            "ayu-light",
            "gruvbox-light",
        ];
        for name in light_names {
            assert!(theme_by_name(name).is_some(), "{name} should resolve");
            assert!(is_light(name), "{name} should be light");
        }
        let lum = |c: Rgb| c.r as u32 + c.g as u32 + c.b as u32;
        for entry in BUILTIN_THEMES.iter().filter(|e| !e.light) {
            assert!(
                lum(entry.theme.bg) < lum(entry.theme.fg),
                "{} bg should be darker than fg",
                entry.canonical
            );
        }
        for entry in BUILTIN_THEMES.iter().filter(|e| e.light) {
            assert!(
                lum(entry.theme.bg) > lum(entry.theme.fg),
                "{} bg should be brighter than fg",
                entry.canonical
            );
        }
    }

    #[test]
    fn unknown_name_defaults_to_tokyo_night_and_dark() {
        assert_eq!(canonical_name("not-a-real-theme"), "tokyo-night");
        assert!(theme_by_name("not-a-real-theme").is_none());
        assert!(!is_light("not-a-real-theme"));
    }

    #[test]
    fn theme_names_matches_builtin_order_with_no_user_themes() {
        // No test in this crate populates the global user-themes list with a
        // real directory (see `user_themes::tests` for isolated coverage of
        // that path), so `theme_names()` here is exactly the built-in order.
        let names = theme_names();
        let expected: Vec<&str> = BUILTIN_THEMES.iter().map(|e| e.canonical).collect();
        assert_eq!(names, expected);
    }
}
