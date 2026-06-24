//! Inline-image protocol support (kitty graphics).
//!
//! Terminals carry images in escape sequences the VT parser otherwise discards:
//! the kitty graphics protocol uses APC (`ESC _ G <key=val,...>;<base64 payload>
//! ESC \`). This module is the pure, decode-only core — it parses a kitty
//! graphics command into a decoded RGBA image plus its placement intent. Wiring
//! it into the PTY byte stream (a tapping reader) and rendering the decoded
//! images as GPU quads in the grid is layered on top of this; keeping the parse
//! + decode here makes it independently testable with no terminal/GPU state.
//!
//! Reference: <https://sw.kovidgoyal.net/kitty/graphics-protocol/>

#![allow(dead_code)] // wired into the renderer in a follow-up step.

use std::collections::HashMap;

use alacritty_terminal::sync::FairMutex;

/// A decoded image ready to upload to the GPU: tightly-packed RGBA8, row-major.
#[derive(Clone)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    /// RGBA8, length == width * height * 4.
    pub rgba: Vec<u8>,
}

/// What a kitty graphics command asks the terminal to do (the `a=` key).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    /// `a=t`: transmit image data only (store, don't display yet).
    Transmit,
    /// `a=T`: transmit and display at the cursor.
    TransmitAndDisplay,
    /// `a=p`: display a previously-transmitted image.
    Display,
    /// `a=d`: delete image(s).
    Delete,
    /// Anything we don't model yet.
    Other(char),
}

impl Action {
    fn from(c: char) -> Self {
        match c {
            't' => Action::Transmit,
            'T' => Action::TransmitAndDisplay,
            'p' => Action::Display,
            'd' => Action::Delete,
            other => Action::Other(other),
        }
    }
}

/// Pixel format of transmitted data (`f=` key).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Format {
    /// f=32: raw RGBA.
    Rgba,
    /// f=24: raw RGB.
    Rgb,
    /// f=100: PNG.
    Png,
}

/// A fully-received kitty graphics command (all chunks reassembled).
pub struct GraphicsCommand {
    pub action: Action,
    /// Image id (`i=`), 0 if unset.
    pub id: u32,
    /// Decoded image, if the command carried displayable pixels.
    pub image: Option<DecodedImage>,
    /// Requested display size in grid cells (`c=`/`r=`); 0 means native pixels.
    pub cols: u32,
    pub rows: u32,
}

/// Maximum number of simultaneously-pending (chunked, `m=1`) kitty image streams.
/// A single kitty image may arrive in many APC chunks; we key them by id. Bounding
/// this prevents a malicious sequence from opening thousands of never-finished
/// streams and growing `pending` without limit.
const MAX_PENDING_STREAMS: usize = 64;

/// Maximum accumulated base64-decoded bytes per pending stream. A single kitty
/// image is at most MAX_IMAGE_DIM²·4 bytes; capping here prevents buffering an
/// arbitrarily large payload before we even know the format.
const MAX_PENDING_PAYLOAD_BYTES: usize = MAX_IMAGE_BYTES;

/// Accumulates kitty graphics commands across APC chunks (`m=1` continuations)
/// and yields a `GraphicsCommand` once a command completes (`m=0`).
#[derive(Default)]
pub struct KittyParser {
    /// Pending payloads keyed by image id, plus the controls from the first chunk.
    pending: HashMap<u32, Pending>,
    /// Monotonic counter for assigning synthetic ids to `i=0` streams.
    next_anon_id: u32,
}

struct Pending {
    controls: Controls,
    payload: Vec<u8>, // accumulated base64-decoded bytes
}

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
                .filter(|&k| k >= ANON_ID_BASE && k < SIXEL_ID_BASE)
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
        if !self.pending.contains_key(&id) && self.pending.len() >= MAX_PENDING_STREAMS {
            if let Some(&oldest) = self.pending.keys().min() {
                self.pending.remove(&oldest);
            }
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

/// Parsed `key=value,key=value` control block of a graphics command.
#[derive(Clone)]
struct Controls {
    action: Action,
    format: Format,
    id: u32,
    width: u32,  // s= (source width for raw formats)
    height: u32, // v= (source height for raw formats)
    cols: u32,   // c= (display width in cells; 0 = native pixels)
    rows: u32,   // r= (display height in cells; 0 = native pixels)
    more: bool,  // m=1 => more chunks follow
}

impl Controls {
    fn parse(s: &str) -> Self {
        let mut action = Action::TransmitAndDisplay; // kitty default for a key-less cmd is `a=t`, but display is the common intent
        let mut format = Format::Rgba;
        let mut id = 0;
        let mut width = 0;
        let mut height = 0;
        let mut cols = 0;
        let mut rows = 0;
        let mut more = false;
        for pair in s.split(',') {
            let Some((k, v)) = pair.split_once('=') else {
                continue;
            };
            match k {
                "a" => action = v.chars().next().map(Action::from).unwrap_or(action),
                "f" => {
                    format = match v {
                        "24" => Format::Rgb,
                        "100" => Format::Png,
                        _ => Format::Rgba,
                    }
                }
                "i" => id = v.parse().unwrap_or(0),
                "s" => width = v.parse().unwrap_or(0),
                "v" => height = v.parse().unwrap_or(0),
                "c" => cols = v.parse().unwrap_or(0),
                "r" => rows = v.parse().unwrap_or(0),
                "m" => more = v == "1",
                _ => {}
            }
        }
        Controls { action, format, id, width, height, cols, rows, more }
    }
}

/// Maximum accepted image dimension (px) per side for raw/sixel decoding. Guards
/// against malformed escape sequences declaring absurd sizes (overflow / OOM).
/// At 4096 px/side the worst-case RGBA allocation is 4096²·4 = 64 MB per image —
/// a tight but workable cap.
const MAX_IMAGE_DIM: u32 = 4096;

/// Maximum total bytes for a single decoded RGBA image (64 MB = MAX_IMAGE_DIM²·4).
const MAX_IMAGE_BYTES: usize = (MAX_IMAGE_DIM as usize) * (MAX_IMAGE_DIM as usize) * 4;

/// Maximum sixel canvas dimension (px) per side. Tighter than [`MAX_IMAGE_DIM`]
/// because sixel RLE (`!count`) can amplify a tiny stream into a huge canvas, so
/// this bounds worst-case memory (≈ 4096²·4 = 64 MB) for one malformed image.
const SIXEL_MAX_DIM: usize = 4096;

/// Decode reassembled payload bytes into RGBA per the declared format.
fn decode_payload(controls: &Controls, payload: &[u8]) -> Option<DecodedImage> {
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
fn decode_png(bytes: &[u8]) -> Option<DecodedImage> {
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
fn b64_decode(s: &str) -> Vec<u8> {
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

/// An image placed on the grid: which stored image, at which screen cell.
#[derive(Clone)]
pub struct Placement {
    pub id: u32,
    pub row: i32,
    pub col: usize,
    /// Display size in grid cells (`c=`/`r=`); 0 means draw at native pixels.
    pub cols: u32,
    pub rows: u32,
}

/// Holds images received from the PTY and where they should be drawn. Decoded
/// pixels are kept by id; `placements` is what the renderer draws. Images whose
/// command requested display land in `pending` until the loop anchors them at the
/// cursor (it knows the cursor position; the decoder does not).
#[derive(Default)]
pub struct ImageStore {
    by_id: HashMap<u32, DecodedImage>,
    placements: Vec<Placement>,
    /// Monotonic counter so the renderer can tell when the image set changed.
    pub revision: u64,
}

/// An image queued for display: the loop will anchor it at the cursor cell once
/// the VT bytes preceding the image in the stream have advanced the cursor.
#[derive(Clone, Copy)]
pub struct PendingDisplay {
    pub id: u32,
    pub cols: u32,
    pub rows: u32,
}

impl ImageStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply a fully-parsed graphics command. Pixels are stored immediately;
    /// placement-affecting actions (display, delete) are returned as ordered
    /// [`TapEvent`]s so the caller applies them in stream order. Returns `None`
    /// for transmit-only commands.
    pub fn apply(&mut self, cmd: GraphicsCommand) -> Option<TapEvent> {
        if let Some(image) = cmd.image {
            self.by_id.insert(cmd.id, image);
            // Cap kitty images (below SIXEL_ID_BASE) at MAX_KITTY_IMAGES. Evict
            // numerically-lowest id (oldest) plus any placements that reference it.
            let kitty_count =
                self.by_id.keys().filter(|&&k| k < SIXEL_ID_BASE).count();
            if kitty_count > MAX_KITTY_IMAGES {
                if let Some(&oldest) =
                    self.by_id.keys().filter(|&&k| k < SIXEL_ID_BASE).min()
                {
                    self.by_id.remove(&oldest);
                    self.placements.retain(|p| p.id != oldest);
                }
            }
            self.revision += 1;
        }
        match cmd.action {
            Action::Delete => Some(TapEvent::Delete(cmd.id)),
            Action::TransmitAndDisplay | Action::Display => {
                Some(TapEvent::Display(PendingDisplay { id: cmd.id, cols: cmd.cols, rows: cmd.rows }))
            }
            _ => None,
        }
    }

    /// Store decoded pixels under `id` without queuing a placement (used by the
    /// sixel path, which displays at the cursor via a separate Display event).
    ///
    /// Sixel images get a fresh monotonic id every time and are never redisplayed,
    /// so without bounding them a sixel animation/video would grow `by_id` forever.
    /// After inserting, cap the number of retained sixel images (ids in the
    /// `SIXEL_ID_BASE`+ range) and evict the oldest (lowest ids) plus any
    /// placements that reference them. Kitty images (lower ids, redisplayable via
    /// `a=p`) are untouched. The just-inserted image has the highest id, so it is
    /// never the one evicted.
    pub fn insert_pixels(&mut self, id: u32, image: DecodedImage) {
        self.by_id.insert(id, image);
        let mut sixel_ids: Vec<u32> = self
            .by_id
            .keys()
            .copied()
            .filter(|&k| k >= SIXEL_ID_BASE)
            .collect();
        if sixel_ids.len() > MAX_SIXEL_IMAGES {
            sixel_ids.sort_unstable();
            let evict = sixel_ids.len() - MAX_SIXEL_IMAGES;
            for &old in &sixel_ids[..evict] {
                self.by_id.remove(&old);
                self.placements.retain(|p| p.id != old);
            }
        }
        self.revision += 1;
    }

    /// Remove placements (kitty `a=d`): a specific id, or all when `id == 0`.
    /// Pixel data is retained so a later `a=p` can redisplay the same id.
    pub fn delete(&mut self, id: u32) {
        if id == 0 {
            self.placements.clear();
        } else {
            self.placements.retain(|p| p.id != id);
        }
        self.revision += 1;
    }

    /// Anchor an image at a screen cell with its requested cell size. Ignored if
    /// the id has no stored pixels (e.g. a display of a never-transmitted id).
    /// Caps the placement list at [`MAX_PLACEMENTS`] by dropping the oldest entry.
    pub fn place(&mut self, id: u32, row: i32, col: usize, cols: u32, rows: u32) {
        if !self.by_id.contains_key(&id) {
            return;
        }
        if self.placements.len() >= MAX_PLACEMENTS {
            self.placements.remove(0);
        }
        self.placements.push(Placement { id, row, col, cols, rows });
        self.revision += 1;
    }

    pub fn placements(&self) -> &[Placement] {
        &self.placements
    }

    pub fn image(&self, id: u32) -> Option<&DecodedImage> {
        self.by_id.get(&id)
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

/// Maximum bytes buffered per APC/DCS/OSC accumulation buffer in [`StreamTap`].
/// A single kitty or sixel payload should never exceed one decoded image worth of
/// base64 (≈ 87 MB at 4096² but almost always far smaller); we cap at 1 MB which
/// is generous for all realistic sequences. Bytes beyond the cap are simply
/// dropped — the sequence will fail to decode cleanly rather than OOM the process.
const TAP_BUF_CAP: usize = 1 << 20; // 1 MiB

/// Extracts kitty-graphics APC sequences from a PTY byte stream, feeding them to
/// a [`KittyParser`] (and an [`ImageStore`]) while returning the remaining bytes
/// for the VT parser. State persists across reads, so a sequence split across
/// `read()` boundaries is handled correctly.
///
/// APC sequences are `ESC _ <data> ST`, where ST is `ESC \` or BEL. Only `G`-led
/// (graphics) APCs are interpreted; any other APC is dropped (the VT parser would
/// discard it anyway), and everything else passes through untouched.
pub struct StreamTap {
    state: TapState,
    apc: Vec<u8>,
    kitty: KittyParser,
    dcs: Vec<u8>,
    /// OSC body accumulated for cwd (OSC 7) detection. OSC bytes are *also* passed
    /// through to the VT parser unchanged; this buffer only observes them.
    osc: Vec<u8>,
    /// Next id to assign to a decoded sixel image (which carry no kitty id).
    next_sixel_id: u32,
}

/// Synthetic ids for anonymous (`i=0`) kitty streams live in this range, well
/// above typical app-assigned ids and below the sixel range.
const ANON_ID_BASE: u32 = 0x4000_0000;

/// Sixel images have no protocol id, so they get synthetic ids from a high range
/// that cannot collide with app-chosen kitty ids or anonymous kitty ids.
const SIXEL_ID_BASE: u32 = 0x8000_0000;

/// Max number of decoded sixel images retained at once. Sixels are never
/// redisplayed, so old ones are evicted oldest-first — bounds memory for sixel
/// animations/video while keeping plenty of recent frames available.
const MAX_SIXEL_IMAGES: usize = 64;

/// Max number of kitty (non-sixel) images in `ImageStore::by_id`. When exceeded,
/// the numerically-lowest id (oldest assigned) is evicted plus its placements.
const MAX_KITTY_IMAGES: usize = 256;

/// Max number of placements recorded at once. When exceeded the oldest (index 0)
/// is dropped before the new one is appended, keeping the list bounded.
const MAX_PLACEMENTS: usize = 1024;

/// One ordered item produced by [`StreamTap::process`]: either VT bytes for the
/// parser, or a point at which an image should be displayed (anchored at the
/// cursor *after* the preceding `Vt` bytes have advanced it).
pub enum TapEvent {
    Vt(Vec<u8>),
    Display(PendingDisplay),
    /// Delete placements for an image id (0 = all). Ordered with displays so a
    /// delete that follows a display in the stream is applied after it.
    Delete(u32),
    /// OSC 7 reported a working directory (already percent-decoded, local host
    /// only). Surfaced so the UI can store it per-session for new-tab/split cwd
    /// inheritance. The original OSC bytes are still passed through to the parser.
    Cwd(std::path::PathBuf),
}

#[derive(PartialEq, Eq)]
enum TapState {
    Normal,
    Escape,    // saw ESC in normal text
    Apc,       // inside an APC body
    ApcEscape, // saw ESC inside an APC body (maybe ST)
    Dcs,       // inside a DCS body (ESC P ... ST) — sixel candidate
    DcsEscape, // saw ESC inside a DCS body (maybe ST)
    Osc,       // inside an OSC body (ESC ] ... ST) — observed for OSC 7, passed through
    OscEscape, // saw ESC inside an OSC body (maybe ST)
}

impl Default for StreamTap {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamTap {
    pub fn new() -> Self {
        Self {
            state: TapState::Normal,
            apc: Vec::new(),
            kitty: KittyParser::new(),
            dcs: Vec::new(),
            osc: Vec::new(),
            next_sixel_id: SIXEL_ID_BASE,
        }
    }

    /// Process `input`, routing kitty graphics commands into `store` and
    /// returning an ordered list of events: VT byte runs interleaved with image
    /// display points, so the caller can advance the parser and anchor each image
    /// at the cursor position it occupied at that point in the stream.
    pub fn process(&mut self, input: &[u8], store: &FairMutex<ImageStore>) -> Vec<TapEvent> {
        let mut events = Vec::new();
        let mut out: Vec<u8> = Vec::with_capacity(input.len());
        // Flush accumulated VT bytes (if any) before recording an image event.
        macro_rules! finish {
            () => {
                if let Some(ev) = self.finish_apc(store) {
                    if !out.is_empty() {
                        events.push(TapEvent::Vt(std::mem::take(&mut out)));
                    }
                    events.push(ev);
                }
            };
        }
        for &b in input {
            match self.state {
                TapState::Normal => {
                    if b == 0x1b {
                        self.state = TapState::Escape;
                    } else {
                        out.push(b);
                    }
                }
                TapState::Escape => {
                    if b == b'_' {
                        self.apc.clear();
                        self.state = TapState::Apc; // APC introducer; drop it
                    } else if b == b'P' {
                        self.dcs.clear();
                        self.state = TapState::Dcs; // DCS introducer; buffer (sixel?)
                    } else if b == b']' {
                        // OSC introducer. We only *observe* OSC (for OSC 7 cwd); the
                        // bytes still pass through to the VT parser, so emit `ESC ]`
                        // now and mirror the body into `out` as we buffer it.
                        self.osc.clear();
                        out.push(0x1b);
                        out.push(b']');
                        self.state = TapState::Osc;
                    } else if b == 0x1b {
                        out.push(0x1b); // another ESC; emit held ESC, stay
                    } else {
                        out.push(0x1b); // not an APC/DCS/OSC; emit held ESC + this byte
                        out.push(b);
                        self.state = TapState::Normal;
                    }
                }
                TapState::Apc => {
                    if b == 0x1b {
                        self.state = TapState::ApcEscape;
                    } else if b == 0x07 {
                        finish!(); // BEL terminator
                        self.state = TapState::Normal;
                    } else if self.apc.len() < TAP_BUF_CAP {
                        self.apc.push(b);
                    }
                    // Bytes beyond cap are silently dropped; the sequence will fail
                    // to decode but the process won't OOM.
                }
                TapState::ApcEscape => {
                    if b == b'\\' {
                        finish!(); // ST terminator (ESC \)
                        self.state = TapState::Normal;
                    } else {
                        // ESC was body, not terminator — push both if room.
                        if self.apc.len() + 1 < TAP_BUF_CAP {
                            self.apc.push(0x1b);
                            self.apc.push(b);
                        }
                        self.state = TapState::Apc;
                    }
                }
                TapState::Dcs => {
                    if b == 0x1b {
                        self.state = TapState::DcsEscape;
                    } else if self.dcs.len() < TAP_BUF_CAP {
                        self.dcs.push(b);
                    }
                }
                TapState::DcsEscape => {
                    if b == b'\\' {
                        // ST terminator. Sixel -> image event; any other DCS is
                        // reconstructed and passed through to the VT parser.
                        match self.finish_dcs(store) {
                            Some(ev) => {
                                if !out.is_empty() {
                                    events.push(TapEvent::Vt(std::mem::take(&mut out)));
                                }
                                events.push(ev);
                            }
                            None => {
                                out.push(0x1b);
                                out.push(b'P');
                                out.extend_from_slice(&self.dcs);
                                out.push(0x1b);
                                out.push(b'\\');
                                self.dcs.clear();
                            }
                        }
                        self.state = TapState::Normal;
                    } else {
                        // ESC was body, not terminator — push both if room.
                        if self.dcs.len() + 1 < TAP_BUF_CAP {
                            self.dcs.push(0x1b);
                            self.dcs.push(b);
                        }
                        self.state = TapState::Dcs;
                    }
                }
                TapState::Osc => {
                    if b == 0x1b {
                        self.state = TapState::OscEscape;
                    } else if b == 0x07 {
                        out.push(0x07); // BEL terminator (passed through)
                        if let Some(ev) = self.finish_osc() {
                            if !out.is_empty() {
                                events.push(TapEvent::Vt(std::mem::take(&mut out)));
                            }
                            events.push(ev);
                        }
                        self.state = TapState::Normal;
                    } else {
                        out.push(b); // mirror body byte to the parser always
                        if self.osc.len() < TAP_BUF_CAP {
                            self.osc.push(b); // observe for OSC 7 only while under cap
                        }
                    }
                }
                TapState::OscEscape => {
                    if b == b'\\' {
                        out.push(0x1b); // ST terminator (ESC \), passed through
                        out.push(b'\\');
                        if let Some(ev) = self.finish_osc() {
                            if !out.is_empty() {
                                events.push(TapEvent::Vt(std::mem::take(&mut out)));
                            }
                            events.push(ev);
                        }
                        self.state = TapState::Normal;
                    } else {
                        out.push(0x1b); // ESC was body, not terminator
                        out.push(b);
                        if self.osc.len() + 1 < TAP_BUF_CAP {
                            self.osc.push(0x1b);
                            self.osc.push(b);
                        }
                        self.state = TapState::Osc;
                    }
                }
            }
        }
        if !out.is_empty() {
            events.push(TapEvent::Vt(out));
        }
        events
    }

    fn finish_apc(&mut self, store: &FairMutex<ImageStore>) -> Option<TapEvent> {
        let ev = if self.apc.first() == Some(&b'G')
            && let Some(cmd) = self.kitty.feed(&self.apc[1..])
        {
            store.lock().apply(cmd)
        } else {
            None
        };
        self.apc.clear();
        ev
    }

    /// Finish a buffered DCS. If it is a sixel sequence (`<params> q <data>`,
    /// where the params before `q` are only digits/semicolons), decode it, store
    /// the pixels, and return a Display event. Otherwise return `None` so the
    /// caller passes the original DCS through to the VT parser. Clears the buffer
    /// only when consumed as sixel (passthrough needs the bytes intact).
    fn finish_dcs(&mut self, store: &FairMutex<ImageStore>) -> Option<TapEvent> {
        let q = self.dcs.iter().position(|&b| b == b'q')?;
        // Sixel params are digits and ';' only; anything else (e.g. DECRQSS `$q`)
        // means this is not sixel.
        if !self.dcs[..q].iter().all(|&b| b.is_ascii_digit() || b == b';') {
            return None;
        }
        let image = decode_sixel(&self.dcs[q + 1..])?;
        let id = self.next_sixel_id;
        self.next_sixel_id = self.next_sixel_id.wrapping_add(1).max(SIXEL_ID_BASE);
        store.lock().insert_pixels(id, image);
        self.dcs.clear();
        Some(TapEvent::Display(PendingDisplay { id, cols: 0, rows: 0 }))
    }

    /// Finish a buffered OSC. If it is OSC 7 (`7;file://<host>/<path>`) for the
    /// local host, return a `Cwd` event with the percent-decoded path. The OSC
    /// bytes were already passed through to the parser; this only observes them.
    /// Always clears the buffer.
    fn finish_osc(&mut self) -> Option<TapEvent> {
        let osc = std::mem::take(&mut self.osc);
        parse_osc7_cwd(&osc).map(TapEvent::Cwd)
    }
}

/// Parse an OSC 7 body (`7;file://<host>/<path>`) into a working-directory path,
/// percent-decoding the path and accepting only the local host (empty,
/// `localhost`, or this machine's hostname). Returns `None` for any other OSC,
/// a non-local host, or an undecodable path.
fn parse_osc7_cwd(body: &[u8]) -> Option<std::path::PathBuf> {
    let body = std::str::from_utf8(body).ok()?;
    // OSC code is the part before the first ';'. Must be exactly "7".
    let rest = body.strip_prefix("7;")?;
    let url = rest.strip_prefix("file://")?;
    // Split host authority (up to the first '/') from the path. A `file://` URL
    // without an explicit host (`file:///path`) yields an empty authority.
    let slash = url.find('/')?;
    let host = &url[..slash];
    if !host_is_local(host) {
        return None;
    }
    let path = percent_decode(&url[slash..]);
    let path = String::from_utf8(path).ok()?;
    if path.is_empty() {
        return None;
    }
    Some(std::path::PathBuf::from(path))
}

/// Whether an OSC 7 `file://` authority refers to the local machine: empty,
/// `localhost`, or the current hostname (case-insensitive). Remote hosts are
/// rejected so a cwd from another machine is never used to spawn a local shell.
///
/// Hostname is read from `/proc/sys/kernel/hostname` only — never from `$HOSTNAME`
/// which is a user-controlled environment variable and could be spoofed by a
/// malicious program running in the terminal.
fn host_is_local(host: &str) -> bool {
    if host.is_empty() || host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .is_some_and(|h| {
            // Match either the full hostname or its short (pre-dot) form.
            h.eq_ignore_ascii_case(host)
                || h.split('.').next().is_some_and(|s| s.eq_ignore_ascii_case(host))
        })
}

/// Percent-decode a byte string (`%XX` -> the byte). Invalid escapes are passed
/// through literally. Returns raw bytes (OSC 7 paths are UTF-8 in practice but
/// the protocol permits arbitrary bytes).
fn percent_decode(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_roundtrip_known_vectors() {
        assert_eq!(b64_decode("aGVsbG8="), b"hello");
        assert_eq!(b64_decode("Zm9vYmFy"), b"foobar");
        assert_eq!(b64_decode(""), b"");
        // whitespace (chunk newlines) is ignored
        assert_eq!(b64_decode("aGVs\nbG8="), b"hello");
    }

    #[test]
    fn parses_controls() {
        let c = Controls::parse("a=T,f=32,i=7,s=2,v=1,m=1");
        assert_eq!(c.action, Action::TransmitAndDisplay);
        assert_eq!(c.format, Format::Rgba);
        assert_eq!(c.id, 7);
        assert_eq!((c.width, c.height), (2, 1));
        assert!(c.more);
    }

    #[test]
    fn decodes_raw_rgba_command() {
        // 1x1 red pixel, RGBA, base64 of [255,0,0,255] = "/wAA/w=="
        let mut p = KittyParser::new();
        let cmd = p.feed(b"a=T,f=32,s=1,v=1;/wAA/w==").expect("complete command");
        assert_eq!(cmd.action, Action::TransmitAndDisplay);
        let img = cmd.image.expect("decoded image");
        assert_eq!((img.width, img.height), (1, 1));
        assert_eq!(img.rgba, vec![255, 0, 0, 255]);
    }

    #[test]
    fn decodes_16bit_png_to_8bit() {
        // Encode a 2x1 16-bit RGBA PNG (red, then green) and confirm decode_png
        // normalizes it to 8-bit RGBA (regression: 16-bit was read as noise).
        let mut png_bytes = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut png_bytes, 2, 1);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Sixteen);
            let mut w = enc.write_header().unwrap();
            // Big-endian 16-bit samples: red opaque, green opaque.
            let data: [u8; 16] = [
                0xFF, 0xFF, 0, 0, 0, 0, 0xFF, 0xFF, // red
                0, 0, 0xFF, 0xFF, 0, 0, 0xFF, 0xFF, // green
            ];
            w.write_image_data(&data).unwrap();
        }
        let img = decode_png(&png_bytes).expect("decoded");
        assert_eq!((img.width, img.height), (2, 1));
        assert_eq!(img.rgba, vec![255, 0, 0, 255, 0, 255, 0, 255]);
    }

    #[test]
    fn reassembles_chunked_payload() {
        // RGBA 1x2 split across two chunks (m=1 then m=0).
        let mut p = KittyParser::new();
        // pixel 1 = white, pixel 2 = black: [255,255,255,255, 0,0,0,255]
        assert!(p.feed(b"a=T,f=32,s=1,v=2,i=9,m=1;/////w==").is_none());
        let cmd = p.feed(b"i=9,m=0;AAAA/w==").expect("complete after final chunk");
        let img = cmd.image.expect("image");
        assert_eq!((img.width, img.height), (1, 2));
        assert_eq!(img.rgba, vec![255, 255, 255, 255, 0, 0, 0, 255]);
    }

    /// Concatenate the VT byte runs from a tap event list (dropping display points).
    fn vt_bytes(events: &[TapEvent]) -> Vec<u8> {
        let mut out = Vec::new();
        for ev in events {
            if let TapEvent::Vt(b) = ev {
                out.extend_from_slice(b);
            }
        }
        out
    }

    fn display_count(events: &[TapEvent]) -> usize {
        events.iter().filter(|e| matches!(e, TapEvent::Display(_))).count()
    }

    #[test]
    fn tap_strips_image_passes_text() {
        let store = FairMutex::new(ImageStore::new());
        let mut tap = StreamTap::new();
        let mut input = Vec::new();
        input.extend_from_slice(b"hi");
        // kitty APC: 1x1 red RGBA pixel.
        input.extend_from_slice(b"\x1b_Ga=T,f=32,s=1,v=1;/wAA/w==\x1b\\");
        input.extend_from_slice(b"!");
        let events = tap.process(&input, &store);
        assert_eq!(vt_bytes(&events), b"hi!"); // text passes through, image stripped
        assert_eq!(display_count(&events), 1); // one display point emitted
        assert_eq!(store.lock().len(), 1); // one image captured
    }

    #[test]
    fn tap_orders_display_after_preceding_text() {
        // The display point must come AFTER the "hi" text run so the cursor has
        // advanced past it before the image is anchored.
        let store = FairMutex::new(ImageStore::new());
        let mut tap = StreamTap::new();
        let events =
            tap.process(b"hi\x1b_Ga=T,f=32,s=1,v=1;/wAA/w==\x1b\\rest", &store);
        match (&events[0], &events[1], &events[2]) {
            (TapEvent::Vt(a), TapEvent::Display(_), TapEvent::Vt(b)) => {
                assert_eq!(a, b"hi");
                assert_eq!(b, b"rest");
            }
            _ => panic!("expected Vt, Display, Vt ordering, got {} events", events.len()),
        }
    }

    #[test]
    fn tap_handles_split_across_reads() {
        let store = FairMutex::new(ImageStore::new());
        let mut tap = StreamTap::new();
        // APC split mid-sequence across two process() calls.
        let a = tap.process(b"A\x1b_Ga=T,f=32,s=1,v=1;/wAA", &store);
        let b = tap.process(b"/w==\x1b\\B", &store);
        assert_eq!(vt_bytes(&a), b"A");
        assert_eq!(vt_bytes(&b), b"B");
        assert_eq!(store.lock().len(), 1);
    }

    #[test]
    fn tap_orders_delete_after_display() {
        // A display followed by a delete of the same id must emit Display *then*
        // Delete, so the loop places the image before removing it (regression:
        // applying delete eagerly inside process() ran it before placement).
        let store = FairMutex::new(ImageStore::new());
        let mut tap = StreamTap::new();
        let events = tap.process(
            b"\x1b_Ga=T,f=32,s=1,v=1,i=3;/wAA/w==\x1b\\\x1b_Ga=d,i=3\x1b\\",
            &store,
        );
        let kinds: Vec<_> = events
            .iter()
            .map(|e| match e {
                TapEvent::Vt(_) => "vt",
                TapEvent::Display(_) => "display",
                TapEvent::Delete(_) => "delete",
                TapEvent::Cwd(_) => "cwd",
            })
            .collect();
        assert_eq!(kinds, vec!["display", "delete"]);
    }

    #[test]
    fn rejects_oversized_raw_image_dims() {
        // Malformed: absurd dimensions whose w*h*4 would overflow u32. Must be
        // rejected, not accepted with a wrapped size or panic.
        let c = Controls {
            action: Action::TransmitAndDisplay,
            format: Format::Rgba,
            id: 1,
            width: 65536,
            height: 65536,
            cols: 0,
            rows: 0,
            more: false,
        };
        assert!(decode_payload(&c, &[1, 2, 3, 4]).is_none());
    }

    #[test]
    fn sixel_huge_repeat_is_bounded() {
        // A tiny stream with a 4-billion repeat must not OOM/hang: the canvas is
        // capped, so this returns quickly with width clamped to the cap.
        let img = decode_sixel(b"#0;2;100;0;0!4000000000~").expect("decoded");
        assert!(img.width as usize <= SIXEL_MAX_DIM);
        assert_eq!(img.height, 6);
        assert!(img.rgba.len() <= SIXEL_MAX_DIM * 6 * 4);
    }

    #[test]
    fn decodes_basic_sixel() {
        // One color (red), one full sixel column `~` (all six bits) -> 1x6 red.
        let img = decode_sixel(b"#0;2;100;0;0~").expect("decoded");
        assert_eq!((img.width, img.height), (1, 6));
        assert_eq!(&img.rgba[0..4], &[255, 0, 0, 255]);
        // All six rows are red.
        assert!(img.rgba.chunks_exact(4).all(|p| p == [255, 0, 0, 255]));
    }

    #[test]
    fn sixel_repeat_and_newline() {
        // `!3~` -> three columns; `-` -> next band; one column. Width 3, height 12.
        let img = decode_sixel(b"#0;2;0;100;0!3~-#0;2;0;100;0~").expect("decoded");
        assert_eq!((img.width, img.height), (3, 12));
        // Top-left is green; the second band's columns 1..2 were never written
        // (transparent).
        assert_eq!(&img.rgba[0..4], &[0, 255, 0, 255]);
        let band2 = (6 * 3 + 1) * 4; // row 6, col 1
        assert_eq!(&img.rgba[band2..band2 + 4], &[0, 0, 0, 0]);
    }

    #[test]
    fn tap_routes_sixel_dcs_to_display() {
        let store = FairMutex::new(ImageStore::new());
        let mut tap = StreamTap::new();
        let events = tap.process(b"x\x1bPq#0;2;100;0;0~\x1b\\y", &store);
        assert_eq!(vt_bytes(&events), b"xy");
        assert_eq!(display_count(&events), 1);
        assert_eq!(store.lock().len(), 1);
    }

    #[test]
    fn tap_passes_through_non_sixel_dcs() {
        // A DECRQSS-style DCS (`$q...`) is not sixel and must reach the parser.
        let store = FairMutex::new(ImageStore::new());
        let mut tap = StreamTap::new();
        let events = tap.process(b"\x1bP$q\"p\x1b\\", &store);
        assert_eq!(vt_bytes(&events), b"\x1bP$q\"p\x1b\\");
        assert_eq!(display_count(&events), 0);
        assert_eq!(store.lock().len(), 0);
    }

    /// The first `Cwd` event's path, if any.
    fn first_cwd(events: &[TapEvent]) -> Option<std::path::PathBuf> {
        events.iter().find_map(|e| match e {
            TapEvent::Cwd(p) => Some(p.clone()),
            _ => None,
        })
    }

    #[test]
    fn parses_osc7_cwd_basic() {
        // file:// with empty host (file:///path) -> the decoded path.
        assert_eq!(
            parse_osc7_cwd(b"7;file:///home/alice/projects"),
            Some(std::path::PathBuf::from("/home/alice/projects"))
        );
        // "localhost" host is accepted.
        assert_eq!(
            parse_osc7_cwd(b"7;file://localhost/var/log"),
            Some(std::path::PathBuf::from("/var/log"))
        );
    }

    #[test]
    fn osc7_percent_decodes_path() {
        // A space encoded as %20 must round-trip to a real space.
        assert_eq!(
            parse_osc7_cwd(b"7;file:///home/alice/My%20Code"),
            Some(std::path::PathBuf::from("/home/alice/My Code"))
        );
    }

    #[test]
    fn osc7_rejects_remote_host_and_other_oscs() {
        // A non-local host is rejected (don't cd into another machine's path).
        assert_eq!(parse_osc7_cwd(b"7;file://otherbox/home/bob"), None);
        // Other OSC codes are not cwd reports.
        assert_eq!(parse_osc7_cwd(b"0;some window title"), None);
        assert_eq!(parse_osc7_cwd(b"2;title"), None);
        // OSC 7 without a file:// URL.
        assert_eq!(parse_osc7_cwd(b"7;not-a-url"), None);
    }

    #[test]
    fn tap_observes_osc7_and_passes_it_through() {
        // OSC 7 must yield a Cwd event AND still reach the VT parser unchanged
        // (BEL-terminated form here), with surrounding text intact.
        let store = FairMutex::new(ImageStore::new());
        let mut tap = StreamTap::new();
        let events = tap.process(b"a\x1b]7;file:///tmp/work\x07b", &store);
        assert_eq!(vt_bytes(&events), b"a\x1b]7;file:///tmp/work\x07b");
        assert_eq!(first_cwd(&events), Some(std::path::PathBuf::from("/tmp/work")));
    }

    #[test]
    fn tap_osc7_st_terminated() {
        // The ST (ESC \) terminated form is handled too, and passed through.
        let store = FairMutex::new(ImageStore::new());
        let mut tap = StreamTap::new();
        let events = tap.process(b"\x1b]7;file:///srv\x1b\\", &store);
        assert_eq!(vt_bytes(&events), b"\x1b]7;file:///srv\x1b\\");
        assert_eq!(first_cwd(&events), Some(std::path::PathBuf::from("/srv")));
    }

    #[test]
    fn tap_passes_non_cwd_osc_through() {
        // A title OSC (OSC 0) must reach the parser untouched and yield no Cwd.
        let store = FairMutex::new(ImageStore::new());
        let mut tap = StreamTap::new();
        let events = tap.process(b"\x1b]0;my title\x07", &store);
        assert_eq!(vt_bytes(&events), b"\x1b]0;my title\x07");
        assert_eq!(first_cwd(&events), None);
    }

    #[test]
    fn tap_osc7_split_across_reads() {
        // An OSC split mid-sequence across two process() calls still parses and
        // passes through, exercising the persistent OSC buffer.
        let store = FairMutex::new(ImageStore::new());
        let mut tap = StreamTap::new();
        let a = tap.process(b"\x1b]7;file:///home/a", &store);
        let b = tap.process(b"/b\x07", &store);
        let mut vt = vt_bytes(&a);
        vt.extend_from_slice(&vt_bytes(&b));
        assert_eq!(vt, b"\x1b]7;file:///home/a/b\x07");
        assert_eq!(first_cwd(&b), Some(std::path::PathBuf::from("/home/a/b")));
    }

    #[test]
    fn sixel_images_are_bounded() {
        // Inserting many sixel images (as an animation would) must not grow the
        // store without bound; the newest stays, the oldest are evicted.
        let mut store = ImageStore::new();
        let img = || DecodedImage { width: 1, height: 1, rgba: vec![1, 2, 3, 4] };
        for k in 0..(MAX_SIXEL_IMAGES as u32 + 50) {
            store.insert_pixels(SIXEL_ID_BASE + k, img());
        }
        assert_eq!(store.len(), MAX_SIXEL_IMAGES);
        // The most recent id is retained; an early one is evicted.
        assert!(store.image(SIXEL_ID_BASE + MAX_SIXEL_IMAGES as u32 + 49).is_some());
        assert!(store.image(SIXEL_ID_BASE).is_none());
    }

    #[test]
    fn sixel_eviction_keeps_kitty_images() {
        // Kitty images (low ids) must survive sixel eviction churn.
        let mut store = ImageStore::new();
        let img = || DecodedImage { width: 1, height: 1, rgba: vec![9, 9, 9, 9] };
        store.insert_pixels(7, img()); // a kitty-range id
        for k in 0..(MAX_SIXEL_IMAGES as u32 + 10) {
            store.insert_pixels(SIXEL_ID_BASE + k, img());
        }
        assert!(store.image(7).is_some());
    }

    #[test]
    fn delete_removes_placements_keeps_pixels() {
        let mut store = ImageStore::new();
        let img = DecodedImage { width: 1, height: 1, rgba: vec![1, 2, 3, 4] };
        store.by_id.insert(5, img);
        store.place(5, 0, 0, 0, 0);
        assert_eq!(store.placements().len(), 1);
        store.delete(5);
        assert_eq!(store.placements().len(), 0); // placement gone
        assert!(store.image(5).is_some()); // pixels retained for redisplay
    }

    #[test]
    fn id0_streams_get_distinct_synthetic_ids() {
        // Two consecutive i=0 images must not merge their payloads.
        // Each is a 1x1 RGBA pixel: red then green.
        let mut p = KittyParser::new();
        // Red pixel: base64 of [255,0,0,255] = "/wAA/w=="
        let cmd1 = p.feed(b"a=T,f=32,s=1,v=1,i=0;/wAA/w==").expect("first command");
        // Green pixel: base64 of [0,255,0,255] = "AP8A/w=="
        let cmd2 = p.feed(b"a=T,f=32,s=1,v=1,i=0;AP8A/w==").expect("second command");
        // The ids assigned must be distinct.
        assert_ne!(cmd1.id, cmd2.id, "i=0 images must get distinct synthetic ids");
        let img1 = cmd1.image.expect("first image decoded");
        let img2 = cmd2.image.expect("second image decoded");
        assert_eq!(img1.rgba, vec![255, 0, 0, 255], "first image should be red");
        assert_eq!(img2.rgba, vec![0, 255, 0, 255], "second image should be green");
    }

    #[test]
    fn placements_are_bounded() {
        // Placements must not grow past MAX_PLACEMENTS; old ones are dropped.
        let mut store = ImageStore::new();
        let img = DecodedImage { width: 1, height: 1, rgba: vec![1, 2, 3, 4] };
        store.by_id.insert(1, img);
        for i in 0..(MAX_PLACEMENTS + 10) as i32 {
            store.place(1, i, 0, 0, 0);
        }
        assert_eq!(store.placements().len(), MAX_PLACEMENTS);
        // The most recently placed row must be present; the very first must be gone.
        assert_eq!(store.placements().last().unwrap().row, MAX_PLACEMENTS as i32 + 9);
    }

    #[test]
    fn png_size_limit_rejects_oversized_declared_dim() {
        // A PNG that declares dimensions beyond MAX_IMAGE_DIM must be rejected.
        // We construct a valid PNG but with an intentionally oversized header.
        // Because max dim is now 4096, a 4097x1 PNG should be rejected.
        // Build a minimal 4097x1 RGBA PNG.
        let mut png_bytes = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut png_bytes, MAX_IMAGE_DIM + 1, 1);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header().unwrap();
            let row = vec![0u8; (MAX_IMAGE_DIM as usize + 1) * 4];
            w.write_image_data(&row).unwrap();
        }
        assert!(decode_png(&png_bytes).is_none(), "oversized PNG must be rejected");
    }
}
