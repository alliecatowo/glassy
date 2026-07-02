//! Image store (placement registry) and stream-tap for OSC sequences.

use super::*;

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
    pub(crate) by_id: HashMap<u32, DecodedImage>,
    placements: Vec<Placement>,
    /// Monotonic counter so the renderer can tell when the image set changed.
    pub revision: u64,
}

/// An image queued for display: the loop will anchor it at the cursor cell once
/// the VT bytes preceding the image in the stream have advanced the cursor.
#[derive(Clone, Copy, Debug)]
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
            let kitty_count = self.by_id.keys().filter(|&&k| k < SIXEL_ID_BASE).count();
            if kitty_count > MAX_KITTY_IMAGES
                && let Some(&oldest) = self.by_id.keys().filter(|&&k| k < SIXEL_ID_BASE).min()
            {
                self.by_id.remove(&oldest);
                self.placements.retain(|p| p.id != oldest);
            }
            self.revision += 1;
        }
        match cmd.action {
            Action::Delete => Some(TapEvent::Delete(cmd.id)),
            Action::TransmitAndDisplay | Action::Display => {
                Some(TapEvent::Display(PendingDisplay {
                    id: cmd.id,
                    cols: cmd.cols,
                    rows: cmd.rows,
                }))
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
        self.placements.push(Placement {
            id,
            row,
            col,
            cols,
            rows,
        });
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

/// OSC 9;4 progress state. Mirrors the ConEmu/Windows Terminal progress-state API.
/// The sequence is `ESC ] 9 ; 4 ; <state> ; <pct> ST` where `pct` is 0-100.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgressState {
    /// state=0: remove the progress indicator.
    Remove,
    /// state=1: active progress at the given percentage (0-100).
    Set(u8),
    /// state=2: error state (failed / exit ≠ 0). Percentage is advisory.
    Error(u8),
    /// state=3: indeterminate / no percentage known.
    Indeterminate,
}

/// One ordered item produced by [`StreamTap::process`]: either VT bytes for the
/// parser, or a point at which an image should be displayed (anchored at the
/// cursor *after* the preceding `Vt` bytes have advanced it).
#[derive(Debug)]
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
    /// OSC 133 shell-integration semantic mark. The mark character is one of:
    /// `A` (prompt start), `B` (command start), `C` (command executed), `D`
    /// (command finished). For a `D` mark the optional `i32` is the command's
    /// exit code (`133;D;<exit>`); it is `None` for A/B/C and for a bare `D`
    /// without an exit code. The original OSC bytes are still passed through to
    /// the VT parser. Used to record per-command zones (prompt rows, exit status,
    /// duration) for command-block grouping + jump-to-prompt navigation.
    SemanticMark(char, Option<i32>),
    /// OSC 9 (iTerm2 / ConEmu style) or OSC 777 (terminal-notifier style) desktop
    /// notification request from the running shell. The payload is a structured
    /// [`NotifySpec`] (title/body/icon/sound/urgency/actions) parsed from the
    /// sequence. The original OSC bytes are still passed through to the VT parser.
    Notify(super::NotifySpec),
    /// OSC 9;4 progress report. Forwarded to the UI thread so it can render a
    /// subtle progress indicator in the status bar and/or tab chip. The original
    /// OSC bytes are still passed through to the VT parser unchanged.
    Progress(ProgressState),
    /// OSC 1337 `Peek=<path>` inline-preview request (a glassy extension, e.g.
    /// emitted by a `glassy-peek <file>` helper). The UI thread reads a small head
    /// of the file and shows a syntax/markdown peek card near the cursor. The
    /// original OSC bytes are still passed through to the VT parser unchanged.
    Peek(std::path::PathBuf),
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
                        if let Some(ev) = self.finish_osc(store) {
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
                        if let Some(ev) = self.finish_osc(store) {
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
        if !self.dcs[..q]
            .iter()
            .all(|&b| b.is_ascii_digit() || b == b';')
        {
            return None;
        }
        let image = decode_sixel(&self.dcs[q + 1..])?;
        let id = self.next_sixel_id;
        self.next_sixel_id = self.next_sixel_id.wrapping_add(1).max(SIXEL_ID_BASE);
        store.lock().insert_pixels(id, image);
        self.dcs.clear();
        Some(TapEvent::Display(PendingDisplay {
            id,
            cols: 0,
            rows: 0,
        }))
    }

    /// Finish a buffered OSC. Dispatches to specialised parsers for:
    /// - OSC 7: cwd (returns a [`TapEvent::Cwd`])
    /// - OSC 9;4: progress report (returns a [`TapEvent::Progress`])
    /// - OSC 9 / OSC 777: desktop notification request (returns a
    ///   [`TapEvent::Notify`])
    /// - OSC 133: shell-integration semantic mark (returns a
    ///   [`TapEvent::SemanticMark`])
    ///
    /// The raw OSC bytes are always passed through to the VT parser already (before
    /// this method is called); this only observes them for side-effects.
    /// Always clears the buffer.
    fn finish_osc(&mut self, store: &FairMutex<ImageStore>) -> Option<TapEvent> {
        let osc = std::mem::take(&mut self.osc);
        // OSC 1337 File= — iTerm2 inline image protocol (so `imgcat` works). Decode
        // the embedded base64 image and store it, returning a Display event so the
        // PTY loop anchors it at the cursor (mirroring the kitty/sixel image path).
        // Checked before the generic OSC 1337 Peek= and OSC 7 so File= is not
        // mis-routed. Only PNG is decodable (glassy ships no JPEG/GIF decoder), so a
        // non-PNG payload yields no event and passes through harmlessly.
        if let Some(image) = parse_osc1337_file(&osc) {
            let id = self.next_sixel_id;
            self.next_sixel_id = self.next_sixel_id.wrapping_add(1).max(SIXEL_ID_BASE);
            store.lock().insert_pixels(id, image);
            return Some(TapEvent::Display(PendingDisplay {
                id,
                cols: 0,
                rows: 0,
            }));
        }
        // Try OSC 7 first — most common shell-integration sequence.
        if let Some(path) = parse_osc7_cwd(&osc) {
            return Some(TapEvent::Cwd(path));
        }
        // OSC 9;4 — progress report: `9;4;<state>;<pct>`. Check BEFORE the
        // generic OSC 9 notification so `9;4;...` is not mis-parsed as a
        // notification whose body starts with "4;...".
        if let Some(ev) = parse_osc9_progress(&osc) {
            return Some(ev);
        }
        // OSC 9 — iTerm2 / ConEmu desktop notification: `9;<message>` (plus the
        // glassy rich `key=value;…;body` prefix). Parsed into a structured spec.
        if let Some(spec) = super::parse_osc9(&osc) {
            return Some(TapEvent::Notify(spec));
        }
        // OSC 777 — terminal-notifier / Kitty desktop notification:
        // `777;notify;<title>;<body>[;key=value…]`.
        if let Some(spec) = super::parse_osc777(&osc) {
            return Some(TapEvent::Notify(spec));
        }
        // OSC 1337 — glassy inline-preview request: `1337;Peek=<path>`.
        if let Some(ev) = parse_osc1337_peek(&osc) {
            return Some(ev);
        }
        // OSC 133 — shell-integration semantic marks: `133;A`, `133;B`, etc.
        // (and `133;D;<exit>` carrying the command's exit code).
        parse_osc133_mark(&osc).map(|(mark, exit)| TapEvent::SemanticMark(mark, exit))
    }
}

/// Parse an OSC 7 body (`7;file://<host>/<path>`) into a working-directory path,
/// percent-decoding the path and accepting only the local host (empty,
/// `localhost`, or this machine's hostname). Returns `None` for any other OSC,
/// a non-local host, or an undecodable path.
pub(crate) fn parse_osc7_cwd(body: &[u8]) -> Option<std::path::PathBuf> {
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

/// Parse an OSC 9;4 progress body (`9;4;<state>;<pct>`) into a
/// [`TapEvent::Progress`]. The format is from the ConEmu / Windows Terminal
/// progress-bar protocol: state 0=remove, 1=set progress (pct 0-100), 2=error,
/// 3=indeterminate. Returns `None` for any body that is not a valid OSC 9;4
/// sequence (wrong prefix, missing fields, etc.).
pub(crate) fn parse_osc9_progress(body: &[u8]) -> Option<TapEvent> {
    let body = std::str::from_utf8(body).ok()?;
    // Must start with "9;4;"
    let rest = body.strip_prefix("9;4;")?;
    // Split into state and optional pct fields.
    let (state_str, pct_str) = match rest.split_once(';') {
        Some((s, p)) => (s, p),
        None => (rest, ""),
    };
    let state: u8 = state_str.parse().ok()?;
    let pct: u8 = pct_str.parse().unwrap_or(0).min(100);
    let progress = match state {
        0 => ProgressState::Remove,
        1 => ProgressState::Set(pct),
        2 => ProgressState::Error(pct),
        3 => ProgressState::Indeterminate,
        _ => return None,
    };
    Some(TapEvent::Progress(progress))
}

/// Parse an OSC 1337 File= body (iTerm2 inline image protocol) into a decoded
/// image. The format is:
///
/// `1337;File=<key>=<value>;<key>=<value>...:<base64 image bytes>`
///
/// The args (name, size, width, height, inline, preserveAspectRatio, …) precede a
/// colon; everything after the colon is the base64-encoded image file. We decode
/// the base64 then decode the image (PNG only — glassy ships no JPEG/GIF decoder),
/// returning the RGBA pixels. Display sizing (`width=`/`height=`) is currently
/// ignored: the image is placed at native pixels at the cursor, like sixel. Returns
/// `None` for any non-File= OSC 1337, a missing payload, or an undecodable image.
///
/// LIMITATION: the OSC observation buffer is capped at [`TAP_BUF_CAP`] (1 MiB), so
/// a base64 payload larger than that is truncated and will fail to decode. This
/// covers typical `imgcat` icons/screenshots; very large inline images are not yet
/// supported (kitty/sixel remain the path for those).
pub(crate) fn parse_osc1337_file(body: &[u8]) -> Option<DecodedImage> {
    // The args region is ASCII; the base64 payload is ASCII too. Find the prefix.
    let body = std::str::from_utf8(body).ok()?;
    let rest = body.strip_prefix("1337;File=")?;
    // Split the key=value args from the base64 payload at the first ':'.
    let (_args, payload_b64) = rest.split_once(':')?;
    let bytes = b64_decode(payload_b64.trim());
    if bytes.is_empty() {
        return None;
    }
    // Only PNG is decodable here. iTerm2's `imgcat` emits PNG for most captures;
    // other formats pass through without an image (no panic, no event).
    decode_png(&bytes)
}

/// Parse an OSC 1337 body (`1337;Peek=<path>`) into a [`TapEvent::Peek`]. This is
/// a glassy extension to the iTerm2 OSC 1337 namespace (e.g. emitted by a
/// `glassy-peek <file>` helper) that asks the terminal to show a small inline
/// preview of `<path>` near the cursor. The path is taken verbatim (trimmed);
/// resolution + reading happens on the UI thread. Returns `None` for any other
/// OSC 1337 key or an empty path.
pub(crate) fn parse_osc1337_peek(body: &[u8]) -> Option<TapEvent> {
    let body = std::str::from_utf8(body).ok()?;
    let rest = body.strip_prefix("1337;")?;
    let path = rest.strip_prefix("Peek=")?.trim();
    if path.is_empty() {
        return None;
    }
    Some(TapEvent::Peek(std::path::PathBuf::from(path)))
}

/// Parse an OSC 133 body (`133;<mark>`) into a shell-integration semantic mark
/// plus an optional exit code.
///
/// Valid marks are `A` (prompt start), `B` (command start), `C` (command
/// executed), `D` (command finished). For a `D` mark, an exit code may follow
/// as `133;D;<exit>` (e.g. `133;D;0` success, `133;D;1` failure, `133;D;130`
/// SIGINT); the exit field is parsed into the returned `Option<i32>`. iTerm2's
/// `aid=` / key=value params (e.g. `133;D;1;aid=123`) are tolerated — only the
/// first field after `D` is read as the exit code, and it is ignored if it is
/// not a bare integer.
///
/// Returns `(mark, exit_code)`, or `None` if not a valid OSC 133 sequence. The
/// exit code is `None` for A/B/C and for a bare `D` with no numeric exit field.
pub(crate) fn parse_osc133_mark(body: &[u8]) -> Option<(char, Option<i32>)> {
    let body = std::str::from_utf8(body).ok()?;
    let rest = body.strip_prefix("133;")?;
    // The mark is the first character; optional params follow after ';'.
    let mark = rest.chars().next()?;
    if !matches!(mark, 'A' | 'B' | 'C' | 'D') {
        return None;
    }
    // For D, read the first field after the mark as the exit code (if numeric).
    let exit = if mark == 'D' {
        rest.strip_prefix("D;")
            .map(|p| p.split(';').next().unwrap_or(""))
            .and_then(|f| f.trim().parse::<i32>().ok())
    } else {
        None
    };
    Some((mark, exit))
}

/// Whether an OSC 7 `file://` authority refers to the local machine: empty,
/// `localhost`, or the current hostname (case-insensitive). Remote hosts are
/// rejected so a cwd from another machine is never used to spawn a local shell.
///
/// Hostname comes from the kernel via `uname(2)` (`nodename`) — portable across
/// Linux/macOS/BSD and never from `$HOSTNAME`, a user-controlled environment
/// variable a malicious program in the terminal could spoof.
fn host_is_local(host: &str) -> bool {
    if host.is_empty() || host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let uname = rustix::system::uname();
    let Ok(node) = uname.nodename().to_str() else {
        return false;
    };
    // Match either the full hostname or its short (pre-dot) form.
    node.eq_ignore_ascii_case(host)
        || node
            .split('.')
            .next()
            .is_some_and(|s| s.eq_ignore_ascii_case(host))
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
