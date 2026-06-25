//! Platform detection for platform-aware keybindings + chord display.
//!
//! macOS uses ⌘-based chords (Cmd+C/V/T/W, Cmd+1-9, Cmd+comma, Cmd+F) and the
//! HIG symbol run (⌃⌥⇧⌘ printed together with no `+`), while Linux/Windows use
//! the familiar Ctrl / Ctrl+Shift chords with `+`-joined labels.
//!
//! [`Platform::current`] is resolved at compile time via `cfg(target_os)` and so
//! costs nothing at runtime; tests may construct the variants directly to
//! exercise the macOS path on a Linux build.

/// The host platform, used to pick default keybindings and chord rendering.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Platform {
    /// macOS: ⌘-based chords, HIG modifier symbols.
    Mac,
    /// Linux / BSD: Ctrl / Ctrl+Shift chords.
    #[default]
    Linux,
    /// Windows: Ctrl / Ctrl+Shift chords (same as Linux for our purposes).
    Windows,
}

impl Platform {
    /// The platform this binary was compiled for.
    pub const fn current() -> Self {
        #[cfg(target_os = "macos")]
        {
            Platform::Mac
        }
        #[cfg(target_os = "windows")]
        {
            Platform::Windows
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            Platform::Linux
        }
    }

    /// True on macOS, where the primary modifier is ⌘ (Super/Meta) rather than
    /// Ctrl, and chords render as the HIG symbol run.
    pub const fn is_mac(self) -> bool {
        matches!(self, Platform::Mac)
    }

    /// Platform to use for *display* (the help panel's chord rendering),
    /// honoring the `GLASSY_HELP_PLATFORM` headless capture override so the
    /// macOS HIG symbol run can be screenshotted on a Linux build:
    ///
    /// - `GLASSY_HELP_PLATFORM=mac` → [`Platform::Mac`]
    /// - `=linux` / `=windows` → that platform
    /// - unset / unrecognized → [`Platform::current`]
    ///
    /// Only the rendered labels change; the live keymap (resolved at startup
    /// from the real platform) is unaffected, so this never alters behavior.
    pub fn display_override() -> Self {
        match std::env::var("GLASSY_HELP_PLATFORM")
            .ok()
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("mac" | "macos" | "darwin") => Platform::Mac,
            Some("windows" | "win") => Platform::Windows,
            Some("linux" | "bsd") => Platform::Linux,
            _ => Platform::current(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_matches_cfg() {
        let p = Platform::current();
        #[cfg(target_os = "macos")]
        assert_eq!(p, Platform::Mac);
        #[cfg(target_os = "windows")]
        assert_eq!(p, Platform::Windows);
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        assert_eq!(p, Platform::Linux);
    }

    #[test]
    fn is_mac_only_true_for_mac() {
        assert!(Platform::Mac.is_mac());
        assert!(!Platform::Linux.is_mac());
        assert!(!Platform::Windows.is_mac());
    }

    #[test]
    fn default_is_linux() {
        assert_eq!(Platform::default(), Platform::Linux);
    }
}
