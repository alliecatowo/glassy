#!/bin/bash
# Post-install script for glassy: compile and install terminfo database.
# This runs after the .deb is unpacked.

set -e

# Compile the terminfo entry. tic is part of ncurses-bin (essential package).
# We install to /usr/share/terminfo for system-wide availability.
if command -v tic &> /dev/null; then
    tic -x -o /usr/share/terminfo /usr/share/doc/glassy/terminfo/glassy.terminfo 2>/dev/null || true
fi

exit 0
