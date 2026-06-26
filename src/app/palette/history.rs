//! Command-history and recent-cwd sources for the command palette.
//!
//! These two ring buffers feed the palette's dynamic rows (alongside the static
//! action/setting/theme registry in the parent module):
//!
//! - `cmd_history`: shell commands captured from OSC 133 `B`..`C` zones (see
//!   [`crate::pty::cmdzone`]), recorded here via [`App::record_command_history`].
//! - `cwd_history`: working directories reported via OSC 7, recorded via
//!   [`App::record_cwd_history`].
//!
//! Recording is a pure side-effect (no redraw); the registry is rebuilt from the
//! rings each time the palette opens. Split out of `palette.rs` to keep that file
//! under the project's 700-line limit.

use super::super::*;

/// Maximum number of recent working directories retained for the palette's cwd
/// source. Independent of the command-history capacity (which is user-configurable
/// via `command_history`); cwds are cheap and few in practice.
pub(crate) const CWD_HISTORY_CAP: usize = 64;

impl App {
    /// Record a shell command captured from an OSC 133 `B`..`C` zone into the
    /// command-history ring. Deduplicates against the most-recent entry (so
    /// hitting Enter on the same command twice does not stack it) and bounds the
    /// ring to `config.command_history`. A no-op when capture is disabled
    /// (capacity 0) or the command is blank. Not a visual change, so no redraw.
    pub(crate) fn record_command_history(&mut self, cmd: String) {
        let cap = self.config.command_history;
        if cap == 0 {
            return;
        }
        let cmd = cmd.trim();
        if cmd.is_empty() {
            return;
        }
        // Skip if identical to the last recorded command.
        if self.cmd_history.back().map(String::as_str) == Some(cmd) {
            return;
        }
        self.cmd_history.push_back(cmd.to_string());
        while self.cmd_history.len() > cap {
            self.cmd_history.pop_front();
        }
    }

    /// Drop oldest command-history entries down to the current capacity. Called
    /// after `config.command_history` shrinks on a live config reload.
    pub(crate) fn trim_command_history(&mut self) {
        let cap = self.config.command_history;
        while self.cmd_history.len() > cap {
            self.cmd_history.pop_front();
        }
    }

    /// Record a working directory reported via OSC 7 into the cwd-history ring.
    /// A directory already present is moved to the back (most-recent wins) rather
    /// than duplicated; the ring is bounded to [`CWD_HISTORY_CAP`]. Not visual.
    pub(crate) fn record_cwd_history(&mut self, dir: std::path::PathBuf) {
        // Drop an existing occurrence so the path floats to most-recent.
        if let Some(pos) = self.cwd_history.iter().position(|p| p == &dir) {
            self.cwd_history.remove(pos);
        }
        self.cwd_history.push_back(dir);
        while self.cwd_history.len() > CWD_HISTORY_CAP {
            self.cwd_history.pop_front();
        }
    }
}

/// Quote a directory path for safe use after `cd ` in a POSIX shell: wrap in
/// single quotes and escape any embedded single quotes (`'` → `'\''`). Paths
/// without shell-special characters are returned unquoted to keep the common
/// case tidy. Pure for unit testing.
pub(crate) fn shell_quote(dir: &str) -> String {
    let needs_quote = dir
        .chars()
        .any(|c| !(c.is_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '~' | '+' | ':')));
    if !needs_quote {
        return dir.to_string();
    }
    let mut out = String::with_capacity(dir.len() + 2);
    out.push('\'');
    for c in dir.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Render a path with the user's home directory collapsed to `~` for display in
/// the palette (`/home/me/src` → `~/src`). Falls back to the full path when the
/// path is not under `$HOME`. Pure-ish (reads `$HOME`); kept here for the cwd
/// source's display labels.
pub(crate) fn compact_home(dir: &std::path::Path) -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let home = std::path::PathBuf::from(home);
        if let Ok(rest) = dir.strip_prefix(&home) {
            if rest.as_os_str().is_empty() {
                return "~".to_string();
            }
            return format!("~/{}", rest.display());
        }
    }
    dir.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_leaves_plain_paths_unquoted() {
        assert_eq!(shell_quote("/home/me/src"), "/home/me/src");
        assert_eq!(shell_quote("~/projects/glassy"), "~/projects/glassy");
        assert_eq!(shell_quote("/a-b_c.d/e+f"), "/a-b_c.d/e+f");
    }

    #[test]
    fn shell_quote_wraps_and_escapes_special_chars() {
        // A space forces quoting.
        assert_eq!(shell_quote("/my dir"), "'/my dir'");
        // An embedded single-quote is escaped as '\''.
        assert_eq!(shell_quote("/a'b"), "'/a'\\''b'");
    }

    #[test]
    fn compact_home_collapses_home_prefix() {
        // Use a synthetic HOME so the test is deterministic.
        // SAFETY: single-threaded test; we restore nothing because cargo runs each
        // test process fresh, but scope the var to avoid surprising siblings.
        unsafe {
            std::env::set_var("HOME", "/home/tester");
        }
        assert_eq!(
            compact_home(std::path::Path::new("/home/tester/src/glassy")),
            "~/src/glassy"
        );
        assert_eq!(compact_home(std::path::Path::new("/home/tester")), "~");
        // Not under HOME → unchanged.
        assert_eq!(
            compact_home(std::path::Path::new("/etc/hosts")),
            "/etc/hosts"
        );
    }
}
