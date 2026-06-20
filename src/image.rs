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

/// Accumulates kitty graphics commands across APC chunks (`m=1` continuations)
/// and yields a `GraphicsCommand` once a command completes (`m=0`).
#[derive(Default)]
pub struct KittyParser {
    /// Pending payloads keyed by image id, plus the controls from the first chunk.
    pending: HashMap<u32, Pending>,
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
        let controls = Controls::parse(control_str);
        let chunk = b64_decode(payload_b64.trim());

        let id = controls.id;
        let more = controls.more;

        // Append to (or start) the pending buffer for this id.
        let entry = self.pending.entry(id).or_insert_with(|| Pending {
            controls: controls.clone(),
            payload: Vec::new(),
        });
        entry.payload.extend_from_slice(&chunk);

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

/// Decode reassembled payload bytes into RGBA per the declared format.
fn decode_payload(controls: &Controls, payload: &[u8]) -> Option<DecodedImage> {
    if payload.is_empty() {
        return None;
    }
    match controls.format {
        Format::Png => decode_png(payload),
        Format::Rgba => {
            let (w, h) = (controls.width, controls.height);
            (w * h * 4 == payload.len() as u32).then(|| DecodedImage {
                width: w,
                height: h,
                rgba: payload.to_vec(),
            })
        }
        Format::Rgb => {
            let (w, h) = (controls.width, controls.height);
            if w * h * 3 != payload.len() as u32 {
                return None;
            }
            let mut rgba = Vec::with_capacity((w * h * 4) as usize);
            for px in payload.chunks_exact(3) {
                rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
            Some(DecodedImage { width: w, height: h, rgba })
        }
    }
}

/// Decode a PNG into tightly-packed RGBA8 via the `png` crate.
fn decode_png(bytes: &[u8]) -> Option<DecodedImage> {
    let decoder = png::Decoder::new(bytes);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width, info.height);
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
    pub fn place(&mut self, id: u32, row: i32, col: usize, cols: u32, rows: u32) {
        if !self.by_id.contains_key(&id) {
            return;
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
}

/// One ordered item produced by [`StreamTap::process`]: either VT bytes for the
/// parser, or a point at which an image should be displayed (anchored at the
/// cursor *after* the preceding `Vt` bytes have advanced it).
pub enum TapEvent {
    Vt(Vec<u8>),
    Display(PendingDisplay),
    /// Delete placements for an image id (0 = all). Ordered with displays so a
    /// delete that follows a display in the stream is applied after it.
    Delete(u32),
}

#[derive(PartialEq, Eq)]
enum TapState {
    Normal,
    Escape,    // saw ESC in normal text
    Apc,       // inside an APC body
    ApcEscape, // saw ESC inside an APC body (maybe ST)
}

impl Default for StreamTap {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamTap {
    pub fn new() -> Self {
        Self { state: TapState::Normal, apc: Vec::new(), kitty: KittyParser::new() }
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
                    } else if b == 0x1b {
                        out.push(0x1b); // another ESC; emit held ESC, stay
                    } else {
                        out.push(0x1b); // not an APC; emit held ESC + this byte
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
                    } else {
                        self.apc.push(b);
                    }
                }
                TapState::ApcEscape => {
                    if b == b'\\' {
                        finish!(); // ST terminator (ESC \)
                        self.state = TapState::Normal;
                    } else {
                        self.apc.push(0x1b); // ESC was body, not terminator
                        self.apc.push(b);
                        self.state = TapState::Apc;
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
            })
            .collect();
        assert_eq!(kinds, vec!["display", "delete"]);
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
}
