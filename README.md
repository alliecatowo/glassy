# glassy

A minimal, GPU-accelerated terminal emulator written in Rust.

glassy aims to be a fast, lightweight terminal: it renders the grid with an
instanced-quad `wgpu` pipeline, idles at 0% CPU until there's something new to
draw, and ships as a single stripped binary. VT parsing and PTY handling are
currently delegated to [`alacritty_terminal`], with the intent of hand-rolling
and replacing chunks over time.

## Features

- **GPU rendering** — instanced quads via `wgpu`; one background pass and one
  glyph pass per frame over a shared glyph atlas.
- **Render on demand** — the event loop runs `ControlFlow::Wait`, so an idle
  terminal submits no frames and burns no CPU. Wakeups are coalesced to at most
  one redraw per monitor refresh, so a fast producer (e.g. streaming output)
  collapses into a single redraw instead of one per token burst.
- **CPU/GPU split text stack** — glyph shaping and rasterization use
  `cosmic-text` + `swash`, fully cached per `(char, bold, italic)`; the renderer
  just uploads bitmaps into the atlas.
- **Smart font discovery** — `GLASSY_FONT` override, then a curated list of good
  monospace families resolved via fontconfig, then a system scan; with emoji and
  CJK fallback.
- **Modern dark theme by default** — a Tokyo Night-inspired palette, with OSC and
  256-color overrides honored.

## Requirements

- Rust (latest stable — see `mise.toml`; this crate uses edition 2024)
- A Vulkan/Metal/DX12-capable GPU and drivers (whatever `wgpu` supports)
- Linux: `fontconfig` (`fc-match`) is used for font discovery

## Build & run

```sh
cargo run --release
```

A debug build keeps glassy's own crate quick to recompile while still optimizing
the heavy dependencies (`wgpu`, `cosmic-text`):

```sh
cargo run
```

## Configuration

glassy is configured via environment variables:

| Variable            | Effect                                                        |
| ------------------- | ------------------------------------------------------------ |
| `GLASSY_FONT`       | Absolute path to a font file to use as the primary font.     |
| `GLASSY_CAPTURE`    | Headless mode: render once to the given `.ppm` path and exit.|
| `GLASSY_CAPTURE_MS` | Delay (ms) before the headless capture (default lets the shell start up). |
| `RUST_LOG`          | Standard `env_logger` filter (e.g. `RUST_LOG=glassy=debug`). |

The shell is the user's login shell, resolved from the passwd database (falling
back to `$SHELL`).

## Architecture

The source is small and split by concern:

| File              | Responsibility                                                          |
| ----------------- | ---------------------------------------------------------------------- |
| `src/main.rs`     | Entry point; builds the winit event loop and `App`.                    |
| `src/app.rs`      | winit UI/render driver; wakeup coalescing and the render-on-demand throttle. |
| `src/pty.rs`      | PTY + VT integration via `alacritty_terminal` on a background thread.  |
| `src/renderer.rs` | `wgpu` grid renderer: instanced-quad pipelines and the glyph atlas.    |
| `src/text.rs`     | Font loading, cell metrics, and on-demand glyph rasterization (GPU-free). |
| `src/color.rs`    | Resolves cell colors to RGBA, including the default theme.             |
| `src/input.rs`    | Keyboard encoding: winit `KeyEvent` → PTY byte sequences.              |
| `src/shader.wgsl` | Background and glyph WGSL pipelines.                                   |

The PTY thread reads the fd, runs the VT parser, and mutates a shared `Term`
behind a `FairMutex`. It wakes the UI thread via an `EventLoopProxy`; the UI
thread only ever reads `Term` state and writes input bytes through the
`Notifier`.

[`alacritty_terminal`]: https://crates.io/crates/alacritty_terminal
