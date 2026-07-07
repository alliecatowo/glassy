#!/usr/bin/env bash
#
# glassy macro throughput/startup/RSS harness — vtebench-driven VT100 firehose
# comparison against alacritty and ghostty (when installed), plus hyperfine
# startup timing and a sampled idle-RSS reading for glassy itself.
#
# This machine (and most dev boxes) will not have alacritty/ghostty/hyperfine/
# vtebench installed. That's the *common* case, not an edge case: this script
# checks for each tool up front and reports a clear, actionable message (with
# install instructions) rather than silently skipping or failing deep inside a
# comparison loop.
#
# Usage:
#   scripts/bench.sh                 # run whatever comparisons are possible
#   scripts/bench.sh --require-all   # exit non-zero if any tool is missing
#
# Output: a markdown results table printed to stdout, suitable for pasting
# into docs/benchmarks.md (or appended automatically with --write-docs).
set -euo pipefail

REQUIRE_ALL=0
WRITE_DOCS=0
for arg in "$@"; do
    case "$arg" in
        --require-all) REQUIRE_ALL=1 ;;
        --write-docs) WRITE_DOCS=1 ;;
        *) echo "bench.sh: unknown argument: $arg" >&2; exit 2 ;;
    esac
done

info()  { printf '\033[1;34m==>\033[0m %s\n' "$*" >&2; }
warn()  { printf '\033[1;33mwarn:\033[0m %s\n' "$*" >&2; }
die()   { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# ---------------------------------------------------------------------------
# 1. Tool discovery — check, don't assume. Print install hints for anything
#    missing; only exit non-zero on a missing tool when --require-all is set.
# ---------------------------------------------------------------------------
missing=()

have_glassy_release() { [ -x "$REPO_ROOT/target/release/glassy" ]; }
have_alacritty() { command -v alacritty >/dev/null 2>&1; }
have_ghostty()   { command -v ghostty >/dev/null 2>&1; }
have_hyperfine() { command -v hyperfine >/dev/null 2>&1; }
have_vtebench()  { command -v vtebench >/dev/null 2>&1; }

check_tool() {
    local name="$1" have_fn="$2" hint="$3"
    if "$have_fn"; then
        info "found: $name"
    else
        warn "missing: $name — $hint"
        missing+=("$name")
    fi
}

info "checking for comparison tools..."
check_tool "alacritty" have_alacritty \
    "install via your distro package manager (e.g. 'dnf install alacritty', 'apt install alacritty') or https://github.com/alacritty/alacritty#installation"
check_tool "ghostty" have_ghostty \
    "install via your distro package manager (e.g. 'dnf install ghostty') or https://ghostty.org/download"
check_tool "hyperfine" have_hyperfine \
    "install via 'cargo install hyperfine' or your distro package manager"
check_tool "vtebench" have_vtebench \
    "install via 'cargo install --git https://github.com/alacritty/vtebench' (note: unmaintained upstream as of this writing — verify it still builds against your rustc before relying on it; this script falls back to a bespoke byte-firehose corpus below if vtebench itself fails to run)"

if [ "${#missing[@]}" -gt 0 ] && [ "$REQUIRE_ALL" -eq 1 ]; then
    die "missing required tool(s): ${missing[*]} (rerun without --require-all to get a partial report)"
fi

# ---------------------------------------------------------------------------
# 2. Build glassy release if stale.
# ---------------------------------------------------------------------------
if ! have_glassy_release || [ -n "$(find src Cargo.toml -newer target/release/glassy 2>/dev/null)" ]; then
    info "building glassy --release (target/release/glassy missing or stale)..."
    cargo build --release
fi
GLASSY_BIN="$REPO_ROOT/target/release/glassy"

# ---------------------------------------------------------------------------
# 3. vtebench corpora. Each corpus is a byte stream a terminal must parse+
#    render as fast as it can; vtebench measures wall time for the terminal's
#    PTY child to finish draining it. Corpus names match vtebench's own
#    benches/ directory (alacritty/vtebench on GitHub): alt-screen-random-write,
#    scrolling, unicode, dense-cells, light-cells, ...
#
#    Falls back to a small bespoke corpus generator (plain `printf`/`yes`-based
#    VT100 byte streams) if vtebench itself is unavailable or fails to run, so
#    the throughput section of the report degrades gracefully rather than
#    disappearing entirely.
# ---------------------------------------------------------------------------
CORPORA=(alt-screen-random-write scrolling unicode dense-cells light-cells)

BENCH_DIR="$(mktemp -d)"
trap 'rm -rf "$BENCH_DIR"' EXIT

fallback_corpus() {
    # A crude but terminal-agnostic byte firehose: N lines of printable ASCII
    # with SGR color changes, used only when vtebench is unavailable. Not a
    # substitute for vtebench's carefully-designed corpora — just enough to
    # get *a* throughput number when the real tool can't be used.
    local out="$1" lines="${2:-20000}"
    {
        for i in $(seq 1 "$lines"); do
            printf '\033[%dm line %d: the quick brown fox jumps over the lazy dog 0123456789\033[0m\n' \
                "$((31 + i % 7))" "$i"
        done
    } > "$out"
}

# Drive `$1` (a terminal binary) with `-e` running a program that cats the
# corpus into its own stdin, timing wall-clock via hyperfine if present, else
# plain `time`. Prints one markdown row: "| term | corpus | seconds |".
run_corpus() {
    local term_bin="$1" term_name="$2" corpus_file="$3" corpus_name="$4"
    local cmd="cat '$corpus_file'"
    local secs
    if have_hyperfine; then
        local json="$BENCH_DIR/hf-$term_name-$corpus_name.json"
        if hyperfine --warmup 1 --runs 5 --export-json "$json" \
            "$term_bin -e sh -c \"$cmd\"" >/dev/null 2>&1; then
            secs=$(python3 -c "import json;print(json.load(open('$json'))['results'][0]['mean'])" 2>/dev/null || echo "n/a")
        else
            secs="n/a (hyperfine run failed)"
        fi
    else
        local start end
        start=$(date +%s.%N)
        "$term_bin" -e sh -c "$cmd" >/dev/null 2>&1 || true
        end=$(date +%s.%N)
        secs=$(python3 -c "print($end-$start)" 2>/dev/null || echo "n/a")
    fi
    printf '| %s | %s | %s |\n' "$term_name" "$corpus_name" "$secs"
}

info "generating corpora..."
declare -A corpus_paths
if have_vtebench; then
    for c in "${CORPORA[@]}"; do
        f="$BENCH_DIR/$c.corpus"
        if vtebench --only-generate --dir "$BENCH_DIR" "$c" >/dev/null 2>&1 && [ -s "$f" ]; then
            corpus_paths["$c"]="$f"
        else
            warn "vtebench could not generate '$c' — falling back to a bespoke corpus for it"
            fallback_corpus "$f"
            corpus_paths["$c"]="$f"
        fi
    done
else
    warn "vtebench not installed — using bespoke fallback corpora only (see --write-docs output for caveats)"
    for c in "${CORPORA[@]}"; do
        f="$BENCH_DIR/$c.corpus"
        fallback_corpus "$f"
        corpus_paths["$c"]="$f"
    done
fi

# ---------------------------------------------------------------------------
# 4. Run the comparison matrix and emit the markdown table.
# ---------------------------------------------------------------------------
{
    echo "## Macro throughput (vtebench-style corpora)"
    echo
    echo "Machine: $(uname -srmo 2>/dev/null || uname -a)"
    if command -v lscpu >/dev/null 2>&1; then
        echo "CPU: $(lscpu | awk -F: '/Model name/{gsub(/^ +/,"",$2); print $2; exit}')"
    fi
    echo "Generated: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo
    echo "| terminal | corpus | mean wall time (s) |"
    echo "| --- | --- | --- |"

    for c in "${CORPORA[@]}"; do
        corpus_file="${corpus_paths[$c]}"
        run_corpus "$GLASSY_BIN" "glassy" "$corpus_file" "$c"
        if have_alacritty; then
            run_corpus "$(command -v alacritty)" "alacritty" "$corpus_file" "$c"
        fi
        if have_ghostty; then
            run_corpus "$(command -v ghostty)" "ghostty" "$corpus_file" "$c"
        fi
    done

    if ! have_alacritty && ! have_ghostty; then
        echo
        echo "> Only glassy was measured — alacritty and ghostty are not installed"
        echo "> on this machine. Install them (see the warnings above) and rerun"
        echo "> for a real comparison."
    fi
    if ! have_vtebench; then
        echo
        echo "> vtebench itself was not available; the corpora above are a bespoke"
        echo "> fallback (repeated colored text lines), not vtebench's real"
        echo "> alt-screen/scrolling/unicode benches. Install vtebench for a"
        echo "> meaningful throughput comparison."
    fi

    echo
    echo "## Startup (hyperfine)"
    echo
    if have_hyperfine; then
        json="$BENCH_DIR/startup.json"
        if hyperfine --warmup 3 --runs 20 --export-json "$json" \
            "$GLASSY_BIN -e true" >/dev/null 2>&1; then
            python3 -c "
import json
r = json.load(open('$json'))['results'][0]
print(f\"glassy \`-e true\`: mean {r['mean']*1000:.1f} ms, stddev {r['stddev']*1000:.1f} ms over 20 runs\")
"
        else
            echo "> hyperfine run failed (GPU window spawn under hyperfine's process"
            echo "> harness can be flaky in headless/CI environments — see"
            echo "> docs/benchmarks.md's known-gaps section)."
        fi
    else
        echo "> hyperfine not installed — skipped. \`cargo install hyperfine\`."
    fi

    echo
    echo "## Idle RSS"
    echo
    "$GLASSY_BIN" -e sh -c "sleep 5" &
    gpid=$!
    sleep 2
    if kill -0 "$gpid" 2>/dev/null; then
        rss_kib=$(awk '/VmRSS/{print $2}' "/proc/$gpid/status" 2>/dev/null || echo "n/a")
        echo "glassy idle RSS (2s after spawn, VmRSS): ${rss_kib} KiB"
    else
        echo "> could not sample RSS (process exited before the 2s sample point)"
    fi
    wait "$gpid" 2>/dev/null || true
} | tee "$BENCH_DIR/report.md"

if [ "$WRITE_DOCS" -eq 1 ]; then
    info "appending report to docs/benchmarks.md"
    {
        echo
        cat "$BENCH_DIR/report.md"
    } >> "$REPO_ROOT/docs/benchmarks.md"
fi

info "done."
