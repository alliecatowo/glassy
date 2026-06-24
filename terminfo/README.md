# Glassy Terminfo Database

This directory contains the custom terminfo definition for glassy, a fast GPU-accelerated terminal emulator.

## File: glassy.terminfo

The `glassy.terminfo` file describes a glassy-256color terminal with support for:

- **Truecolor (24-bit RGB)** — `Tc` capability for 16.7M color support
- **Styled underline** — `Su` capability for underline styles (straight, double, curly, dotted, dashed)
- **Kitty keyboard protocol** — Extended function key support (F13-F20, modifiers)
- **OSC 8 hyperlinks** — Terminal hyperlink support via escape sequences
- **OSC 52 clipboard** — Copy/paste via escape sequences (`Ms` capability)
- **Standard VT100 capabilities** — Cursor movement, clear, SGR (bold, italic, reverse, etc.)
- **Mouse reporting** — Kitty and SGR mouse protocol support
- **Scrollback region** — Scroll history management

## Installation

### System-wide Installation (Linux)

```bash
# Compile the terminfo and install to /usr/share/terminfo
sudo tic -x terminfo/glassy.terminfo

# Verify installation
infocmp glassy | head
```

### User Installation

```bash
# Install to ~/.terminfo (takes precedence over system-wide)
tic -x terminfo/glassy.terminfo

# Verify installation
infocmp glassy | head
```

### Package Installation

The glassy binary packages (AUR, Debian) automatically install this terminfo during package setup:

- **AUR (Arch Linux)**: Runs `tic` in post-install hook
- **Debian (.deb)**: Runs post-install script to compile and install terminfo
- **Homebrew (macOS)**: Can be added to formula

## Usage

When glassy runs, it automatically:

1. Checks if `glassy-256color` terminfo is available
2. Sets `TERM=glassy-256color` if available
3. Falls back to `TERM=xterm-256color` if not found
4. Always sets `COLORTERM=truecolor` for 24-bit color support

Applications running in glassy can detect the terminal via:

- `$TERM` environment variable (will be `glassy-256color` or `xterm-256color`)
- `$TERM_PROGRAM` environment variable (always set to `glassy`)
- `$GLASSY_WINDOW_ID` environment variable (indicates running under glassy)

## Terminfo Lookup

The terminfo database is searched in this order:

1. `$TERMINFO` (environment override)
2. `$HOME/.terminfo` (user personal database)
3. `/usr/share/terminfo` (system database, Linux/BSD)
4. `/lib/terminfo` (alternative system location)
5. `/etc/terminfo` (another system location)

If none of these contain `glassy` or `glassy-256color`, glassy falls back to `xterm-256color`.

## Validation

To validate the terminfo definition:

```bash
# After installation, check capabilities
infocmp glassy

# Compare with xterm-256color
infocmp -d glassy xterm-256color
```

## Building from Source

If you build glassy from source:

```bash
# Build the binary
cargo build --release

# Install terminfo (one-time)
tic -x terminfo/glassy.terminfo

# The binary automatically uses glassy-256color if available
./target/release/glassy
```

## Development

To modify the terminfo:

1. Edit `glassy.terminfo`
2. Rebuild and reinstall: `tic -x glassy.terminfo`
3. Restart glassy or any running shells for the new terminfo to take effect

## References

- [Terminfo Manual](https://man.archlinux.org/man/terminfo.5)
- [VT100/ANSI Escape Codes](https://en.wikipedia.org/wiki/ANSI_escape_code)
- [Xterm Control Sequences](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html)
- [Kitty Keyboard Protocol](https://sw.kovidgoyal.net/kitty/keyboard-protocol/)
