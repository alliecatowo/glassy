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

## Caveats

- Single machine, single run order; no warmup/averaging discipline beyond
  casual repetition.
- The ghostty comparison is binary size only, and against a specific published
  release; it is not a feature-for-feature comparison.
- These numbers will change as glassy's hand-rolled internals replace the
  currently-vendored crates. They are a snapshot, not a contract.
