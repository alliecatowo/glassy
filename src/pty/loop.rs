//! PTY read/parse loop: owns the PTY fd, waits on it with polling, reads bytes,
//! taps OSC/image sequences, and feeds the rest to the VT parser.

use std::io::{Read, Write};
use std::num::NonZeroUsize;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event, EventListener, OnResize};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::Term;
use alacritty_terminal::tty::{self, EventedReadWrite};
use alacritty_terminal::vte::ansi::Processor;
use polling::{Event as PollEvent, Events, PollMode, Poller};

use super::scan;
use crate::image::ImageStore;
use crate::pty::{EventProxy, LoopMsg, PromptTracker};

/// Maximum time to hold a synchronized-output buffer before forcibly waking the
/// UI. 16 ms gives ~1 frame at 60 Hz; applications should close the bracket well
/// within this, but we never stall longer.
const SYNC_TIMEOUT: Duration = Duration::from_millis(16);

/// PTY read/parse loop: waits on the master fd, drains control messages, reads
/// available bytes, taps image/OSC sequences, and feeds the rest to the VT parser.
///
/// New VT features handled here (beyond alacritty_terminal's own parser):
///
/// **modifyOtherKeys** (XTMODKEYS, `CSI > 4 ; N m`): alacritty_terminal 0.26
/// does not implement `set_modify_other_keys`, so we scan VT byte runs for the
/// sequence and forward a `UserEvent::ModifyOtherKeys` to the UI thread, which
/// updates `App::modify_other_keys`; `encode_key` then uses that state.
///
/// **Synchronized Output** (DECSET 2026, `CSI ? 2026 h/l`): when an
/// application opens a synchronized update (`?2026h`) we hold the UI wakeup
/// until the matching `?2026l` end marker or at most `SYNC_TIMEOUT` (16 ms),
/// whichever comes first. This avoids waking the renderer mid-frame and tearing
/// complex full-screen redraws. The VT parser still receives all bytes eagerly
/// (so terminal state stays up to date) — we only delay the `Wakeup` event.
pub fn run_loop(
    mut pty: tty::Pty,
    term: Arc<FairMutex<Term<EventProxy>>>,
    proxy: EventProxy,
    rx: Receiver<LoopMsg>,
    poller: Arc<Poller>,
    images: Arc<FairMutex<ImageStore>>,
    prompts: Arc<Mutex<PromptTracker>>,
) {
    const PTY_KEY: usize = 1;
    let mut processor: Processor = Processor::new();
    let mut tap = crate::image::StreamTap::new();

    let fd = pty.reader().as_raw_fd();
    // alacritty opens the master non-blocking; since we only read after a poll
    // readiness event (so reads never block) and want writes to never drop input
    // on EAGAIN, switch the fd to blocking mode for reliable write_all.
    let _ = rustix::io::ioctl_fionbio(unsafe { BorrowedFd::borrow_raw(fd) }, false);
    let mode = if poller.supports_level() {
        PollMode::Level
    } else {
        PollMode::Oneshot
    };
    // SAFETY: `fd` is owned by `pty`, which this thread owns for the whole loop;
    // we delete it from the poller before `pty` is dropped at return.
    if unsafe { poller.add_with_mode(fd, PollEvent::readable(PTY_KEY), mode) }.is_err() {
        return;
    }

    let mut events = Events::with_capacity(NonZeroUsize::new(64).unwrap());
    let mut buf = vec![0u8; 65536];
    // Set true only on the paths that actually mean the child is gone (EOF / read
    // error). A transient poller error or a UI-initiated shutdown must NOT report
    // a child exit, which would wrongly close the session.
    let mut child_exited = false;

    // ---- Synchronized output state (DECSET 2026) --------------------------------
    // `sync_depth > 0` means the application has opened a synchronized-output
    // bracket (`?2026h`) without a matching close (`?2026l`). We suppress the UI
    // Wakeup while depth > 0, emitting it only when the bracket closes or when
    // `sync_deadline` elapses (hard cap to avoid stalling the UI indefinitely).
    let mut sync_depth: u32 = 0;
    let mut sync_deadline: Option<Instant> = None;
    // Tracks whether any terminal data was processed in the current sync bracket
    // (so we know whether to send a Wakeup when the bracket closes).
    let mut sync_pending_wakeup = false;
    // ---- End sync state ---------------------------------------------------------

    'main: loop {
        // Compute the poll timeout: while inside a sync bracket, wake at the
        // deadline so we never stall the UI longer than SYNC_TIMEOUT even if
        // `?2026l` never arrives.
        let timeout = if sync_depth > 0 {
            sync_deadline.map(|d| d.saturating_duration_since(Instant::now()))
        } else {
            None // block until ready
        };

        events.clear();
        if poller.wait(&mut events, timeout).is_err() {
            break;
        }

        // Check for sync timeout expiry: if the deadline passed and we're still
        // inside a bracket, force-close it and wake the UI.
        if sync_depth > 0 && sync_deadline.is_some_and(|d| Instant::now() >= d) {
            sync_depth = 0;
            sync_deadline = None;
            if sync_pending_wakeup {
                sync_pending_wakeup = false;
                EventListener::send_event(&proxy, Event::Wakeup);
            }
        }

        // Drain control messages (input/resize/shutdown).
        loop {
            match rx.try_recv() {
                Ok(LoopMsg::Input(b)) => {
                    let _ = pty.writer().write_all(&b);
                }
                Ok(LoopMsg::Resize(ws)) => pty.on_resize(ws),
                Ok(LoopMsg::Shutdown) => {
                    child_exited = false;
                    break 'main;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    child_exited = false;
                    break 'main;
                }
            }
        }

        // Read pending output if the fd signalled readable.
        if events.iter().any(|ev| ev.key == PTY_KEY) {
            match pty.reader().read(&mut buf) {
                Ok(0) => {
                    child_exited = true; // EOF: child gone
                    break 'main;
                }
                Ok(n) => {
                    // Tap inline-image (kitty graphics) sequences out of the
                    // stream, yielding VT byte runs interleaved with image display
                    // points. Advance the parser for each run, then anchor each
                    // image at the cursor cell it occupies at that point.
                    let tap_events = tap.process(&buf[..n], &images);
                    let mut term = term.lock();
                    let mut did_process = false;

                    for ev in tap_events {
                        match ev {
                            crate::image::TapEvent::Vt(bytes) => {
                                // ---- modifyOtherKeys interception ----------------
                                // Scan for `CSI > 4 ; N m` before feeding to the
                                // alacritty parser (which ignores the sequence).
                                // Forward the level to the UI thread so encode_key
                                // can emit the CSI 27 ; mods ; code ~ form.
                                if let Some(level) = scan::scan_modify_other_keys(&bytes) {
                                    proxy.send_user(crate::pty::UserEvent::ModifyOtherKeys(
                                        proxy.id, level,
                                    ));
                                }

                                // ---- Synchronized output interception ------------
                                // Scan for ?2026h (begin) and ?2026l (end) before
                                // feeding to the parser. The bytes still go to
                                // alacritty (which handles DECSET/DECRST for its own
                                // purposes; 2026 is unknown to it and ignored).
                                let (begin_count, end_count) = scan::scan_sync_2026(&bytes);
                                if begin_count > 0 {
                                    if sync_depth == 0 {
                                        // Arm the deadline on the first open.
                                        sync_deadline = Some(Instant::now() + SYNC_TIMEOUT);
                                    }
                                    sync_depth = sync_depth.saturating_add(begin_count);
                                }
                                if end_count > 0 {
                                    sync_depth = sync_depth.saturating_sub(end_count);
                                    if sync_depth == 0 {
                                        sync_deadline = None;
                                    }
                                }

                                // A full screen erase (CSI 2J / 3J) or terminal
                                // reset (RIS, ESC c) wipes the content images sit
                                // on, so drop placements at that point in the
                                // stream (ordered: images later in this read
                                // survive, since the tap split them into their own
                                // Display events after this Vt run).
                                if scan::clears_screen(&bytes) {
                                    images.lock().delete(0);
                                }
                                processor.advance(&mut *term, &bytes);
                                did_process = true;
                            }
                            crate::image::TapEvent::Display(p) => {
                                let (row, col) = {
                                    let c = term.renderable_content();
                                    (
                                        c.cursor.point.line.0 + c.display_offset as i32,
                                        c.cursor.point.column.0,
                                    )
                                };
                                images.lock().place(p.id, row, col, p.cols, p.rows);
                                did_process = true;
                            }
                            crate::image::TapEvent::Delete(id) => {
                                images.lock().delete(id);
                                did_process = true;
                            }
                            crate::image::TapEvent::Cwd(path) => {
                                // OSC 7: surface the shell's cwd to the UI thread so
                                // new tabs/splits of this session can inherit it.
                                proxy.send_user(crate::pty::UserEvent::Cwd(proxy.id, path));
                            }
                            crate::image::TapEvent::SemanticMark(mark) => {
                                // OSC 133: record prompt-start rows (mark 'A') in the
                                // shared PromptTracker so the UI can jump to them via
                                // Shift+Up/Down. Other marks (B/C/D) are forwarded to
                                // the UI for potential future use (e.g. command timing).
                                if mark == 'A' {
                                    // Capture the current cursor row as an absolute
                                    // grid offset (display_offset + cursor.line). `term`
                                    // is already locked (the MutexGuard from above), so
                                    // read through the guard directly.
                                    let row = {
                                        let c = term.renderable_content();
                                        c.cursor.point.line.0 + c.display_offset as i32
                                    };
                                    if let Ok(mut p) = prompts.lock() {
                                        p.push(row);
                                    }
                                }
                                proxy
                                    .send_user(crate::pty::UserEvent::SemanticMark(proxy.id, mark));
                            }
                            crate::image::TapEvent::Notification(text) => {
                                // OSC 9 / OSC 777: forward to the UI thread so it can
                                // fire a native desktop notification when unfocused.
                                proxy
                                    .send_user(crate::pty::UserEvent::Notification(proxy.id, text));
                            }
                            crate::image::TapEvent::Progress(state) => {
                                // OSC 9;4: progress report — forward to the UI thread
                                // so it can render the progress indicator in the status
                                // bar / tab chip.
                                proxy.send_user(crate::pty::UserEvent::Progress(proxy.id, state));
                            }
                        }
                    }
                    drop(term);

                    // Wake the UI only when we are outside a synchronized-output
                    // bracket. Inside the bracket, set `sync_pending_wakeup` so the
                    // wakeup is sent when the bracket closes (or times out).
                    if did_process {
                        if sync_depth == 0 {
                            EventListener::send_event(&proxy, Event::Wakeup);
                        } else {
                            sync_pending_wakeup = true;
                        }
                    }
                }
                Err(ref e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => {
                    child_exited = true; // e.g. EIO when the child exits
                    break 'main;
                }
            }
        }

        if mode == PollMode::Oneshot {
            let _ = poller.modify_with_mode(
                unsafe { BorrowedFd::borrow_raw(fd) },
                PollEvent::readable(PTY_KEY),
                PollMode::Oneshot,
            );
        }
    }

    let _ = poller.delete(unsafe { BorrowedFd::borrow_raw(fd) });
    if child_exited {
        EventListener::send_event(&proxy, Event::Exit);
    }
}
