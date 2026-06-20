//! Terminal bell feedback: a brief window flash (visual) and a short soft beep
//! (audible). Both are optional and configurable; see [`crate::app::Config`].
//!
//! The visual bell is driven entirely by `app.rs` (a timed flash overlay in the
//! renderer), so this module is concerned only with the audible bell.
//!
//! The audible backend (rodio/cpal) is behind the `bell-audio` Cargo feature so
//! the default build needs no system audio dev libraries. When the feature is not
//! compiled in, [`AudioBell`] degrades to a no-op (logged once). Either way a
//! missing or broken audio device never panics — it is logged at debug level and
//! the bell is silently dropped.

/// How long the visual-bell flash stays up before the window restores. ~80ms is
/// long enough to register as a blink without being distracting.
pub const FLASH_MS: u64 = 80;

/// Visual-bell flash overlay strength: a low-alpha tint blended over the whole
/// window toward the foreground color. Restrained on purpose.
pub const FLASH_ALPHA: f32 = 0.18;

/// Plays a short, soft beep on demand. Holds the audio output device open lazily
/// (first ring) so repeated bells don't re-acquire it, and so its absence never
/// costs anything until the first bell.
#[derive(Default)]
pub struct AudioBell {
    #[cfg(feature = "bell-audio")]
    sink: Option<rodio::MixerDeviceSink>,
    /// Set once the device proved unavailable, so we don't retry (and re-log) on
    /// every subsequent bell.
    unavailable: bool,
}

impl AudioBell {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ring the audible bell: a brief, soft sine tone. Never panics; a missing or
    /// failing audio device is logged once and the bell is dropped.
    #[cfg(feature = "bell-audio")]
    pub fn ring(&mut self) {
        use std::time::Duration;

        use rodio::Source;
        use rodio::source::SineWave;

        if self.unavailable {
            return;
        }
        if self.sink.is_none() {
            match rodio::DeviceSinkBuilder::open_default_sink() {
                Ok(sink) => self.sink = Some(sink),
                Err(e) => {
                    log::debug!("audible bell: no audio output ({e}); disabling");
                    self.unavailable = true;
                    return;
                }
            }
        }
        let Some(sink) = self.sink.as_ref() else {
            return;
        };

        // A short, gentle beep: a mid tone, quiet, with a quick fade-in to avoid a
        // click on attack. The mixer plays it asynchronously and the source ends
        // itself after the duration, so there is nothing to clean up.
        let beep = SineWave::new(660.0)
            .take_duration(Duration::from_millis(120))
            .amplify(0.12)
            .fade_in(Duration::from_millis(8));
        sink.mixer().add(beep);
    }

    /// No-op fallback when the audio backend is not compiled in.
    #[cfg(not(feature = "bell-audio"))]
    pub fn ring(&mut self) {
        if !self.unavailable {
            log::debug!(
                "audible bell requested but glassy was built without the `bell-audio` feature"
            );
            self.unavailable = true;
        }
    }
}
