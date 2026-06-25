//! Live theme-from-image generator.
//!
//! Extracts a dominant colour palette from any image file (PNG, raw RGB/RGBA
//! byte slices or a flat `&[(r,g,b)]` sample list) using a two-pass approach:
//!
//! 1. **Median-cut** colour quantisation (8 buckets → 8 representative colours).
//! 2. **ANSI role assignment** — map colours to all 16 ANSI slots by hue/luminance.
//! 3. **Contrast-safe adjustment** — nudge fg until WCAG contrast ≥ 4.5 : 1.
//!
//! Entry points: [`from_image_path`], [`from_rgba`], [`from_rgb_samples`].
//! No new image deps — reuses the `png` crate already in `Cargo.toml`.

use alacritty_terminal::vte::ansi::Rgb;
use anyhow::{Context, Result, bail};

use crate::color::Theme;

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Generate a [`Theme`] by reading and decoding a PNG image at `path`.
///
/// Returns an error if the file cannot be read or decoded, or if the image
/// is entirely transparent / zero-area.
pub fn from_image_path(path: &str) -> Result<Theme> {
    let bytes =
        std::fs::read(path).with_context(|| format!("could not read image file '{path}'"))?;
    from_image_bytes(&bytes, path)
}

/// Generate a [`Theme`] from raw PNG/image bytes (already in memory).
pub fn from_image_bytes(bytes: &[u8], label: &str) -> Result<Theme> {
    let samples = decode_image_to_samples(bytes)
        .with_context(|| format!("could not decode image '{label}'"))?;
    Ok(from_rgb_samples(&samples))
}

/// Generate a [`Theme`] from a flat RGBA8 pixel buffer (`width × height × 4`
/// bytes, row-major). Transparent pixels (alpha < 128) are skipped.
#[allow(dead_code)]
pub fn from_rgba(width: u32, height: u32, rgba: &[u8]) -> Theme {
    let expected = (width as usize) * (height as usize) * 4;
    debug_assert_eq!(rgba.len(), expected, "RGBA buffer size mismatch");
    let samples: Vec<(u8, u8, u8)> = rgba
        .chunks_exact(4)
        .filter(|px| px[3] >= 128)
        .map(|px| (px[0], px[1], px[2]))
        .collect();
    from_rgb_samples(&samples)
}

/// Generate a [`Theme`] from a list of RGB samples (the unit-test / scripting
/// entry point that requires no file I/O).
///
/// Falls back to the built-in Tokyo Night theme if `samples` is empty or too
/// homogeneous to produce a useful palette.
pub fn from_rgb_samples(samples: &[(u8, u8, u8)]) -> Theme {
    if samples.is_empty() {
        return crate::color::TOKYO_NIGHT;
    }

    // Cap to 8 192 samples for speed; stride through the pixel list.
    const MAX_SAMPLES: usize = 8_192;
    let stride = (samples.len() / MAX_SAMPLES).max(1);
    let reduced: Vec<(u8, u8, u8)> = samples.iter().cloned().step_by(stride).collect();

    // 1. Median-cut → 8 representative colours.
    let palette = median_cut(&reduced, 8);

    // 2. Build the 16-colour ANSI array + fg/bg from the palette.
    build_theme_from_palette(&palette)
}

// ---------------------------------------------------------------------------
// PNG decoding (reuses the `png` crate already in Cargo.toml)
// ---------------------------------------------------------------------------

fn decode_image_to_samples(bytes: &[u8]) -> Result<Vec<(u8, u8, u8)>> {
    let mut decoder = png::Decoder::new(bytes);
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    // Keep the same 64 MB cap used by the kitty image renderer.
    decoder.set_limits(png::Limits {
        bytes: 64 * 1024 * 1024,
    });
    let mut reader = decoder.read_info().context("PNG header decode failed")?;
    let info = reader.info();
    let (w, h) = (info.width, info.height);
    if w == 0 || h == 0 {
        bail!("image has zero-area dimensions ({w}×{h})");
    }
    if w > 8192 || h > 8192 {
        bail!("image too large ({w}×{h}); max 8192×8192 for theme generation");
    }
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let frame = reader
        .next_frame(&mut buf)
        .context("PNG frame decode failed")?;
    let data = &buf[..frame.buffer_size()];
    let samples: Vec<(u8, u8, u8)> = match frame.color_type {
        png::ColorType::Rgba => data
            .chunks_exact(4)
            .filter(|px| px[3] >= 128)
            .map(|px| (px[0], px[1], px[2]))
            .collect(),
        png::ColorType::Rgb => data
            .chunks_exact(3)
            .map(|px| (px[0], px[1], px[2]))
            .collect(),
        png::ColorType::Grayscale => data.iter().map(|&g| (g, g, g)).collect(),
        png::ColorType::GrayscaleAlpha => data
            .chunks_exact(2)
            .filter(|px| px[1] >= 128)
            .map(|px| (px[0], px[0], px[0]))
            .collect(),
        png::ColorType::Indexed => bail!("indexed-colour PNG not supported for theme generation"),
    };
    if samples.is_empty() {
        bail!("image has no opaque pixels");
    }
    Ok(samples)
}

// ---------------------------------------------------------------------------
// Median-cut colour quantisation
// ---------------------------------------------------------------------------

/// A bucket of RGB samples; the representative colour is the mean.
struct Bucket {
    samples: Vec<(u8, u8, u8)>,
}

impl Bucket {
    fn new(samples: Vec<(u8, u8, u8)>) -> Self {
        Self { samples }
    }

    /// The axis (0=R, 1=G, 2=B) with the largest channel range in this bucket.
    fn widest_axis(&self) -> usize {
        let (mut rmin, mut rmax) = (255u8, 0u8);
        let (mut gmin, mut gmax) = (255u8, 0u8);
        let (mut bmin, mut bmax) = (255u8, 0u8);
        for &(r, g, b) in &self.samples {
            rmin = rmin.min(r);
            rmax = rmax.max(r);
            gmin = gmin.min(g);
            gmax = gmax.max(g);
            bmin = bmin.min(b);
            bmax = bmax.max(b);
        }
        let spans = [
            rmax.saturating_sub(rmin),
            gmax.saturating_sub(gmin),
            bmax.saturating_sub(bmin),
        ];
        if spans[1] >= spans[0] && spans[1] >= spans[2] {
            1
        } else if spans[2] >= spans[0] {
            2
        } else {
            0
        }
    }

    /// Mean colour of this bucket (the representative palette entry).
    fn mean(&self) -> (u8, u8, u8) {
        if self.samples.is_empty() {
            return (0, 0, 0);
        }
        let n = self.samples.len() as u64;
        let (sr, sg, sb) = self
            .samples
            .iter()
            .fold((0u64, 0u64, 0u64), |acc, &(r, g, b)| {
                (acc.0 + r as u64, acc.1 + g as u64, acc.2 + b as u64)
            });
        ((sr / n) as u8, (sg / n) as u8, (sb / n) as u8)
    }

    /// Split along the widest axis at the median, returning (low, high) halves.
    fn split(mut self) -> (Bucket, Bucket) {
        let axis = self.widest_axis();
        self.samples.sort_unstable_by_key(|&(r, g, b)| match axis {
            1 => g,
            2 => b,
            _ => r,
        });
        let mid = self.samples.len() / 2;
        let high = self.samples.split_off(mid);
        (Bucket::new(self.samples), Bucket::new(high))
    }
}

/// Run median-cut on `samples`, returning up to `n` representative colours.
/// `n` must be a power of two; if not it is rounded up to the next power of two.
fn median_cut(samples: &[(u8, u8, u8)], n: usize) -> Vec<(u8, u8, u8)> {
    if samples.is_empty() {
        return vec![];
    }
    // Split the largest bucket (by sample count) until we have `n` buckets.
    // This produces exactly `n` buckets regardless of whether `n` is a power of two.
    let target = n.max(1);
    let mut buckets = vec![Bucket::new(samples.to_vec())];
    while buckets.len() < target {
        // Find the largest bucket (by sample count) to split.
        let idx = buckets
            .iter()
            .enumerate()
            .max_by_key(|(_, b)| b.samples.len())
            .map(|(i, _)| i)
            .unwrap_or(0);
        // If the largest bucket has ≤ 1 sample it can't be split further.
        if buckets[idx].samples.len() <= 1 {
            break;
        }
        let bucket = buckets.remove(idx);
        let (lo, hi) = bucket.split();
        buckets.push(lo);
        buckets.push(hi);
    }
    buckets.iter().map(|b| b.mean()).collect()
}

// ---------------------------------------------------------------------------
// ANSI role assignment + contrast-safe Theme construction
// ---------------------------------------------------------------------------

/// Relative luminance (WCAG 2.1 §1.4.3) for an 8-bit sRGB colour.
fn rel_lum(r: u8, g: u8, b: u8) -> f64 {
    fn lin(c: u8) -> f64 {
        let s = c as f64 / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

/// WCAG contrast ratio between two relative luminances.
fn contrast_ratio(l1: f64, l2: f64) -> f64 {
    let (lighter, darker) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
    (lighter + 0.05) / (darker + 0.05)
}

/// Hue angle (0.0–360.0) of an RGB triple, or `None` for achromatic colours.
fn hue(r: u8, g: u8, b: u8) -> Option<f64> {
    let rf = r as f64 / 255.0;
    let gf = g as f64 / 255.0;
    let bf = b as f64 / 255.0;
    let max = rf.max(gf).max(bf);
    let min = rf.min(gf).min(bf);
    let delta = max - min;
    if delta < 1e-3 {
        return None; // achromatic
    }
    let h = if max == rf {
        60.0 * (((gf - bf) / delta) % 6.0)
    } else if max == gf {
        60.0 * (((bf - rf) / delta) + 2.0)
    } else {
        60.0 * (((rf - gf) / delta) + 4.0)
    };
    Some(((h % 360.0) + 360.0) % 360.0)
}

/// Nudge a colour toward white or black until the contrast ratio against `bg`
/// reaches `min_ratio`.  Returns the adjusted colour.
fn ensure_contrast(fg: (u8, u8, u8), bg: (u8, u8, u8), min_ratio: f64) -> (u8, u8, u8) {
    let bg_lum = rel_lum(bg.0, bg.1, bg.2);
    let mut r = fg.0 as i32;
    let mut g = fg.1 as i32;
    let mut b = fg.2 as i32;
    // Decide direction: if background is dark, push fg lighter; else darker.
    let dir: i32 = if bg_lum < 0.18 { 1 } else { -1 };
    for _ in 0..255 {
        let cr = contrast_ratio(rel_lum(r as u8, g as u8, b as u8), bg_lum);
        if cr >= min_ratio {
            break;
        }
        r = (r + dir * 3).clamp(0, 255);
        g = (g + dir * 3).clamp(0, 255);
        b = (b + dir * 3).clamp(0, 255);
    }
    (r as u8, g as u8, b as u8)
}

/// Convert a `(u8,u8,u8)` triple to `Rgb`.
fn to_rgb(t: (u8, u8, u8)) -> Rgb {
    Rgb {
        r: t.0,
        g: t.1,
        b: t.2,
    }
}

/// Map the dominant hue angle of a palette colour to the closest ANSI hue bucket.
/// Returns an index 0..6 corresponding to: 0=Red 1=Yellow 2=Green 3=Cyan 4=Blue 5=Magenta.
fn hue_bucket(h: f64) -> usize {
    // Hue ranges (centre of each 60° sector):
    //   Red:     330–360 + 0–30   → 0
    //   Yellow:  30–90            → 1
    //   Green:   90–150           → 2
    //   Cyan:    150–210          → 3
    //   Blue:    210–270          → 4
    //   Magenta: 270–330          → 5
    match h as u32 {
        330..=360 | 0..=29 => 0,
        30..=89 => 1,
        90..=149 => 2,
        150..=209 => 3,
        210..=269 => 4,
        270..=329 => 5,
        _ => 0,
    }
}

/// Build a full 16-entry ANSI `Theme` from 8 palette colours.
///
/// Assignment strategy (all indices are the standard ANSI terminal palette):
///  - Darkest colour → index 0 (black)
///  - Lightest colour → index 7 (white)
///  - Remaining 6 colours sorted by hue → indices 1–6 (red,green,yellow,blue,magenta,cyan)
///    with a ×1.25 brightened copy in indices 9–14 (bright versions).
///  - Index 8 (bright black) = dark colour lightened slightly.
///  - Index 15 (bright white) = light colour lightened slightly.
///  - bg = darkest (or slightly darker), fg = lightest (contrast-safe).
///  - cursor = most saturated mid-range colour.
fn build_theme_from_palette(palette: &[(u8, u8, u8)]) -> Theme {
    if palette.is_empty() {
        return crate::color::TOKYO_NIGHT;
    }

    // Sort palette by relative luminance.
    let mut sorted = palette.to_vec();
    sorted.sort_unstable_by(|&(ar, ag, ab), &(br, bg, bb)| {
        rel_lum(ar, ag, ab)
            .partial_cmp(&rel_lum(br, bg, bb))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let darkest = sorted[0];
    let lightest = *sorted.last().unwrap();

    // Mid-range colours (excluding the darkest and lightest).
    let mids: Vec<(u8, u8, u8)> = sorted[1..sorted.len().saturating_sub(1)].to_vec();

    // Assign mid colours to hue buckets (Red,Yellow,Green,Cyan,Blue,Magenta).
    // ANSI hue order: 1=Red, 2=Green, 3=Yellow, 4=Blue, 5=Magenta, 6=Cyan.
    // We use our internal bucket index 0=R,1=Y,2=G,3=C,4=B,5=M and then remap.
    let mut hue_slots: [Option<(u8, u8, u8)>; 6] = [None; 6];
    for &c in &mids {
        if let Some(h) = hue(c.0, c.1, c.2) {
            let bucket = hue_bucket(h);
            // First come, first served per slot (palette is sorted light→dark;
            // picking the first gives the most luminance-balanced set).
            if hue_slots[bucket].is_none() {
                hue_slots[bucket] = Some(c);
            }
        }
    }

    // Fill any empty hue slot with a tinted version of the darkest or lightest
    // colour so the theme is always complete.
    let fallback_for_slot = |slot: usize| -> (u8, u8, u8) {
        // Tint toward the hue angle centre by shifting channels.
        // Approximate: rotate the darkest colour toward the hue's r/g/b bias.
        let base = darkest;
        let bump: u8 = 80;
        match slot {
            0 => (base.0.saturating_add(bump), base.1, base.2), // red
            1 => (
                base.0.saturating_add(bump / 2),
                base.1.saturating_add(bump),
                base.2,
            ), // yellow
            2 => (base.0, base.1.saturating_add(bump), base.2), // green
            3 => (
                base.0,
                base.1.saturating_add(bump / 2),
                base.2.saturating_add(bump),
            ), // cyan
            4 => (base.0, base.1, base.2.saturating_add(bump)), // blue
            5 => (
                base.0.saturating_add(bump / 2),
                base.1,
                base.2.saturating_add(bump),
            ), // magenta
            _ => base,
        }
    };

    for (i, slot) in hue_slots.iter_mut().enumerate() {
        if slot.is_none() {
            *slot = Some(fallback_for_slot(i));
        }
    }

    // hue_slots: [0=R, 1=Y, 2=G, 3=C, 4=B, 5=M]
    let col_r = hue_slots[0].unwrap();
    let col_g = hue_slots[2].unwrap();
    let col_y = hue_slots[1].unwrap();
    let col_b = hue_slots[4].unwrap();
    let col_m = hue_slots[5].unwrap();
    let col_c = hue_slots[3].unwrap();

    // Brighten a colour by mixing toward white.
    let brighten = |c: (u8, u8, u8), f: f32| -> (u8, u8, u8) {
        let blend = |ch: u8| (ch as f32 * (1.0 - f) + 255.0 * f) as u8;
        (blend(c.0), blend(c.1), blend(c.2))
    };

    // Darken: mix toward black.
    let darken = |c: (u8, u8, u8), f: f32| -> (u8, u8, u8) {
        let scale = |ch: u8| (ch as f32 * (1.0 - f)) as u8;
        (scale(c.0), scale(c.1), scale(c.2))
    };

    // bg = darkest palette colour.
    let bg_raw = darkest;
    // Ensure bg is reasonably dark; if the palette is light (high-lum darkest), make it darker.
    let bg = if rel_lum(bg_raw.0, bg_raw.1, bg_raw.2) > 0.25 {
        darken(bg_raw, 0.5)
    } else {
        bg_raw
    };

    // fg = contrast-safe light foreground.
    let fg_raw = lightest;
    let fg = ensure_contrast(fg_raw, bg, 4.5);

    // Cursor = most saturated mid-range colour (falls back to the fg).
    let cursor_raw = mids
        .iter()
        .cloned()
        .max_by(|&(ar, ag, ab), &(br, bg, bb)| {
            // Saturation = (max-min)/max.
            let sat = |r: u8, g: u8, b: u8| {
                let max = r.max(g).max(b);
                if max == 0 {
                    0.0f64
                } else {
                    (max - r.min(g).min(b)) as f64 / max as f64
                }
            };
            sat(ar, ag, ab)
                .partial_cmp(&sat(br, bg, bb))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap_or(fg);
    let cursor = ensure_contrast(cursor_raw, bg, 3.0);

    // Selection bg: a dim, semi-transparent mix of the dominant mid and the bg.
    // For the stored Rgb we use a 30% blend of the cursor colour into the bg.
    let sel_r = (bg.0 as u16 * 7 / 10 + cursor.0 as u16 * 3 / 10) as u8;
    let sel_g = (bg.1 as u16 * 7 / 10 + cursor.1 as u16 * 3 / 10) as u8;
    let sel_b = (bg.2 as u16 * 7 / 10 + cursor.2 as u16 * 3 / 10) as u8;
    let selection_bg = (sel_r, sel_g, sel_b);

    // Bright-black (index 8): a slightly lighter version of the bg.
    let bright_black = brighten(bg, 0.25);
    // Black (index 0): even darker than bg.
    let black = darken(bg, 0.15);

    let ansi16 = [
        to_rgb(black),                 // 0  black
        to_rgb(col_r),                 // 1  red
        to_rgb(col_g),                 // 2  green
        to_rgb(col_y),                 // 3  yellow
        to_rgb(col_b),                 // 4  blue
        to_rgb(col_m),                 // 5  magenta
        to_rgb(col_c),                 // 6  cyan
        to_rgb(fg),                    // 7  white
        to_rgb(bright_black),          // 8  bright black
        to_rgb(brighten(col_r, 0.25)), // 9  bright red
        to_rgb(brighten(col_g, 0.25)), // 10 bright green
        to_rgb(brighten(col_y, 0.25)), // 11 bright yellow
        to_rgb(brighten(col_b, 0.25)), // 12 bright blue
        to_rgb(brighten(col_m, 0.25)), // 13 bright magenta
        to_rgb(brighten(col_c, 0.25)), // 14 bright cyan
        to_rgb(brighten(fg, 0.12)),    // 15 bright white
    ];

    Theme {
        fg: to_rgb(fg),
        bg: to_rgb(bg),
        cursor: to_rgb(cursor),
        selection_bg: to_rgb(selection_bg),
        ansi16,
    }
}

/// Serialise a generated [`Theme`] as a set of `color.*` config-file lines so
/// it can be written to `glassy.conf` with [`crate::config::parse::save`].
#[allow(dead_code)]
pub fn theme_to_config_pairs(theme: &Theme) -> Vec<(String, String)> {
    let hex = |c: Rgb| format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b);
    let mut pairs = vec![
        ("color.fg".to_string(), hex(theme.fg)),
        ("color.bg".to_string(), hex(theme.bg)),
        ("color.cursor".to_string(), hex(theme.cursor)),
        ("color.selection_bg".to_string(), hex(theme.selection_bg)),
    ];
    for (i, &c) in theme.ansi16.iter().enumerate() {
        pairs.push((format!("color.ansi{i}"), hex(c)));
    }
    pairs
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Dark-only palette should yield fg/bg with at least 4.5 : 1 contrast.
    #[test]
    fn contrast_safe_dark_bg() {
        // Simulate a very dark wallpaper.
        let dark_samples: Vec<(u8, u8, u8)> = (0..200)
            .flat_map(|i| {
                vec![
                    (i / 10, i / 12, i / 8),
                    (255 - i, 255 - i, 255 - i), // include some brights
                ]
            })
            .collect();
        let theme = from_rgb_samples(&dark_samples);
        let bg_lum = rel_lum(theme.bg.r, theme.bg.g, theme.bg.b);
        let fg_lum = rel_lum(theme.fg.r, theme.fg.g, theme.fg.b);
        let cr = contrast_ratio(fg_lum, bg_lum);
        assert!(
            cr >= 4.5,
            "fg/bg contrast {cr:.2} must be >= 4.5 for dark palette"
        );
    }

    /// Empty sample list should fall back to the default theme without panicking.
    #[test]
    fn empty_samples_returns_default() {
        let theme = from_rgb_samples(&[]);
        // Default theme has a specific bg.
        assert_eq!(
            (theme.bg.r, theme.bg.g, theme.bg.b),
            (0x1A, 0x1B, 0x26),
            "empty samples should fall back to Tokyo Night bg"
        );
    }

    #[test]
    fn rel_lum_black_is_zero() {
        assert!((rel_lum(0, 0, 0) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn rel_lum_white_is_one() {
        assert!((rel_lum(255, 255, 255) - 1.0).abs() < 1e-3);
    }

    #[test]
    fn contrast_ratio_black_white() {
        let cr = contrast_ratio(rel_lum(255, 255, 255), rel_lum(0, 0, 0));
        assert!(
            (cr - 21.0).abs() < 0.1,
            "black/white contrast ratio should be ~21"
        );
    }

    #[test]
    fn hue_red_is_zero() {
        assert_eq!(hue_bucket(hue(255, 0, 0).unwrap()), 0);
    }

    #[test]
    fn hue_green_is_two() {
        assert_eq!(hue_bucket(hue(0, 255, 0).unwrap()), 2);
    }

    #[test]
    fn hue_blue_is_four() {
        assert_eq!(hue_bucket(hue(0, 0, 255).unwrap()), 4);
    }

    #[test]
    fn hue_yellow_is_one() {
        assert_eq!(hue_bucket(hue(255, 255, 0).unwrap()), 1);
    }

    #[test]
    fn hue_cyan_is_three() {
        assert_eq!(hue_bucket(hue(0, 255, 255).unwrap()), 3);
    }

    #[test]
    fn hue_magenta_is_five() {
        assert_eq!(hue_bucket(hue(255, 0, 255).unwrap()), 5);
    }

    #[test]
    fn hue_achromatic_is_none() {
        assert!(hue(128, 128, 128).is_none());
        assert!(hue(0, 0, 0).is_none());
        assert!(hue(255, 255, 255).is_none());
    }

    #[test]
    fn median_cut_returns_n_colours() {
        let samples: Vec<(u8, u8, u8)> = (0..256).map(|i| (i as u8, 0, 0)).collect();
        let palette = median_cut(&samples, 8);
        assert_eq!(
            palette.len(),
            8,
            "median_cut should return exactly 8 colours"
        );
    }

    #[test]
    fn median_cut_single_colour() {
        let samples = vec![(100, 150, 200); 100];
        let palette = median_cut(&samples, 8);
        // Every bucket is the same colour; all means should be (100, 150, 200).
        for &(r, g, b) in &palette {
            assert_eq!((r, g, b), (100, 150, 200));
        }
    }

    #[test]
    fn from_rgb_samples_produces_valid_theme() {
        // A simple set of colours spanning multiple hues.
        let samples = vec![
            (10, 10, 20),    // near black
            (240, 240, 245), // near white
            (200, 50, 50),   // red-ish
            (50, 180, 50),   // green-ish
            (50, 50, 200),   // blue-ish
            (200, 200, 50),  // yellow-ish
            (50, 200, 200),  // cyan-ish
            (200, 50, 200),  // magenta-ish
        ];
        let theme = from_rgb_samples(&samples);
        // ansi16 must have exactly 16 entries.
        assert_eq!(theme.ansi16.len(), 16);
        // fg/bg contrast must meet WCAG AA.
        let cr = contrast_ratio(
            rel_lum(theme.fg.r, theme.fg.g, theme.fg.b),
            rel_lum(theme.bg.r, theme.bg.g, theme.bg.b),
        );
        assert!(cr >= 4.5, "fg/bg contrast must be >= 4.5; got {cr:.2}");
    }

    #[test]
    fn ensure_contrast_always_converges() {
        // Very similar fg and bg should be pushed apart.
        let bg = (30u8, 30, 40);
        let fg = (40u8, 40, 50); // barely different
        let adjusted = ensure_contrast(fg, bg, 4.5);
        let cr = contrast_ratio(
            rel_lum(adjusted.0, adjusted.1, adjusted.2),
            rel_lum(bg.0, bg.1, bg.2),
        );
        assert!(
            cr >= 4.5,
            "contrast must reach 4.5 after adjustment; got {cr:.2}"
        );
    }

    #[test]
    fn theme_to_config_pairs_has_20_entries() {
        use crate::color::TOKYO_NIGHT;
        let pairs = theme_to_config_pairs(&TOKYO_NIGHT);
        // fg, bg, cursor, selection_bg + 16 ansi = 20.
        assert_eq!(pairs.len(), 20);
        assert_eq!(pairs[0].0, "color.fg");
        assert_eq!(pairs[16].0, "color.ansi12");
    }

    #[test]
    fn theme_config_pairs_round_trip_hex() {
        use crate::color::TOKYO_NIGHT;
        let pairs = theme_to_config_pairs(&TOKYO_NIGHT);
        // Every value should be a valid 7-char hex string starting with '#'.
        for (key, val) in &pairs {
            assert_eq!(
                val.len(),
                7,
                "{key} value '{val}' must be 7 chars (#RRGGBB)"
            );
            assert!(val.starts_with('#'), "{key} value must start with '#'");
            assert!(
                u32::from_str_radix(&val[1..], 16).is_ok(),
                "{key} value '{val}' must be valid hex"
            );
        }
    }
}
