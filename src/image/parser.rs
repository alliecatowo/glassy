//! KittyParser implementation and image decode helpers.

use super::*;

impl KittyParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed the inside of one APC graphics sequence (the bytes between `ESC _ G`
    /// and `ESC \`, i.e. `<key=val,...>` optionally followed by `;<base64>`).
    /// Returns a completed command when the final chunk (`m=0`/absent) arrives.
    pub fn feed(&mut self, body: &[u8]) -> Option<GraphicsCommand> {
        let body = std::str::from_utf8(body).ok()?;
        let (control_str, payload_b64) = match body.split_once(';') {
            Some((c, p)) => (c, p),
            None => (body, ""),
        };
        let mut controls = Controls::parse(control_str);
        let chunk = b64_decode(payload_b64.trim());

        // An `i=0` (unset) id means "this is a standalone, anonymous image".
        // Without synthetic ids, every i=0 chunk would share the same pending
        // slot — causing unrelated images to merge their payloads. We assign a
        // fresh monotonic id on the *first* chunk of each i=0 stream (when
        // `more=true` or the command is self-contained).
        //
        // The anonymous id range lives just below SIXEL_ID_BASE (0x4000_0000+)
        // so it never collides with app-assigned kitty ids or sixel ids.
        if controls.id == 0 {
            // An `i=0` command carries no stable id. Without remapping, every
            // i=0 chunk shares the same HashMap slot and images merge across
            // unrelated transmissions.
            //
            // Strategy: if there is already an open anonymous stream (a pending
            // entry in the ANON_ID_BASE range — meaning a prior m=1 chunk was
            // received), continue it. Otherwise allocate a fresh synthetic id.
            // This correctly handles:
            //   - standalone i=0 command (m=0): no prior slot → fresh id.
            //   - multi-chunk i=0 stream: first m=1 → fresh id; subsequent
            //     m=1/m=0 chunks → reuse that slot (the only open anon slot).
            let open_anon = self
                .pending
                .keys()
                .copied()
                .filter(|&k| (ANON_ID_BASE..SIXEL_ID_BASE).contains(&k))
                .max(); // take highest (most recent) open anon slot
            controls.id = match open_anon {
                Some(existing) => existing, // continue in-progress anon stream
                None => {
                    // Start a new anon stream with a fresh id.
                    let offset = self.next_anon_id;
                    self.next_anon_id = self.next_anon_id.wrapping_add(1);
                    ANON_ID_BASE.wrapping_add(offset)
                }
            };
        }

        let id = controls.id;
        let more = controls.more;

        // If we are about to start a new stream (id not yet in pending) and the
        // pending map is already full, evict the oldest entry first so we never
        // exceed MAX_PENDING_STREAMS. Do this before touching `entry` so the
        // borrow checker doesn't see two mutable borrows of `self.pending`.
        if !self.pending.contains_key(&id) && self.pending.len() >= MAX_PENDING_STREAMS
            && let Some(&oldest) = self.pending.keys().min() {
                self.pending.remove(&oldest);
            }

        // Append to (or start) the pending buffer for this id.
        let entry = self.pending.entry(id).or_insert_with(|| Pending {
            controls: controls.clone(),
            payload: Vec::new(),
        });
        // Clamp per-stream payload to avoid OOM from an oversized i=1 flood.
        let remaining = MAX_PENDING_PAYLOAD_BYTES.saturating_sub(entry.payload.len());
        entry.payload.extend_from_slice(&chunk[..chunk.len().min(remaining)]);

        if more {
            return None; // wait for further chunks
        }

        let Pending { controls, payload } = self.pending.remove(&id)?;
        let image = decode_payload(&controls, &payload);
        Some(GraphicsCommand {
            action: controls.action,
            id,
            image,
            cols: controls.cols,
            rows: controls.rows,
        })
    }
}


/// Decode reassembled payload bytes into RGBA per the declared format.
pub(crate) fn decode_payload(controls: &Controls, payload: &[u8]) -> Option<DecodedImage> {
    if payload.is_empty() {
        return None;
    }
    let (w, h) = (controls.width, controls.height);
    // Reject absurd dimensions before any size math (avoids u32 overflow in the
    // expected-length check below and downstream allocation/upload blowups).
    if matches!(controls.format, Format::Rgba | Format::Rgb)
        && (w == 0 || h == 0 || w > MAX_IMAGE_DIM || h > MAX_IMAGE_DIM)
    {
        return None;
    }
    match controls.format {
        Format::Png => decode_png(payload),
        Format::Rgba => {
            // u64 math: w,h are each <= MAX_IMAGE_DIM so this cannot overflow.
            let expected = w as u64 * h as u64 * 4;
            (expected == payload.len() as u64).then(|| DecodedImage {
                width: w,
                height: h,
                rgba: payload.to_vec(),
            })
        }
        Format::Rgb => {
            if w as u64 * h as u64 * 3 != payload.len() as u64 {
                return None;
            }
            let mut rgba = Vec::with_capacity((w as usize) * (h as usize) * 4);
            for px in payload.chunks_exact(3) {
                rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
            Some(DecodedImage { width: w, height: h, rgba })
        }
    }
}

/// Decode a PNG into tightly-packed RGBA8 via the `png` crate.
pub(crate) fn decode_png(bytes: &[u8]) -> Option<DecodedImage> {
    let mut decoder = png::Decoder::new(bytes);
    // Normalize to 8-bit channels: expand palette/low-bit-depth and tRNS, and
    // strip 16-bit down to 8 so the color-type match below always sees 8bpp.
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    // Limit total bytes the decoder may allocate to prevent OOM from crafted PNGs.
    decoder.set_limits(png::Limits { bytes: MAX_IMAGE_BYTES });
    let mut reader = decoder.read_info().ok()?;
    // Validate header dimensions before allocating the output buffer.
    {
        let info = reader.info();
        if info.width > MAX_IMAGE_DIM || info.height > MAX_IMAGE_DIM {
            return None;
        }
    }
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width, info.height);
    // Belt-and-suspenders: verify decoded dims even though we checked the header.
    if w > MAX_IMAGE_DIM || h > MAX_IMAGE_DIM {
        return None;
    }
    let data = &buf[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => data.to_vec(),
        png::ColorType::Rgb => {
            let mut out = Vec::with_capacity((w * h * 4) as usize);
            for px in data.chunks_exact(3) {
                out.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
            out
        }
        png::ColorType::Grayscale => {
            let mut out = Vec::with_capacity((w * h * 4) as usize);
            for &g in data {
                out.extend_from_slice(&[g, g, g, 255]);
            }
            out
        }
        png::ColorType::GrayscaleAlpha => {
            let mut out = Vec::with_capacity((w * h * 4) as usize);
            for px in data.chunks_exact(2) {
                out.extend_from_slice(&[px[0], px[0], px[0], px[1]]);
            }
            out
        }
        png::ColorType::Indexed => return None, // expansion not requested; skip
    };
    Some(DecodedImage { width: w, height: h, rgba })
}

/// Minimal standard-base64 decoder (RFC 4648, no padding required). Kept
/// hand-rolled to avoid a dependency. Ignores ASCII whitespace; returns the
/// bytes decoded so far on the first invalid symbol.
pub(crate) fn b64_decode(s: &str) -> Vec<u8> {
    fn val(b: u8) -> Option<u8> {
        match b {
            b'A'..=b'Z' => Some(b - b'A'),
            b'a'..=b'z' => Some(b - b'a' + 26),
            b'0'..=b'9' => Some(b - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3 + 3);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for &b in s.as_bytes() {
        if b == b'=' {
            break;
        }
        if b.is_ascii_whitespace() {
            continue;
        }
        let Some(v) = val(b) else { break };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

/// Decode a sixel data stream (the bytes after `DCS <params> q`, up to `ST`)
/// into RGBA. Pixels never written stay transparent so they composite over the
/// terminal background. Returns `None` if nothing was drawn.
///
/// Sixel grammar handled: `#n;type;x;y;z` color definition (type 2 = RGB 0-100,
/// type 1 = HLS), `#n` color select, `!n` repeat-count, `$` carriage return,
/// `-` next 6-pixel band, `"..."` raster attributes (skipped), and the sixel
/// data bytes `?`..`~` (each six vertical pixels in the current color).
pub fn decode_sixel(data: &[u8]) -> Option<DecodedImage> {
    let mut palette = default_sixel_palette();
    let mut color = 0usize; // current color register
    let mut rows: Vec<Vec<[u8; 4]>> = Vec::new(); // row-major, grown on demand
    let mut x = 0usize; // current column
    let mut band = 0usize; // current 6-pixel band index (top row = band*6)
    let mut max_w = 0usize;

    // Set one pixel for the current color, ignoring writes beyond the canvas cap
    // so a malformed stream (huge !repeat / many bands) can't grow the buffer
    // without bound.
    fn put(rows: &mut Vec<Vec<[u8; 4]>>, x: usize, y: usize, c: [u8; 4]) {
        if x >= SIXEL_MAX_DIM || y >= SIXEL_MAX_DIM {
            return;
        }
        while rows.len() <= y {
            rows.push(Vec::new());
        }
        let row = &mut rows[y];
        while row.len() <= x {
            row.push([0, 0, 0, 0]);
        }
        row[x] = c;
    }

    let mut i = 0;
    while i < data.len() {
        let b = data[i];
        match b {
            b'#' => {
                // Color: #n  or  #n;type;a;b;c
                i += 1;
                let (n, used) = parse_uint(&data[i..]);
                i += used;
                let n = n as usize;
                if data.get(i) == Some(&b';') {
                    let mut nums = [0u32; 4]; // type, a, b, c
                    let mut k = 0;
                    while data.get(i) == Some(&b';') && k < 4 {
                        i += 1;
                        let (v, u) = parse_uint(&data[i..]);
                        nums[k] = v;
                        i += u;
                        k += 1;
                    }
                    if n < palette.len() {
                        palette[n] = sixel_color(nums[0], nums[1], nums[2], nums[3]);
                    }
                }
                color = n.min(palette.len() - 1);
            }
            b'!' => {
                // Repeat: !n <sixelchar>
                i += 1;
                let (count, used) = parse_uint(&data[i..]);
                i += used;
                if let Some(&sx) = data.get(i) {
                    if (b'?'..=b'~').contains(&sx) {
                        let bits = sx - b'?';
                        let c = palette[color];
                        // Clamp the run so x can't advance far past the canvas cap
                        // (bounds the loop against a malicious huge repeat count).
                        let reps = (count.max(1) as usize)
                            .min(SIXEL_MAX_DIM.saturating_sub(x) + 1);
                        for _ in 0..reps {
                            for bit in 0..6 {
                                if bits & (1 << bit) != 0 {
                                    put(&mut rows, x, band * 6 + bit, c);
                                }
                            }
                            x += 1;
                        }
                        max_w = max_w.max(x.min(SIXEL_MAX_DIM));
                    }
                    i += 1;
                }
            }
            b'$' => {
                x = 0;
                i += 1;
            }
            b'-' => {
                band += 1;
                x = 0;
                i += 1;
            }
            b'"' => {
                // Raster attributes: "Pan;Pad;Ph;Pv — consume digits/semicolons.
                i += 1;
                while i < data.len() && (data[i].is_ascii_digit() || data[i] == b';') {
                    i += 1;
                }
            }
            b'?'..=b'~' => {
                let bits = b - b'?';
                let c = palette[color];
                for bit in 0..6 {
                    if bits & (1 << bit) != 0 {
                        put(&mut rows, x, band * 6 + bit, c);
                    }
                }
                x += 1;
                max_w = max_w.max(x).min(SIXEL_MAX_DIM);
                i += 1;
            }
            _ => i += 1, // whitespace / unknown
        }
    }

    if rows.is_empty() || max_w == 0 {
        return None;
    }
    let width = max_w as u32;
    let height = rows.len() as u32;
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for row in &rows {
        for x in 0..max_w {
            let px = row.get(x).copied().unwrap_or([0, 0, 0, 0]);
            rgba.extend_from_slice(&px);
        }
    }
    Some(DecodedImage { width, height, rgba })
}

/// Parse a leading base-10 unsigned integer, returning `(value, bytes_consumed)`.
fn parse_uint(data: &[u8]) -> (u32, usize) {
    let mut v = 0u32;
    let mut n = 0;
    while n < data.len() && data[n].is_ascii_digit() {
        v = v.saturating_mul(10).saturating_add((data[n] - b'0') as u32);
        n += 1;
    }
    (v, n)
}

/// Convert a sixel color spec to RGBA. `kind` 2 = RGB (each 0-100), 1 = HLS
/// (H 0-360, L 0-100, S 0-100). Unknown kinds fall back to the RGB reading.
fn sixel_color(kind: u32, a: u32, b: u32, c: u32) -> [u8; 4] {
    let pct = |v: u32| ((v.min(100) as f32) * 255.0 / 100.0).round() as u8;
    if kind == 1 {
        let (r, g, bl) = hls_to_rgb(a as f32, b as f32 / 100.0, c as f32 / 100.0);
        [r, g, bl, 255]
    } else {
        [pct(a), pct(b), pct(c), 255]
    }
}

/// HLS (hue 0-360, lightness/saturation 0-1) to RGB bytes.
fn hls_to_rgb(h: f32, l: f32, s: f32) -> (u8, u8, u8) {
    if s <= 0.0 {
        let v = (l * 255.0).round() as u8;
        return (v, v, v);
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let hk = (h / 360.0).rem_euclid(1.0);
    let conv = |t: f32| {
        let t = t.rem_euclid(1.0);
        let v = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        (v * 255.0).round() as u8
    };
    (conv(hk + 1.0 / 3.0), conv(hk), conv(hk - 1.0 / 3.0))
}

/// The standard 16-color VT340 sixel palette (registers 0-15), RGB 0-100 scaled
/// to 0-255; registers 16-255 default to opaque black. Images that define their
/// own colors overwrite these.
fn default_sixel_palette() -> Vec<[u8; 4]> {
    const BASE: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (20, 20, 80),
        (80, 13, 13),
        (20, 80, 20),
        (80, 20, 80),
        (20, 80, 80),
        (80, 80, 20),
        (53, 53, 53),
        (26, 26, 26),
        (33, 33, 60),
        (60, 26, 26),
        (33, 60, 33),
        (60, 33, 60),
        (33, 60, 60),
        (60, 60, 33),
        (80, 80, 80),
    ];
    let s = |v: u8| ((v as f32) * 255.0 / 100.0).round() as u8;
    let mut p = vec![[0u8, 0, 0, 255]; 256];
    for (i, &(r, g, b)) in BASE.iter().enumerate() {
        p[i] = [s(r), s(g), s(b), 255];
    }
    p
}

