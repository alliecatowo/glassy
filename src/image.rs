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
        Some(GraphicsCommand { action: controls.action, id, image })
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
    more: bool,  // m=1 => more chunks follow
}

impl Controls {
    fn parse(s: &str) -> Self {
        let mut action = Action::TransmitAndDisplay; // kitty default for a key-less cmd is `a=t`, but display is the common intent
        let mut format = Format::Rgba;
        let mut id = 0;
        let mut width = 0;
        let mut height = 0;
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
                "m" => more = v == "1",
                _ => {}
            }
        }
        Controls { action, format, id, width, height, more }
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
}
