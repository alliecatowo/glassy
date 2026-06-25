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
- **Low input latency.** The swapchain uses Mailbox present mode with
  `max_frames_in_flight = 1`, so a keystroke reaches the screen on the next
  frame instead of queueing behind buffered ones. (Latency is not quantified
  here; it is a design choice, noted for context.)

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

## Caveats

- Single machine, single run order; no warmup/averaging discipline beyond
  casual repetition.
- The ghostty comparison is binary size only, and against a specific published
  release; it is not a feature-for-feature comparison.
- These numbers will change as glassy's hand-rolled internals replace the
  currently-vendored crates. They are a snapshot, not a contract.
