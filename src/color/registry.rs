//! The single source of truth mapping theme names/aliases to [`Theme`] values.
//!
//! [`BUILTIN_THEMES`] is the canonical list of the 60 themes glassy ships with
//! (the original 18 plus the w14 themes-pack wave's 42 -- 12 light + 30 dark,
//! every one sourced from a fetched, verified upstream palette -- see the
//! `// Source: ...` comment above each `const Theme` in `builtin.rs`): each
//! entry carries its canonical display name, the extra aliases it accepts,
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

/// The 60 built-in themes, in the order they've always shipped in (this order
/// is user-visible: it's the settings dropdown / theme-cycle / palette order).
/// Aliases and light flags exactly match the pre-registry hand-written `match`
/// arms in the old `theme_by_name` / `canonical_name` / `is_light` — see
/// `every_builtin_name_and_alias_resolves` below, which encodes them as a test
/// so a future edit can't silently drop one. The original 18 come first,
/// followed by the w14 themes-pack wave's light pack (12) then dark pack (30)
/// in the order added — see `builtin.rs` for the color data + source citations.
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
    // --- w14 themes-pack wave: 12 light + 30 dark, added below ---
    // --- light pack ---
    ThemeEntry {
        canonical: "github-light",
        aliases: &[],
        light: true,
        theme: GITHUB_LIGHT,
    },
    ThemeEntry {
        canonical: "solarized-light",
        aliases: &[],
        light: true,
        theme: SOLARIZED_LIGHT,
    },
    ThemeEntry {
        canonical: "one-half-light",
        aliases: &[],
        light: true,
        theme: ONE_HALF_LIGHT,
    },
    ThemeEntry {
        canonical: "tokyo-night-day",
        aliases: &["day"],
        light: true,
        theme: TOKYO_NIGHT_DAY,
    },
    ThemeEntry {
        canonical: "kanagawa-lotus",
        aliases: &["lotus"],
        light: true,
        theme: KANAGAWA_LOTUS,
    },
    ThemeEntry {
        canonical: "papercolor-light",
        aliases: &["papercolor"],
        light: true,
        theme: PAPERCOLOR_LIGHT,
    },
    ThemeEntry {
        canonical: "modus-operandi",
        aliases: &["modus"],
        light: true,
        theme: MODUS_OPERANDI,
    },
    ThemeEntry {
        canonical: "flexoki-light",
        aliases: &[],
        light: true,
        theme: FLEXOKI_LIGHT,
    },
    ThemeEntry {
        canonical: "vitesse-light",
        aliases: &[],
        light: true,
        theme: VITESSE_LIGHT,
    },
    ThemeEntry {
        canonical: "dayfox",
        aliases: &[],
        light: true,
        theme: DAYFOX,
    },
    ThemeEntry {
        canonical: "selenized-light",
        aliases: &["selenized"],
        light: true,
        theme: SELENIZED_LIGHT,
    },
    ThemeEntry {
        canonical: "alabaster",
        aliases: &[],
        light: true,
        theme: ALABASTER,
    },
    // --- dark pack ---
    ThemeEntry {
        canonical: "github-dark",
        aliases: &["github"],
        light: false,
        theme: GITHUB_DARK,
    },
    ThemeEntry {
        canonical: "monokai",
        aliases: &["monokaiclassic"],
        light: false,
        theme: MONOKAI,
    },
    ThemeEntry {
        canonical: "monokai-pro",
        aliases: &[],
        light: false,
        theme: MONOKAI_PRO,
    },
    ThemeEntry {
        canonical: "material",
        aliases: &[],
        light: false,
        theme: MATERIAL,
    },
    ThemeEntry {
        canonical: "material-darker",
        aliases: &[],
        light: false,
        theme: MATERIAL_DARKER,
    },
    ThemeEntry {
        canonical: "night-owl",
        aliases: &[],
        light: false,
        theme: NIGHT_OWL,
    },
    ThemeEntry {
        canonical: "snazzy",
        aliases: &[],
        light: false,
        theme: SNAZZY,
    },
    ThemeEntry {
        canonical: "horizon-dark",
        aliases: &["horizon"],
        light: false,
        theme: HORIZON_DARK,
    },
    ThemeEntry {
        canonical: "oceanic-next",
        aliases: &["oceanic"],
        light: false,
        theme: OCEANIC_NEXT,
    },
    ThemeEntry {
        canonical: "palenight",
        aliases: &[],
        light: false,
        theme: PALENIGHT,
    },
    ThemeEntry {
        canonical: "zenburn",
        aliases: &[],
        light: false,
        theme: ZENBURN,
    },
    ThemeEntry {
        canonical: "iceberg-dark",
        aliases: &["iceberg"],
        light: false,
        theme: ICEBERG_DARK,
    },
    ThemeEntry {
        canonical: "nightfox",
        aliases: &[],
        light: false,
        theme: NIGHTFOX,
    },
    ThemeEntry {
        canonical: "vitesse-dark",
        aliases: &["vitesse"],
        light: false,
        theme: VITESSE_DARK,
    },
    ThemeEntry {
        canonical: "flexoki-dark",
        aliases: &["flexoki"],
        light: false,
        theme: FLEXOKI_DARK,
    },
    ThemeEntry {
        canonical: "everblush",
        aliases: &[],
        light: false,
        theme: EVERBLUSH,
    },
    ThemeEntry {
        canonical: "melange-dark",
        aliases: &["melange"],
        light: false,
        theme: MELANGE_DARK,
    },
    ThemeEntry {
        canonical: "synthwave-84",
        aliases: &["synthwave"],
        light: false,
        theme: SYNTHWAVE_84,
    },
    ThemeEntry {
        canonical: "catppuccin-frappe",
        aliases: &["frappe"],
        light: false,
        theme: CATPPUCCIN_FRAPPE,
    },
    ThemeEntry {
        canonical: "tokyo-night-storm",
        aliases: &["storm"],
        light: false,
        theme: TOKYO_NIGHT_STORM,
    },
    ThemeEntry {
        canonical: "gruvbox-material",
        aliases: &[],
        light: false,
        theme: GRUVBOX_MATERIAL,
    },
    ThemeEntry {
        canonical: "one-half-dark",
        aliases: &["onehalf"],
        light: false,
        theme: ONE_HALF_DARK,
    },
    ThemeEntry {
        canonical: "ayu-mirage",
        aliases: &["mirage"],
        light: false,
        theme: AYU_MIRAGE,
    },
    ThemeEntry {
        canonical: "rose-pine-moon",
        aliases: &["moon"],
        light: false,
        theme: ROSE_PINE_MOON,
    },
    ThemeEntry {
        canonical: "kanagawa-dragon",
        aliases: &["dragon"],
        light: false,
        theme: KANAGAWA_DRAGON,
    },
    ThemeEntry {
        canonical: "solarized-osaka",
        aliases: &["osaka"],
        light: false,
        theme: SOLARIZED_OSAKA,
    },
    ThemeEntry {
        canonical: "poimandres",
        aliases: &["poi"],
        light: false,
        theme: POIMANDRES,
    },
    ThemeEntry {
        canonical: "andromeda",
        aliases: &[],
        light: false,
        theme: ANDROMEDA,
    },
    ThemeEntry {
        canonical: "aura",
        aliases: &[],
        light: false,
        theme: AURA,
    },
    ThemeEntry {
        canonical: "challenger-deep",
        aliases: &["deep"],
        light: false,
        theme: CHALLENGER_DEEP,
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

    /// No canonical name or alias appears twice across `BUILTIN_THEMES` — a
    /// mechanical guard for the themes-pack wave (60 entries, added by hand)
    /// against a copy-paste duplicate silently shadowing an earlier theme
    /// (`find_builtin` returns the FIRST match, so a duplicate would make one
    /// entry permanently unreachable by name without any other test failing).
    #[test]
    fn no_duplicate_canonical_names_or_aliases() {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for entry in BUILTIN_THEMES {
            let key = normalize(entry.canonical);
            assert!(
                seen.insert(key),
                "duplicate canonical name (or alias colliding with it): {}",
                entry.canonical
            );
            for alias in entry.aliases {
                let key = normalize(alias);
                assert!(
                    seen.insert(key),
                    "duplicate alias (or alias colliding with a canonical name): {alias}",
                );
            }
        }
    }

    /// Every theme flagged `light: true` actually has a light background —
    /// computed the same way the renderer would (via [`super::luma`] on the
    /// theme's `bg`), not the coarse RGB-sum check `light_flags_match_known_light_themes`
    /// already does. Catches a transcription error (e.g. a copy-pasted `light: true`
    /// on a theme whose fetched palette turned out to be dark, or vice versa)
    /// that a sum-based check could theoretically miss.
    #[test]
    fn light_flagged_themes_have_a_light_background() {
        for entry in BUILTIN_THEMES.iter().filter(|e| e.light) {
            let l = crate::color::luma(crate::color::to_f32(entry.theme.bg));
            assert!(
                l > 0.6,
                "{} is flagged light but its background luma is {l:.3} (expected > 0.6)",
                entry.canonical
            );
        }
        // And the converse, for symmetry: no dark-flagged theme should have a
        // luma high enough to read as light.
        for entry in BUILTIN_THEMES.iter().filter(|e| !e.light) {
            let l = crate::color::luma(crate::color::to_f32(entry.theme.bg));
            assert!(
                l <= 0.6,
                "{} is flagged dark but its background luma is {l:.3} (expected <= 0.6)",
                entry.canonical
            );
        }
    }
}
