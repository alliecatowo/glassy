# Makefile for glassy -- a fast GPU terminal emulator.
#
# This Makefile is intentionally POSIX-make compatible (no GNU-only
# constructs such as $(wildcard ...), pattern rules with %, or ifeq).
# It drives the usual cargo workflow plus a user-local install/uninstall
# flow that follows the XDG / freedesktop directory layout.
#
# Common usage:
#   make build              # cargo build --release
#   make install            # install into $(PREFIX) (default: ~/.local)
#   make uninstall          # remove what `install` placed
#   make install PREFIX=/usr DESTDIR=/tmp/pkg   # staged / system install
#
# DESTDIR is honored for packaging (staged installs); PREFIX controls the
# logical install prefix and defaults to a user-local location so that no
# elevated privileges are required.

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

# Cargo binary (override with `make CARGO=...` if needed).
CARGO       = cargo

# Name of the produced binary.
BIN         = glassy

# Install prefix. User-local by default so `make install` needs no sudo.
PREFIX      = $(HOME)/.local

# DESTDIR supports staged installs for packaging; empty by default.
DESTDIR     =

# Derived install directories (freedesktop / XDG layout).
BINDIR      = $(DESTDIR)$(PREFIX)/bin
DATADIR     = $(DESTDIR)$(PREFIX)/share
ICONDIR     = $(DATADIR)/icons/hicolor
APPDIR      = $(DATADIR)/applications

# Where the binary expects the bundled color-emoji font at runtime.
# The binary loads ~/.local/share/glassy/fonts/NotoColorEmoji.ttf, so we
# install relative to $(PREFIX)/share rather than under a system prefix.
FONTDIR     = $(DESTDIR)$(PREFIX)/share/glassy/fonts

# Build artifact and source asset locations.
RELEASE_BIN = target/release/$(BIN)
ICON_SRC    = assets/icons
FONT_SRC    = assets/fonts/NotoColorEmoji-CBDT.ttf
DESKTOP_SRC = extra/glassy.desktop

# Icon sizes available under $(ICON_SRC) as glassy-<size>.png.
# The 512 icon is installed into the 512x512 directory.
ICON_SIZES  = 16 32 48 64 128 256 512

# Tools.
INSTALL     = install

.POSIX:

.PHONY: all build run test fmt clippy install uninstall clean

# ---------------------------------------------------------------------------
# Development targets
# ---------------------------------------------------------------------------

all: build

# Build an optimized release binary.
build:
	$(CARGO) build --release

# Run the release binary via cargo.
run:
	$(CARGO) run --release

# Run the test suite.
test:
	$(CARGO) test

# Format all sources.
fmt:
	$(CARGO) fmt

# Lint with clippy, treating warnings as errors.
clippy:
	$(CARGO) clippy --all-targets -- -D warnings

# ---------------------------------------------------------------------------
# Install / uninstall
# ---------------------------------------------------------------------------

# Install the binary, icons, desktop entry, and (if present) the bundled
# color-emoji font. Honors DESTDIR/PREFIX. No sudo required for the default
# user-local PREFIX.
install:
	# Binary.
	$(INSTALL) -d "$(BINDIR)"
	$(INSTALL) -m 0755 "$(RELEASE_BIN)" "$(BINDIR)/$(BIN)"
	# Icons: install each glassy-<size>.png as
	# hicolor/<size>x<size>/apps/glassy.png.
	for sz in $(ICON_SIZES); do \
		dir="$(ICONDIR)/$${sz}x$${sz}/apps"; \
		$(INSTALL) -d "$$dir"; \
		$(INSTALL) -m 0644 "$(ICON_SRC)/$(BIN)-$${sz}.png" "$$dir/$(BIN).png"; \
	done
	# Desktop entry.
	$(INSTALL) -d "$(APPDIR)"
	$(INSTALL) -m 0644 "$(DESKTOP_SRC)" "$(APPDIR)/$(BIN).desktop"
	# Bundled color-emoji font (optional). The binary loads it from
	# ~/.local/share/glassy/fonts/NotoColorEmoji.ttf at runtime.
	if [ -f "$(FONT_SRC)" ]; then \
		$(INSTALL) -d "$(FONTDIR)"; \
		$(INSTALL) -m 0644 "$(FONT_SRC)" "$(FONTDIR)/NotoColorEmoji.ttf"; \
		echo "Installed color-emoji font -> $(FONTDIR)/NotoColorEmoji.ttf"; \
	else \
		echo "NOTE: $(FONT_SRC) not found; skipping color-emoji font."; \
		echo "      For emoji rendering, place a NotoColorEmoji.ttf at"; \
		echo "      $(PREFIX)/share/glassy/fonts/NotoColorEmoji.ttf"; \
	fi
	@echo "glassy installed under $(DESTDIR)$(PREFIX)"
	@echo "Ensure $(PREFIX)/bin is on your PATH."

# Remove everything `install` placed.
uninstall:
	rm -f "$(BINDIR)/$(BIN)"
	for sz in $(ICON_SIZES); do \
		rm -f "$(ICONDIR)/$${sz}x$${sz}/apps/$(BIN).png"; \
	done
	rm -f "$(APPDIR)/$(BIN).desktop"
	rm -f "$(FONTDIR)/NotoColorEmoji.ttf"
	@echo "glassy uninstalled from $(DESTDIR)$(PREFIX)"

# ---------------------------------------------------------------------------
# Cleanup
# ---------------------------------------------------------------------------

clean:
	$(CARGO) clean
