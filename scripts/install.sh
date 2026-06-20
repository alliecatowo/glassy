#!/usr/bin/env bash
#
# Convenience wrapper to build and install glassy.
#
# Builds an optimized release binary and installs it (plus icons, the
# desktop entry, and the bundled color-emoji font if present) into a
# user-local prefix (default: ~/.local).
#
# Override the install prefix:
#   PREFIX=/usr scripts/install.sh        # (may require sudo)
#
set -euo pipefail

# Resolve repo root so the script works from any working directory.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

PREFIX="${PREFIX:-$HOME/.local}"

echo "==> Building and installing glassy (PREFIX=$PREFIX)"
make build install PREFIX="$PREFIX"

echo
echo "==> Done. glassy installed to $PREFIX/bin/glassy"
if ! printf '%s' ":$PATH:" | grep -q ":$PREFIX/bin:"; then
	echo "    Note: $PREFIX/bin is not on your PATH."
	echo "    Add it with: export PATH=\"$PREFIX/bin:\$PATH\""
fi
