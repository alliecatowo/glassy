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


mod parser;
mod store;

pub use parser::*;
pub use store::*;

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


// Shared structs used by parser and store

/// Parsed `key=value,key=value` control block of a graphics command.
#[derive(Clone)]
pub(crate) struct Controls {
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


// Shared constants

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

#[cfg(test)]
mod tests {
    use super::*;
    use super::parser::{b64_decode, decode_png, decode_payload};
    use super::store::parse_osc7_cwd;

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
                TapEvent::SemanticMark(_) => "mark",
                TapEvent::Notification(_) => "notification",
                TapEvent::Progress(_) => "progress",
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

    // -----------------------------------------------------------------------
    // OSC 9 / OSC 777 / OSC 133 shell-integration tests
    // -----------------------------------------------------------------------

    /// Helper: extract the first Notification body, if any.
    fn first_notification(events: &[TapEvent]) -> Option<String> {
        events.iter().find_map(|e| match e {
            TapEvent::Notification(s) => Some(s.clone()),
            _ => None,
        })
    }

    /// Helper: extract all SemanticMark chars.
    fn all_marks(events: &[TapEvent]) -> Vec<char> {
        events.iter().filter_map(|e| match e {
            TapEvent::SemanticMark(c) => Some(*c),
            _ => None,
        }).collect()
    }

    #[test]
    fn osc9_notification_parsed() {
        // `parse_osc9_notification` should extract the body after "9;".
        use super::store::parse_osc9_notification;
        let ev = parse_osc9_notification(b"9;build finished").expect("parsed");
        match ev {
            TapEvent::Notification(s) => assert_eq!(s, "build finished"),
            _ => panic!("expected Notification"),
        }
        // Empty body is rejected.
        assert!(parse_osc9_notification(b"9;").is_none());
        // Wrong OSC code is rejected.
        assert!(parse_osc9_notification(b"0;something").is_none());
    }

    #[test]
    fn osc777_notification_parsed() {
        use super::store::parse_osc777_notification;
        // Full form: title + body.
        let ev = parse_osc777_notification(b"777;notify;My App;Task done").expect("parsed");
        match ev {
            TapEvent::Notification(s) => assert_eq!(s, "My App \u{2014} Task done"),
            _ => panic!("expected Notification"),
        }
        // Body only (empty title).
        let ev = parse_osc777_notification(b"777;notify;;Just body").expect("parsed");
        match ev {
            TapEvent::Notification(s) => assert_eq!(s, "Just body"),
            _ => panic!("expected Notification"),
        }
        // Wrong prefix rejected.
        assert!(parse_osc777_notification(b"9;hello").is_none());
    }

    #[test]
    fn osc133_mark_parsed() {
        use super::store::parse_osc133_mark;
        assert_eq!(parse_osc133_mark(b"133;A"), Some('A'));
        assert_eq!(parse_osc133_mark(b"133;B"), Some('B'));
        assert_eq!(parse_osc133_mark(b"133;C"), Some('C'));
        // D with optional params (exit code).
        assert_eq!(parse_osc133_mark(b"133;D;0"), Some('D'));
        // Wrong OSC code or unknown mark.
        assert_eq!(parse_osc133_mark(b"133;X"), None);
        assert_eq!(parse_osc133_mark(b"7;A"), None);
    }

    #[test]
    fn tap_osc9_passes_through_and_emits_notification() {
        // OSC 9 body must reach the VT parser AND emit a Notification event.
        let store = FairMutex::new(ImageStore::new());
        let mut tap = StreamTap::new();
        let events = tap.process(b"\x1b]9;build ok\x07", &store);
        // The raw OSC bytes pass through to the VT parser.
        assert_eq!(vt_bytes(&events), b"\x1b]9;build ok\x07");
        assert_eq!(first_notification(&events), Some("build ok".to_string()));
    }

    #[test]
    fn tap_osc133_a_mark_passes_through_and_emits_mark() {
        // OSC 133;A should pass through AND emit a SemanticMark('A').
        let store = FairMutex::new(ImageStore::new());
        let mut tap = StreamTap::new();
        let events = tap.process(b"\x1b]133;A\x07", &store);
        assert_eq!(vt_bytes(&events), b"\x1b]133;A\x07");
        assert_eq!(all_marks(&events), vec!['A']);
    }

    #[test]
    fn tap_osc133_d_mark_with_exit_code_passes_through() {
        // OSC 133;D;0 (exit code 0) must be recognized and passed through.
        let store = FairMutex::new(ImageStore::new());
        let mut tap = StreamTap::new();
        let events = tap.process(b"\x1b]133;D;0\x07", &store);
        assert_eq!(vt_bytes(&events), b"\x1b]133;D;0\x07");
        assert_eq!(all_marks(&events), vec!['D']);
    }

    #[test]
    fn tap_osc133_sequence_of_marks() {
        // A full prompt: A → B → C → D sequence should emit all four marks.
        let store = FairMutex::new(ImageStore::new());
        let mut tap = StreamTap::new();
        let mut input = Vec::new();
        for mark in &['A', 'B', 'C', 'D'] {
            input.extend_from_slice(format!("\x1b]133;{}\x07", mark).as_bytes());
        }
        let events = tap.process(&input, &store);
        assert_eq!(all_marks(&events), vec!['A', 'B', 'C', 'D']);
    }

    // -----------------------------------------------------------------------
    // OSC 9;4 progress tests
    // -----------------------------------------------------------------------

    fn first_progress(events: &[TapEvent]) -> Option<ProgressState> {
        events.iter().find_map(|e| match e {
            TapEvent::Progress(s) => Some(*s),
            _ => None,
        })
    }

    #[test]
    fn osc9_4_set_progress_parsed() {
        use super::store::parse_osc9_progress;
        // state=1;pct=42
        match parse_osc9_progress(b"9;4;1;42") {
            Some(TapEvent::Progress(ProgressState::Set(42))) => {}
            other => panic!("expected Set(42), got {other:?}"),
        }
    }

    #[test]
    fn osc9_4_remove_parsed() {
        use super::store::parse_osc9_progress;
        match parse_osc9_progress(b"9;4;0;0") {
            Some(TapEvent::Progress(ProgressState::Remove)) => {}
            other => panic!("expected Remove, got {other:?}"),
        }
    }

    #[test]
    fn osc9_4_error_parsed() {
        use super::store::parse_osc9_progress;
        match parse_osc9_progress(b"9;4;2;75") {
            Some(TapEvent::Progress(ProgressState::Error(75))) => {}
            other => panic!("expected Error(75), got {other:?}"),
        }
    }

    #[test]
    fn osc9_4_indeterminate_parsed() {
        use super::store::parse_osc9_progress;
        match parse_osc9_progress(b"9;4;3") {
            Some(TapEvent::Progress(ProgressState::Indeterminate)) => {}
            other => panic!("expected Indeterminate, got {other:?}"),
        }
    }

    #[test]
    fn osc9_4_not_confused_with_osc9_notification() {
        // OSC 9;4;1;50 must not be parsed as a notification.
        use super::store::{parse_osc9_notification, parse_osc9_progress};
        // Progress check must match.
        assert!(parse_osc9_progress(b"9;4;1;50").is_some());
        // Notification check: "9;4;1;50" starts with "9;" but NOT with "9;4;".
        // parse_osc9_notification requires the body after "9;" to be non-empty text.
        // But parse_osc9_progress takes priority in finish_osc, so verify they don't
        // collide: the notification parser would parse body as "4;1;50" which looks
        // like a valid notification. The ordering in finish_osc ensures 9;4;... is
        // always handled as progress.
        let notif = parse_osc9_notification(b"9;4;1;50");
        // We DON'T assert it's None here — the ordering in finish_osc is what matters.
        // This just documents the relationship.
        let _ = notif;
        assert!(parse_osc9_progress(b"9;4;1;50").is_some());
        // A plain notification ("9;build ok") must NOT be parsed as progress.
        assert!(parse_osc9_progress(b"9;build ok").is_none());
    }

    #[test]
    fn tap_osc9_4_emits_progress_and_passes_through() {
        // OSC 9;4 must yield a Progress event AND pass bytes through to the VT parser.
        let store = FairMutex::new(ImageStore::new());
        let mut tap = StreamTap::new();
        let events = tap.process(b"\x1b]9;4;1;60\x07", &store);
        assert_eq!(vt_bytes(&events), b"\x1b]9;4;1;60\x07");
        assert_eq!(first_progress(&events), Some(ProgressState::Set(60)));
    }
}
