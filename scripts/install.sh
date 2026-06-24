#!/usr/bin/env bash
#
# glassy installer — downloads the latest pre-built binary from GitHub Releases,
# verifies its SHA-256 checksum, and installs it to ~/.local/bin (or
# /usr/local/bin as a fallback when ~/.local/bin is not writable / on PATH).
#
# Usage (the canonical one-liner):
#   curl -fsSL https://raw.githubusercontent.com/alliecatowo/glassy/main/scripts/install.sh | bash
#
# Environment overrides:
#   INSTALL_DIR   — absolute path for the binary (default: auto-detect)
#   GLASSY_TAG    — specific release tag, e.g. v0.2.0 (default: latest)
#   NO_MODIFY_PATH=1 — skip the PATH export hint
#
set -euo pipefail

REPO="alliecatowo/glassy"
BINARY_NAME="glassy"

# ---------------------------------------------------------------------------
# helpers
# ---------------------------------------------------------------------------
info()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
ok()    { printf '\033[1;32m  ok\033[0m %s\n' "$*"; }
die()   { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

need() {
    command -v "$1" >/dev/null 2>&1 || die "required tool not found: $1 — please install it and retry"
}

# ---------------------------------------------------------------------------
# OS / arch detection
# ---------------------------------------------------------------------------
detect_target() {
    local os arch
    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Linux)
            case "$arch" in
                x86_64)  echo "x86_64-linux" ;;
                aarch64|arm64) echo "aarch64-linux" ;;
                *) die "unsupported architecture: $arch (only x86_64 and aarch64 are supported on Linux)" ;;
            esac
            ;;
        Darwin)
            case "$arch" in
                x86_64)  echo "x86_64-macos" ;;
                arm64)   echo "aarch64-macos" ;;
                *) die "unsupported architecture: $arch" ;;
            esac
            ;;
        *) die "unsupported OS: $os (Windows users: see README for the MSI installer)" ;;
    esac
}

# ---------------------------------------------------------------------------
# checksum verification
# ---------------------------------------------------------------------------
verify_sha256() {
    local file="$1" expected="$2"
    local actual
    if command -v sha256sum >/dev/null 2>&1; then
        actual="$(sha256sum "$file" | awk '{print $1}')"
    elif command -v shasum >/dev/null 2>&1; then
        actual="$(shasum -a 256 "$file" | awk '{print $1}')"
    else
        # No checksum tool available — warn but continue.
        printf '\033[1;33mwarn:\033[0m sha256sum / shasum not found; skipping checksum verification\n' >&2
        return 0
    fi
    [ "$actual" = "$expected" ] || die "checksum mismatch!\n  expected: $expected\n  got:      $actual\nAbort."
}

# ---------------------------------------------------------------------------
# resolve install dir
# ---------------------------------------------------------------------------
resolve_install_dir() {
    if [ -n "${INSTALL_DIR:-}" ]; then
        echo "$INSTALL_DIR"
        return
    fi

    local user_local="$HOME/.local/bin"
    mkdir -p "$user_local" 2>/dev/null || true

    # Prefer ~/.local/bin if it's writable and (already or soon) on PATH.
    if [ -w "$user_local" ]; then
        echo "$user_local"
    elif [ -w "/usr/local/bin" ]; then
        echo "/usr/local/bin"
    else
        # Fall back to ~/.local/bin anyway; user may need to sudo separately.
        echo "$user_local"
    fi
}

# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------
main() {
    need curl
    need mktemp

    local target
    target="$(detect_target)"
    info "Detected target: $target"

    # Resolve the release tag.
    local tag="${GLASSY_TAG:-}"
    if [ -z "$tag" ]; then
        info "Fetching latest release tag from GitHub…"
        tag="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
            | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')"
        [ -n "$tag" ] || die "could not determine latest release tag — set GLASSY_TAG manually"
    fi
    info "Installing glassy $tag"

    local base_url="https://github.com/$REPO/releases/download/$tag"
    local asset="${BINARY_NAME}-${target}"
    local sums_url="${base_url}/SHA256SUMS"
    local bin_url="${base_url}/${asset}"

    # Download to a temp dir.
    local tmpdir
    tmpdir="$(mktemp -d)"
    trap 'rm -rf "$tmpdir"' EXIT

    info "Downloading binary: $bin_url"
    curl -fSL --progress-bar -o "$tmpdir/$asset" "$bin_url" \
        || die "download failed — does release $tag exist? Check: https://github.com/$REPO/releases"

    # Verify checksum if SHA256SUMS is present.
    info "Downloading SHA256SUMS…"
    if curl -fsSL -o "$tmpdir/SHA256SUMS" "$sums_url" 2>/dev/null; then
        local expected
        expected="$(grep " ${asset}$" "$tmpdir/SHA256SUMS" | awk '{print $1}')"
        if [ -n "$expected" ]; then
            info "Verifying checksum…"
            verify_sha256 "$tmpdir/$asset" "$expected"
            ok "checksum verified"
        else
            printf '\033[1;33mwarn:\033[0m %s not found in SHA256SUMS; skipping verification\n' "$asset" >&2
        fi
    else
        printf '\033[1;33mwarn:\033[0m SHA256SUMS not available for this release; skipping verification\n' >&2
    fi

    # Install.
    local install_dir
    install_dir="$(resolve_install_dir)"
    mkdir -p "$install_dir"
    chmod 755 "$tmpdir/$asset"
    cp "$tmpdir/$asset" "$install_dir/$BINARY_NAME"
    ok "installed to $install_dir/$BINARY_NAME"

    # PATH hint.
    if [ -z "${NO_MODIFY_PATH:-}" ]; then
        if ! printf '%s' ":${PATH}:" | grep -q ":${install_dir}:"; then
            printf '\n\033[1;33mnote:\033[0m %s is not on your PATH.\n' "$install_dir"
            printf '      Add it with:\n\n'
            printf '        export PATH="%s:$PATH"\n\n' "$install_dir"
            printf '      Then add that line to ~/.bashrc or ~/.zshrc to make it permanent.\n\n'
        fi
    fi

    info "Done! Run: glassy"
}

main "$@"
