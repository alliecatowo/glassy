# Benchmarks

glassy has three tiers of benchmark now, replacing the old eyeballed-only
methodology below:

1. **Micro-benchmarks** (`cargo bench`, criterion) — hot, GPU-free functions
   benched in isolation: display-row extraction, config parsing, theme
   lookup, pane-damage tracking. Reproducible, statistically sound (criterion
   runs many samples and reports variance), and trend-trackable via
   `target/criterion/`'s saved baselines.
2. **Macro throughput** (`scripts/bench.sh`, vtebench-style) — feeds a VT100
   byte corpus through glassy/alacritty/ghostty over a real PTY and times how
   fast each one drains it. Self-checking: missing comparison binaries are
   reported clearly, not silently skipped.
3. **Startup / RSS** (`scripts/bench.sh`, hyperfine + `/proc`) — `hyperfine`-
   driven wall-clock startup and a sampled idle-RSS reading, replacing the old
   two-manual-runs approach with an N-run average.

Numbers below are still single-machine, single-developer-box figures — they
are not a controlled multi-hardware benchmark suite — but the *methodology*
is now scripted and rerunnable rather than eyeballed.

## Methodology

### Micro-benchmarks (criterion)

`benches/hot_paths.rs` targets four hot paths, reached through the `glassy`
library crate (`src/lib.rs`) added in this change — previously glassy was a
bin-only crate, so no external `benches/` target could import its internals
at all:

- **`collect_display_row`** (`src/app/helpers.rs`) — per-frame, per-row
  extraction of display cells from the alacritty_terminal grid. Benched at
  120×40 and 300×100 against a fixture `Term` populated with mixed
  ASCII/full-width-CJK/combining-mark content, reusing one output buffer
  across rows (matching the real per-frame call pattern).
- **`parse_config_file`** (`src/config/parse.rs`) — the `glassy.conf`
  parser, benched against a minimal config, a config exercising most
  documented keys, and configs with 1/5/20 `[profile.*]` sections (to catch
  profile-scan blowup). Reached via a `#[doc(hidden)]`
  `glassy::config::parse_config_file_bench` shim that keeps the parser's
  private `RawConfig` accumulator out of the public surface.
- **`theme_by_name`** / `theme_entries` / `theme_names`
  (`src/color/registry.rs`) — the 60-built-in-theme registry lookup, benched
  for a hit, a miss, and the two Vec-allocating listing functions.
- **`pane_damaged`** (`src/app/multipane.rs`, `App::pane_damaged`) — the thin
  wrapper around alacritty_terminal's own damage tracker that `render_split`
  uses to skip re-rebuilding unchanged panes. Benched against a real (but
  otherwise idle — a `true` "shell" that exits immediately) `Pty`, since this
  function's only argument is a live `Pty` and there's no lighter-weight
  fixture for it.

Run: `cargo bench --bench hot_paths` (add `-- --measurement-time 1` for a
quick compile+smoke-test run rather than a full statistically-stable one;
criterion's default measurement time is 5s per benchmark, of which there are
14 across the four groups).

### Macro throughput (vtebench)

`scripts/bench.sh` checks for `alacritty`, `ghostty`, `hyperfine`, and
`vtebench` on `$PATH` and reports exactly which are missing (with install
instructions) rather than silently degrading — on a typical dev machine
*none* of these four are preinstalled, so treat "missing" as the expected
starting state, not an error condition. For each installed terminal, it feeds
a VT100 corpus through `<terminal> -e sh -c 'cat corpus'` and times the wall
clock (`hyperfine` when available, plain `time` otherwise). If `vtebench`
itself isn't installed or fails to generate a corpus, the script falls back
to a bespoke repeated-colored-text corpus so the throughput section still
produces *a* number — clearly labeled as the fallback, not vtebench's real
alt-screen/scrolling/unicode benches.

```sh
scripts/bench.sh                 # best-effort: report whatever's installed
scripts/bench.sh --require-all   # fail loudly if any comparison tool is missing
scripts/bench.sh --write-docs    # also append the report to this file
```

### Startup / RSS

- **Startup**: `hyperfine --warmup 3 --runs 20 'glassy -e true'`, scripted in
  `scripts/bench.sh`. Known gap: glassy has no IPC "ready" signal a wrapper
  script could wait on (the IPC socket's `ls` verb answers once the control
  server is listening, but that's before the GPU window has rendered its
  first frame) — `-e true` exits after the shell runs `true`, which requires
  the window to have opened and the shell to have execed, so it's a
  reasonable proxy for "usable," not a precise first-frame timestamp. The
  2026-06-25 TTFF comparison below used the `log::info!` first-present
  timestamp instead, which *is* precise, but has no `hyperfine`-friendly exit
  point (nothing to compare against, and it requires a debug log line to
  exist, not a general-purpose flag).
- **Idle RSS**: `/proc/<pid>/status`'s `VmRSS` field, sampled 2s after spawn
  while sitting at an idle shell (`-e sh -c 'sleep 5'`). `scripts/bench.sh`
  automates a single sample; averaging multiple samples is a documented
  future improvement, not yet scripted.
- **Input latency**: **not measured**. No hardware photodiode/typometer rig
  exists for this repo. This remains an explicit known gap rather than an
  eyeballed number — see "Known gaps" below.

## Micro-benchmark results (this run)

Measured on this dev machine (AMD Ryzen 9 6900HX, Linux 7.0.14-201.fc44,
`cargo bench` default profile — not the release LTO profile, since criterion
needs debug-assertions-off but doesn't require fat LTO/1-codegen-unit to be
representative of relative hot-path cost). `cargo bench --bench hot_paths --
--measurement-time 1` (a short run, enough to prove the harness compiles and
executes cleanly, not a final statistically-tight baseline — see the caveat
below).

| Benchmark | Time |
| --- | --- |
| `collect_display_row/120x40` (whole-frame: 40 row calls) | 4.76 µs (≈119 ns/row) |
| `collect_display_row/300x100` (whole-frame: 100 row calls) | 28.18 µs (≈282 ns/row) |
| `parse_config_file/minimal` (2 keys) | 150.4 ns |
| `parse_config_file/full` (~50 keys + 1 profile + keybindings) | 3.66 µs |
| `parse_config_file/profiles_1` | 399.8 ns |
| `parse_config_file/profiles_5` | 1.50 µs |
| `parse_config_file/profiles_20` | 7.88 µs |
| `theme_by_name/hit` (`"tokyo-night"`) | 88.7 ns |
| `theme_by_name/miss` (unknown name) | 2.42 µs |
| `theme_entries` (60-entry Vec alloc) | 3.06 µs |
| `theme_names` (60-entry Vec alloc) | 3.10 µs |
| `pane_damaged/idle` (no new PTY output) | 16.2 ns |

A few things worth calling out:

- **`theme_by_name` hit vs. miss is a ~27× gap** (88.7 ns vs 2.42 µs): a hit
  short-circuits on the first user-theme/builtin match, while a miss walks
  the entire 60-entry table (both user themes — empty here — and
  `BUILTIN_THEMES`) doing a normalized string compare per entry before
  concluding nothing matched. Not a problem in practice (called once at
  startup + on theme switch, not per-frame), but a real, measured cost.
- **`parse_config_file` scales roughly linearly with `[profile.*]` count**
  (400 ns → 1.5 µs → 7.9 µs for 1/5/20 profiles, ~380-400 ns/profile),
  consistent with a single top-to-bottom line scan — no evidence of
  quadratic blowup from profile count.
- **`pane_damaged` is genuinely cheap** (16 ns) — confirms the recon
  characterization of it as "a thin wrapper," dominated by the `FairMutex`
  lock/unlock rather than any real damage computation.
- **`collect_display_row` cost scales with area, not just row count**: 300×100
  is ~2.5× the rows of 120×40 but ~5.9× the cells, and its measured time is
  ~5.9× — consistent with the O(cols) per-row loop, no surprises.

> These are from a single short (`--measurement-time 1`) run for the
> purposes of proving the benchmarks build and execute correctly against the
> new `[lib]` target, not a tuned baseline. A real regression-tracking
> baseline should use criterion's default measurement time (or longer) and
> `--save-baseline`, on a quiesced machine.

## Binary size

### `[lib]` target (this change, item 1)

Splitting the crate into a `glassy` library (`src/lib.rs`) plus a thin
`glassy` binary (`src/main.rs`) was expected to be size-neutral — same code
reachable from `fn main`, just a different compilation-unit boundary.
Measuring (both stripped, `cargo build --release`, isolated by toggling only
`alacritty_terminal`'s `serde` feature back on so this row is lib-split-only,
serde-drop-free):

| Build | Size (bytes) | Δ vs. baseline |
| --- | --- | --- |
| Baseline (bin-only crate, pre-`[lib]`, pre-serde-drop) | 11,560,440 | — |
| + `[lib]` target (this change, item 1) | 11,630,640 | **+70,200 (+0.61%)** |

So: **not** byte-for-byte identical — there's a small, real regression. The
most likely cause: the four hot-path functions bumped from `pub(crate)`/
`pub(super)` to `pub` (`collect_display_row`, `App::pane_damaged`,
`parse_config_file_bench`, plus the pre-existing `pub` `theme_by_name`) are
now part of the *library's* external API surface, not just the final
binary's internal call graph. Even under `lto = "fat"` + `codegen-units = 1`,
a `pub` item in an rlib crate must be compiled with external linkage — the
lib crate's own compilation can't prove nothing outside the final `glassy`
bin will ever call it, so it can't be as aggressively deadcode-eliminated
within the lib crate's own codegen unit as it could when everything lived in
one bin-crate compilation unit. The final LTO link *does* still fold and
prune where it can, hence a small (not large) delta, not something
proportional to the whole four-function surface.

This is a real, honestly-reported tradeoff of making these four functions
externally reachable for `benches/hot_paths.rs` — 0.61% is a small price for
gaining a criterion micro-benchmark suite, but it is not "no regression" as
originally hoped, so it's recorded here rather than glossed over.

### Dropping alacritty_terminal's default `serde` feature (item 5)

`alacritty_terminal = "0.26.0"` pulled its own `serde` feature by default
(Serialize/Deserialize derives on `Grid`/`Cell`/`Term`, plus the `bitflags/
serde` and `vte/serde` features it enables in turn). glassy never
(de)serializes an alacritty_terminal type directly — its own config/session
serde (`src/config`, `src/session`) is independent — so this looked like dead
weight. Changed to `alacritty_terminal = { version = "0.26.0",
default-features = false }`:

| Build | Size (bytes) | Δ |
| --- | --- | --- |
| `[lib]` split, serde still on (isolated) | 11,630,640 | — |
| `[lib]` split, serde off (this change's final state) | 11,630,640 | **0** |

**Measured release-binary-size delta: zero bytes.** `cargo tree -e features -p
alacritty_terminal` confirms the toggle takes effect at the dependency-graph
level (`serde`/`serde_core`/`serde_derive` disappear from the tree entirely
when off — three fewer crates to compile), and the two resulting binaries
have different SHA-256 hashes (so it's not a stale/cached artifact — genuinely
different bytes were compiled), yet strip to the identical final size. The
explanation: nothing in glassy ever calls the generated `Serialize`/
`Deserialize` impls, so `lto = "fat"` + `strip = true` was *already* eliminating
100% of that dead code from the stripped release binary before this change —
the feature flag just stops it from being compiled into the intermediate
rlib in the first place. The real benefit of dropping it is **not** release
binary size but a smaller `cargo build`/`check`/`clippy` dependency graph
(three fewer crates: `serde`, `serde_core`, `serde_derive`, the latter a
proc-macro crate) and a marginally smaller **debug** build (debug builds have
no LTO to eliminate the dead derive code, so `serde`'s codegen — including
the `serde_derive` proc-macro's own compile cost — is pure overhead there).
Kept anyway: it's strictly cheaper along every axis except release-binary
bytes, where it's a wash.

## Why it stays quiet

- **Render-on-demand.** glassy does not run a fixed frame loop. With nothing
  changing on screen it issues no frames at all, so idle CPU is **0%** — no
  background spin, no periodic wakeups.
- **Damage-based redraw.** When the screen *does* change, only the cells that
  actually changed are re-rasterized and re-uploaded to the glyph atlas, rather
  than redrawing the whole grid every frame.
- **Lean binary.** Fat LTO + one codegen unit + `panic = "abort"` + stripping
  keep the release binary small (see the size table above).
- **Lean swapchain.** The swapchain uses **Fifo** present mode (vsync,
  guaranteed available on every backend) with `desired_maximum_frame_latency: 2`.
  Fifo holds the minimum image count (2, vs. Mailbox's typical 3 — roughly
  8 MB saved at 1080p `Bgra8`), never redraws an idle frame, and sidesteps the
  tearing/latency tradeoffs Mailbox or Immediate exist for — tradeoffs that
  don't matter for a glyph grid that only repaints on damage anyway. (Latency
  is not quantified here; it is a design choice, noted for context.)

## w10/perf wave improvements

The following targeted changes were landed in the `w10/perf` wave. Numbers are
deltas observed on the same dev machine; they are approximate because startup
is eyeballed and RSS varies with GPU driver / window size.

### Parallel GPU + font init

**Before:** GPU adapter/device request (driver IPC, validation layer spin-up)
and font discovery/shaping ran sequentially. On a cold run with a warm page
cache the observed sequence cost ~80-180 ms total.

**After:** GPU init moves to a background thread; font load runs concurrently
on the main thread. The two paths join before surface configuration.
Observed saving: **50-150 ms** off TTFF (time to first frame) on the dev
machine. The saving is larger on cold runs or slow-driver setups (Vulkan MoltenVK,
lavapipe fallback) where adapter selection takes longer.

### Lazy image atlas

**Before:** A 1024×1024 RGBA8 GPU texture (4 MiB of VRAM) was allocated at
startup regardless of whether the session ever displayed inline images.

**After:** The texture and its bind group are created on the first
`draw_image` call. Sessions that never display kitty/sixel images — the
typical case for a text terminal — **never pay this cost**.

| Condition | Before | After |
| --- | --- | --- |
| Text-only session VRAM at idle | baseline | -4 MB |
| Session with inline images | baseline | same (allocated on demand) |

### PTY thread stack

**Before:** Each PTY reader thread used the OS default stack (8 MiB on Linux).
The read/parse loop has no deep recursion and only needs a 64 KiB read buffer
plus a handful of locals.

**After:** Stack size capped at 256 KiB per PTY thread via
`std::thread::Builder::stack_size`. The saving is per-session:

| Sessions | Before (stack RSS) | After (stack RSS) |
| --- | --- | --- |
| 1 | 8 MiB committed | 256 KiB committed |
| 4 (tabs) | 32 MiB committed | 1 MiB committed |
| 10 (heavy split use) | 80 MiB committed | 2.5 MiB committed |

> "Committed" stack pages may not all be faulted in (the OS allocates lazily),
> so the actual RSS saving is smaller on Linux; the virtual address space
> saving is the stated figure.

### Pipeline cache

**Before:** Every launch compiled all three WGSL shaders from scratch (bg,
fg, overlay pipelines). On Vulkan this involves SPIR-V → driver-IR compilation,
which can take tens of milliseconds on the first run after a driver update.

**After:** The compiled pipeline cache is saved to
`$XDG_CACHE_HOME/glassy/<adapter-key>` on exit and reloaded on the next
launch. This is a no-op on non-Vulkan backends (Metal uses its own cache,
Dx12/OpenGL don't support `PIPELINE_CACHE`).

| Condition | Before | After |
| --- | --- | --- |
| First launch (cold cache) | baseline | same |
| Subsequent launches (warm cache, Vulkan) | baseline | -20-80 ms startup |

## glassy vs ghostty (2026-06-25)

Head-to-head comparison on the same machine, both spawned fresh under the same
Wayland session (wayland-0), sampled in a single sitting.

### Machine

- CPU: AMD Ryzen 7 6800U (Zen 3+, 8-core)
- GPU: AMD Radeon 680M (integrated, RDNA 2)
- Driver: RADV (Mesa Vulkan, open-source)
- OS: Fedora Linux 7.0.11-100.fc43 (Wayland, GNOME)
- glassy version: develop @ 2eacb59 (release build, stripped, fat LTO)
- ghostty version: 1.3.1-2.fc43 (distro package, `/usr/bin/ghostty`, 123 MB)
- kitty / alacritty: not installed on this machine

### Results

| Metric | glassy | ghostty | glassy advantage |
| --- | --- | --- | --- |
| **Binary size (stripped)** | **11.0 MB** | 123.2 MB | **11× smaller** |
| **Idle RSS** (VmRSS, /proc, 2 runs avg) | **~133 MB** | ~214 MB | **~38% less RAM** |
| **Idle CPU** (ticks/s after 10 s settle) | **0 ticks (~0%)** | 1 tick (~≤1%) | effectively tied |
| **TTFF** (glassy: `log::info` first-frame; ghostty: `time -e true` wall) | **~450 ms** | ~710 ms | **~260 ms faster** |
| **Threads at idle** | **9** | 45–47 | **5× fewer threads** |

### Method notes

**Binary size** — `stat --format="%s"` on each stripped executable.
glassy: `target/release/glassy` (cargo release profile: fat LTO, 1 codegen
unit, `panic = "abort"`, strip). ghostty: `/usr/bin/ghostty` (distro RPM).

**Idle RSS** — Each terminal spawned with `$WAYLAND_DISPLAY=wayland-0` into
a fresh idle shell (`-e /bin/bash`). RSS read from
`/proc/<spawned-pid>/status` (VmRSS) after 4–5 s. Two runs each; both
were consistent to within 2 MB. glassy runs: 132.6 MB, 132.5 MB.
ghostty runs: 212.7 MB, 215.0 MB. Only that spawned PID was measured;
the existing ghostty session (the one Claude runs in) was never touched.

**Idle CPU** — Both terminals left idle (no typing, no shell output) for
10 s. CPU ticks read from `/proc/<pid>/stat` fields 14+15 (utime+stime)
across a 1-second window. glassy: 0 ticks. ghostty: 1 tick. At HZ=100
this is ≤1% each; both are effectively zero at rest. The `ps -o pcpu=`
cumulative averages reported earlier were still decaying (startup work
amortised) — the per-second tick count is the accurate idle figure.

**TTFF (time to first frame)** — glassy logs
`"glassy time-to-first-frame: N ms"` via `log::info!` on the first
`queue.submit` + `surface.present` call (measured from `Instant::now()`
set in `App::new`). Two warm-cache runs: 448.3 ms and 455.0 ms (~451 ms
avg). ghostty has no equivalent intrinsic log; proxy: `time ghostty
--gtk-single-instance=false -e true`, which measures exec→process-exit
(window open + one shell invocation + exit): two runs 725 ms and 702 ms
(~714 ms avg). The ghostty figure includes shell startup and process
teardown, so it overstates pure TTFF slightly; the glassy figure is the
first-render timestamp only. Both figures come from the same warm-cache
Vulkan state.

**Threads** — read from VmRSS `Threads:` field in
`/proc/<spawned-pid>/status` while at idle. glassy: 9 (main + wgpu
device + 3 pipeline compile workers + pty reader + pty writer + 2
others). ghostty: 45–47 (GTK runtime, tokio thread pool, font threads,
etc.).

### Caveats

- Single machine, single session, single run per metric (no
  `hyperfine`-style averaging). Numbers rounded.
- ghostty TTFF is a wall-time proxy (`time -e true`), not an intrinsic
  first-present timestamp; it systematically overstates pure TTFF.
- RSS includes shared library pages (GPU driver, libc, Wayland client
  libs). glassy's 133 MB figure includes roughly 80–90 MB of GPU-driver
  shared pages that would be present regardless. The marginal per-process
  footprint is lower.
- kitty and alacritty were not installed on this machine and could not be
  measured.

## Known gaps

- **Input latency** is not measured empirically (no photodiode/typometer
  rig). Pending: either a synthetic PTY-write → first-frame-present
  round-trip timestamp (reusing the existing TTFF `log::info!` pattern), or
  an explicit hardware rig.
- **Macro throughput comparisons** (`scripts/bench.sh`) require alacritty,
  ghostty, and vtebench installed; none are on this dev machine as of this
  writing, so the vtebench-vs-alacritty-vs-ghostty table has not actually
  been populated with real comparison data yet — only glassy-vs-itself
  (or the bespoke fallback corpus) can run here. Rerun on a machine with
  those tools installed for a real comparison.
- **CI regression tracking**: `cargo bench` numbers are not yet wired into
  CI (no `cargo bench --save-baseline` job) — out of scope for this change
  per its spec; a future change could add one, gated to avoid blocking PRs
  on noisy shared-runner CPU allocation.

## Caveats

- Single machine, single run order; no warmup/averaging discipline beyond
  casual repetition for the narrative (non-criterion, non-hyperfine) numbers
  above.
- The ghostty comparison is binary size only, and against a specific published
  release; it is not a feature-for-feature comparison.
- These numbers will change as glassy's hand-rolled internals replace the
  currently-vendored crates. They are a snapshot, not a contract.
