//! Leader / multi-key chord *sequences* (e.g. `ctrl+a` then `n`).
//!
//! A small pending-prefix state machine layered on top of the flat keymap. The
//! user binds a sequence in `[keybindings]` by separating chords with a space
//! (`"ctrl+a n" = next_tab`); these land in `config.key_sequences` (a
//! [`crate::config::SequenceMap`]). When a pressed chord is the first chord of
//! one or more bound sequences AND is not already a flat-keymap action, glassy
//! arms a pending prefix; subsequent chords either complete a sequence (fire the
//! action), extend the prefix (another leader hop), or cancel it (no match).
//!
//! Idle-safe: the pending state holds a deadline, but it is only consulted on the
//! next keypress — no timer wakeups are scheduled, so a forgotten leader never
//! violates the 0%-idle-at-rest invariant. It simply lapses on the next key.

use super::*;
use crate::config::{Chord, KeyAction};
use std::time::{Duration, Instant};

/// How long an armed leader prefix stays live before the next chord is treated as
/// a fresh keypress instead of a sequence continuation. Generous enough for a
/// deliberate two-step bind, short enough that a stale prefix never lingers.
pub(crate) const SEQ_TIMEOUT: Duration = Duration::from_millis(1500);

/// A live leader-sequence prefix: the chords accumulated so far plus the instant
/// the prefix lapses. Stored in `App::key_seq_pending` while a multi-key bind is
/// mid-entry; `None` otherwise.
#[derive(Clone, Debug)]
pub(crate) struct PendingSeq {
    /// Chords typed so far (a prefix of at least one bound sequence).
    pub chords: Vec<Chord>,
    /// Wall-clock deadline after which this prefix is considered abandoned.
    pub deadline: Instant,
}

/// The outcome of feeding one chord to the sequence state machine.
#[derive(PartialEq, Eq, Debug)]
pub(crate) enum SeqStep {
    /// The chord is not part of any sequence (no prefix was armed and the chord
    /// does not start one). The caller continues its normal keymap dispatch.
    NotApplicable,
    /// A sequence prefix is now armed (or was extended). The chord is consumed —
    /// the caller must not dispatch it or forward it to the child.
    Pending,
    /// A full sequence completed. The caller runs `action` and consumes the chord.
    Fire(KeyAction),
    /// An armed prefix was canceled because the chord did not extend it. The
    /// chord is consumed (it only served to abort the leader); the caller should
    /// repaint but not dispatch/forward it. This matches tmux/vim leader feel.
    Canceled,
}

/// Pure transition: given the currently-pending prefix (if any), the new chord,
/// the bound sequence map, and whether the chord already has a *flat* keymap
/// action, decide the next step. Split out from `App` so it is unit-testable
/// without a window/event loop. `now` is injected for deterministic tests.
pub(crate) fn step(
    pending: Option<&PendingSeq>,
    chord: &Chord,
    sequences: &crate::config::SequenceMap,
    has_flat_action: bool,
    now: Instant,
) -> SeqStep {
    if sequences.is_empty() {
        return SeqStep::NotApplicable;
    }
    // An armed, un-lapsed prefix takes priority: try to extend/complete it.
    if let Some(p) = pending
        && now <= p.deadline
    {
        let mut next = p.chords.clone();
        next.push(chord.clone());
        if let Some(&action) = sequences.get(&next) {
            return SeqStep::Fire(action);
        }
        // Still a strict prefix of some longer sequence → keep accumulating.
        if is_prefix(&next, sequences) {
            return SeqStep::Pending;
        }
        // The chord doesn't extend the prefix: abort the leader. (We do NOT also
        // dispatch the chord — a mistyped leader continuation is swallowed, which
        // is the least-surprising behaviour for a "leader, then ???" mistake.)
        return SeqStep::Canceled;
    }

    // No live prefix. A chord that ALREADY has a flat keymap action keeps that
    // action (a single-key bind always wins over arming a leader of the same
    // first chord) so existing binds never regress.
    if has_flat_action {
        return SeqStep::NotApplicable;
    }
    // Arm a new prefix if this chord begins at least one bound sequence.
    let start = [chord.clone()];
    if is_prefix(&start, sequences) {
        SeqStep::Pending
    } else {
        SeqStep::NotApplicable
    }
}

/// True when `prefix` is a strict prefix of at least one key in `sequences`
/// (i.e. some bound sequence starts with exactly these chords and is longer).
fn is_prefix(prefix: &[Chord], sequences: &crate::config::SequenceMap) -> bool {
    sequences
        .keys()
        .any(|seq| seq.len() > prefix.len() && seq.starts_with(prefix))
}

impl App {
    /// Feed one pressed chord to the leader-sequence machine. Returns `true` when
    /// the chord was consumed by the sequence layer (armed, fired, or canceled a
    /// prefix) and the caller must NOT continue normal keymap/child dispatch.
    /// Returns `false` to let the caller proceed exactly as before.
    pub(super) fn handle_key_sequence(
        &mut self,
        chord: &Chord,
        event_loop: &ActiveEventLoop,
    ) -> bool {
        let has_flat = self.config.keymap.contains_key(chord);
        let outcome = step(
            self.key_seq_pending.as_ref(),
            chord,
            &self.config.key_sequences,
            has_flat,
            Instant::now(),
        );
        match outcome {
            SeqStep::NotApplicable => false,
            SeqStep::Pending => {
                let mut chords = self
                    .key_seq_pending
                    .take()
                    .map(|p| p.chords)
                    .unwrap_or_default();
                chords.push(chord.clone());
                self.key_seq_pending = Some(PendingSeq {
                    chords,
                    deadline: Instant::now() + SEQ_TIMEOUT,
                });
                true
            }
            SeqStep::Fire(action) => {
                self.key_seq_pending = None;
                self.run_key_action(action, event_loop);
                true
            }
            SeqStep::Canceled => {
                self.key_seq_pending = None;
                self.mark_dirty(event_loop);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::keymap::parse_chord;

    fn seqmap(binds: &[(&str, KeyAction)]) -> crate::config::SequenceMap {
        let mut m = crate::config::SequenceMap::new();
        for (s, a) in binds {
            let chords: Vec<Chord> = s
                .split_whitespace()
                .map(|c| parse_chord(c).unwrap())
                .collect();
            m.insert(chords, *a);
        }
        m
    }

    #[test]
    fn arms_then_fires_a_two_chord_sequence() {
        let seqs = seqmap(&[("ctrl+a n", KeyAction::NewTab)]);
        let now = Instant::now();
        let leader = parse_chord("ctrl+a").unwrap();
        // Leader with no flat action arms the prefix.
        assert_eq!(step(None, &leader, &seqs, false, now), SeqStep::Pending);
        let pending = PendingSeq {
            chords: vec![leader],
            deadline: now + SEQ_TIMEOUT,
        };
        let n = parse_chord("n").unwrap();
        assert_eq!(
            step(Some(&pending), &n, &seqs, false, now),
            SeqStep::Fire(KeyAction::NewTab)
        );
    }

    #[test]
    fn flat_action_wins_over_arming_a_leader() {
        // A chord that already has a single-key action is never hijacked as a
        // leader, even if a sequence begins with it.
        let seqs = seqmap(&[("ctrl+a n", KeyAction::NewTab)]);
        let leader = parse_chord("ctrl+a").unwrap();
        assert_eq!(
            step(None, &leader, &seqs, true, Instant::now()),
            SeqStep::NotApplicable
        );
    }

    #[test]
    fn wrong_continuation_cancels_the_prefix() {
        let seqs = seqmap(&[("ctrl+a n", KeyAction::NewTab)]);
        let now = Instant::now();
        let pending = PendingSeq {
            chords: vec![parse_chord("ctrl+a").unwrap()],
            deadline: now + SEQ_TIMEOUT,
        };
        let x = parse_chord("x").unwrap();
        assert_eq!(
            step(Some(&pending), &x, &seqs, false, now),
            SeqStep::Canceled
        );
    }

    #[test]
    fn lapsed_prefix_treats_chord_as_fresh() {
        // Past the deadline, an armed prefix is ignored and the chord is judged
        // on its own (here it starts no sequence, so NotApplicable).
        let seqs = seqmap(&[("ctrl+a n", KeyAction::NewTab)]);
        let now = Instant::now();
        let pending = PendingSeq {
            chords: vec![parse_chord("ctrl+a").unwrap()],
            deadline: now - Duration::from_millis(1),
        };
        let n = parse_chord("n").unwrap();
        assert_eq!(
            step(Some(&pending), &n, &seqs, false, now),
            SeqStep::NotApplicable
        );
    }

    #[test]
    fn three_chord_sequence_accumulates() {
        let seqs = seqmap(&[("ctrl+a g g", KeyAction::ScrollTop)]);
        let now = Instant::now();
        let a = parse_chord("ctrl+a").unwrap();
        assert_eq!(step(None, &a, &seqs, false, now), SeqStep::Pending);
        let mut p = PendingSeq {
            chords: vec![a],
            deadline: now + SEQ_TIMEOUT,
        };
        let g = parse_chord("g").unwrap();
        assert_eq!(step(Some(&p), &g, &seqs, false, now), SeqStep::Pending);
        p.chords.push(g.clone());
        assert_eq!(
            step(Some(&p), &g, &seqs, false, now),
            SeqStep::Fire(KeyAction::ScrollTop)
        );
    }

    #[test]
    fn empty_sequence_map_is_inert() {
        let seqs = crate::config::SequenceMap::new();
        let a = parse_chord("ctrl+a").unwrap();
        assert_eq!(
            step(None, &a, &seqs, false, Instant::now()),
            SeqStep::NotApplicable
        );
    }
}
