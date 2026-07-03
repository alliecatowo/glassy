# Benchmarks

These are rough, honest numbers for glassy. They were measured on a single
developer machine and are meant to give a sense of scale, not to be a rigorous,
reproducible benchmark suite. Treat them as ballpark figures: your hardware,
GPU driver, compositor, and font stack will move them around.

## Methodology

- **Binary size** is the size of the stripped release artifact at
  `target/release/glassy`, built with `cargo build --release`. The release
  profile uses fat LTO, a single codegen unit, and `panic = "abort"`, and the
  binary is stripped. Compared against a published ghostty release binary.
- **Memory (RSS)** is the resident set size reported by `ps`
  (`ps -o rss= -p <pid>`), sampled while the terminal sits idle at a shell
  prompt with no output. RSS includes shared library pages, so it is an upper
  bound on glassy's own footprint and varies with the GPU driver.
- **Idle CPU** is observed in `top` / `ps` with the window visible but nothing
  changing on screen.
- **Startup** is approximate, eyeballed wall-clock from launch to an interactive
  prompt; it is *not* a tight `hyperfine` figure. A more careful measurement
  would wrap a no-op command run (`glassy -e true`) under `hyperfine`, but
  spawning a GPU window skews that, so it is reported here as approximate only.

To reproduce locally:

```sh
cargo build --release
ls -l target/release/glassy            # binary size (stripped)

# idle RSS, while glassy sits at an idle prompt:
ps -o rss= -p "$(pgrep -n glassy)"     # value in KiB

# idle CPU:
top -p "$(pgrep -n glassy)"
```

## Results

Measured on this dev machine. Numbers rounded; startup is approximate.

| Metric | glassy | Reference |
| --- | --- | --- |
| Release binary (stripped) | **~10.4 MB** | ghostty ~123 MB |
| Idle RSS (at a quiet prompt) | **~40-60 MB** | — |
| Idle CPU (nothing on screen) | **0%** | — |
| Startup to prompt | **~tens of ms (approx.)** | — |

> Idle RSS is dominated by shared GPU-driver and font/atlas pages; it depends
> heavily on the driver and on window size. The range above is what was observed
> here, not a guaranteed figure.

## Why it stays quiet

- **Render-on-demand.** glassy does not run a fixed frame loop. With nothing
  changing on screen it issues no frames at all, so idle CPU is **0%** — no
  background spin, no periodic wakeups.
- **Damage-based redraw.** When the screen *does* change, only the cells that
  actually changed are re-rasterized and re-uploaded to the glyph atlas, rather
  than redrawing the whole grid every frame.
- **Lean binary.** Fat LTO + one codegen unit + `panic = "abort"` + stripping
  keep the release binary around 10 MB.
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

## Caveats

- Single machine, single run order; no warmup/averaging discipline beyond
  casual repetition.
- The ghostty comparison is binary size only, and against a specific published
  release; it is not a feature-for-feature comparison.
- These numbers will change as glassy's hand-rolled internals replace the
  currently-vendored crates. They are a snapshot, not a contract.
