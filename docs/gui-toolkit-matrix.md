# glassy GUI-toolkit comparison matrix

Standalone matrix extracted from [`gui-toolkit-spike.md`](./gui-toolkit-spike.md). See that
document for the recommendation, deep-dives, integration sketch, and sources.

Research date: **2026-06-24**. glassy's stack at the time: Rust 2024, **winit 0.30.13**,
**wgpu 29.0.3**, cosmic-text 0.19, single winit event loop + single wgpu `Device`/`Queue`/`Surface`.
Baseline release binary measured at **12 MB** (stripped, fat-LTO, panic=abort).

## The decisive columns

The two columns that decide everything for glassy:

- **Embed our wgpu grid?** — can the toolkit host glassy's existing instanced cell-grid renderer
  *sharing one wgpu `Device`/`Queue` and one winit loop*, drawing only the chrome? A "NO" here is
  effectively a dealbreaker because the terminal renderer must stay.
- **0% idle?** — does it park at `ControlFlow::Wait` (0% CPU/GPU) when nothing is animating?

| Toolkit | Latest (2026) | wgpu / winit pin | Embed our wgpu grid? | 0% idle? | Binary add | Frosted-glass vibe | Linux (Way+X11) | mac | Win | License | Maturity / shipping | Effort | Verdict |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **Hand-rolled (current)** | n/a | exact (it *is* the stack) | **YES — it is the grid** | **YES** (Poll only while `Anim` unsettled) | **0** (3 primitives, 0 deps) | yes — SDF rrect, per-corner radii, theme tokens, translucency already shipping | native | native | native | own | shipping in glassy now | n/a | **baseline — keep & fix** |
| **egui** (+egui-wgpu/-winit) | 0.34.3 (May 2025) | **wgpu 29.0, winit 0.30.13 — exact match** | **YES** — `CallbackTrait` shares `RenderState` device; or grid pass first + chrome on top, same encoder | **YES** in reactive mode (`request_repaint_after`); minor spurious-repaint bug #4499 (filterable) | ~2–4 MB | **partial** — fully themeable + custom widgets + DIY blur callback; *flat by default*, premium look = real work | mature (Wayland transparency quirks) | mature | mature | MIT/Apache | very high (Rerun, many tools) | **MED** | **strong fit** |
| **Vello** (renderer only) | 0.9.0 (May 2026) | **wgpu ^29.0.3 — exact match**; no winit dep | **YES** — `Renderer::new(&device)` + `render_to_texture`; you write the composite blit | **YES** — no loop; driven by glassy's `Wait` | ~2–5 MB (compute shaders) | strong vector/text; **blur not yet shipped** (active); no widgets at all | runs everywhere wgpu runs | yes | yes | Apache-2.0 | alpha but momentum; powers Xilem | **LOW-MED** (must build widgets) | **strong fit (hybrid)** |
| **Iced** | 0.14 stable / 0.15-dev | stable **wgpu 27** (mismatch!); master wgpu 29 + **winit fork** | PARTIAL — `shader::Primitive` shares device, *but* version pin forces git master + winit-fork reconciliation | YES (reactive since 0.14) | ~4–8 MB (heavier) | partial — themeable, window blur mac/Linux; not cinematic | mature (COSMIC) | ok | least mature | MIT | high (libcosmic/COSMIC) | **HIGH** | viable, blocked by version drift |
| **Slint** | 1.17.0 (Jun 2026) | `unstable-wgpu-29` matches; *unstable* API | PARTIAL — `WGPUConfiguration::Manual` + `set_rendering_notifier` share device; loop re-arch needed | mostly; **Wayland idle-CPU bug #5780** (closed, unfixed) | FemtoVG ~2.6 MB / Skia ~20 MB | partial — themeable; **no first-class Wayland blur** | bug on Wayland idle | ok | ok | **Royalty-Free OK for MIT/Apache** (not GPL); attribution req. | embedded-first; desktop newer | HIGH | weak |
| **Xilem** | 0.4.0 (Oct 2024) | **wgpu 28** (mismatch); winit 0.30.13 | **NO** — owns loop; maintainer rejected embed PR #879; texture-widget unimplemented | likely (alpha, unverified) | 8–15 MB | widgets present; Vello blur gap | experimental | exp | exp | Apache-2.0 | pre-1.0, no shipping app | HIGH | weak |
| **Floem** | 0.2.0 (Nov 2024) | **wgpu 27** (mismatch) + **custom winit fork** | **NO** (PARTIAL) — `PaintCx` is Vello-only; no raw-wgpu injection; fork conflicts | yes (reactive signals) | smaller than Lapce; n/a | CSS-like theming; blur gap | Lapce ships it | yes | yes | MIT | **Lapce** (strong proof) | HIGH | weak (dep conflict) |
| **Vizia** | 0.4.0 (Apr 2026) | **OpenGL/Skia, NO wgpu**; winit 0.30 | **NO** — different GPU API; can't share a wgpu device | YES (`ControlFlow::Wait`) | Skia ~10–30 MB (**breaks budget**) | Skia blur excellent | ok | ok | ok | MIT | nih-plug audio plugins | HIGH | not viable |
| **Makepad** | 1.0.0 (May 2025) | **own GPU + own windowing, no winit/wgpu** | **NO** — full parallel stack | unconfirmed (game-style) | moderate | shader-based, beautiful | OpenGL (dated) | Metal | D3D11 | MIT | 1.0; Makepad-team-first | HIGH | not viable |
| **GPUI** (Zed) | 0.2.2 (Oct 2025) | own windowing; Linux wgpu since Feb 2026 (Metal/D3D elsewhere) | **NO** (upstream) — no external-wgpu API; owns loop; continuous render | **NO** (120 FPS game loop) | 5–15 MB | beautiful (Zed) | recently stabilized, rough | mature | new | Apache-2.0 | **Zed** (but Zed-first) | HIGH | not viable |
| **Freya** (Dioxus+Skia) | 0.3.4 / 0.4-rc | winit yes, **Skia not wgpu** | **NO** — Skia≠wgpu, compositing mismatch; owns loop | plausible (unconfirmed) | **Skia ~20–30 MB (breaks budget)** | Skia blur good | ok | ok | ok | MIT | young, solo, no prod app | HIGH | not viable |
| **Dioxus-native / Blitz** | 0.3.0-alpha.6 (Jun 2026) | **winit + wgpu (via Vello)** | PARTIAL (experimental, currently regressed); owns loop; CSS model wrong for chrome | plausible (doc renderer) | 10–25 MB (Stylo heavy) | CSS theming | winit-broad but pre-alpha | pre-alpha | pre-alpha | Apache/MIT (+MPL stylo) | **pre-alpha, no prod app** | HIGH | revisit at 1.0 |

## One-line readout

- **Only three options share glassy's wgpu device + winit loop *without a version/stack fight*:**
  the **hand-rolled** layer (it *is* the stack), **egui** (exact wgpu-29/winit-0.30 pin + first-class
  `CallbackTrait`), and **Vello** (exact wgpu-29 pin, caller-owns-device renderer).
- **Iced and Slint are technically embeddable but pay a version-pin / API-instability tax.**
- **Everything else** (Xilem, Floem, Vizia, Makepad, GPUI, Freya, Blitz) either can't share a wgpu
  device, owns its own loop, or blows the binary budget — all dealbreakers for glassy today.
