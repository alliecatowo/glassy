# glassy GUI-toolkit feasibility spike

**Date:** 2026-06-24
**Question:** glassy's chrome (tabs, panes, settings panel, menus, help overlay, context menus,
toasts) is a hand-rolled immediate-mode GUI drawn directly on wgpu + cosmic-text. It keeps producing
interaction bugs. Should we adopt a Rust-native GUI toolkit instead, and if so which?

Companion: [`gui-toolkit-matrix.md`](./gui-toolkit-matrix.md) (the matrix standalone).

---

## Executive summary

**Recommendation: KEEP the hand-rolled chrome and fix it. If — and only if — we later decide the
maintenance cost is genuinely unsustainable, the single approved migration target is a HYBRID with
egui for chrome while keeping the wgpu cell-grid; Vello is the runner-up hybrid. Do NOT adopt a
full toolkit that owns the window/event loop.**

The spike was framed as "stop wasting time on hand-rolled GUI and adopt a toolkit." After grounding
the analysis in glassy's *actual* code and verifying every toolkit's current (2025–2026) embedding
story, the evidence points the other way for the *near term*, and narrows the *long-term* escape
hatch to exactly one toolkit. The reasoning:

1. **glassy's chrome is not a heavyweight toolkit — it is ~95 KB of code emitting three renderer
   primitives.** The "toolkit" being considered for replacement is `src/gui/` (`mod.rs` 682 lines,
   `widgets.rs` 608, `chrome.rs` 631, `help.rs` 473) plus the overlay pipeline in
   `src/renderer/overlay.rs` (367 lines). It owns **zero GPU state and zero dependencies**. It emits
   `push_overlay_px` / `push_overlay_rrect_px` / `push_overlay_glyph_px` into two overlay passes that
   already run last in the existing renderer. The bugs we keep hitting (overlay dismiss-on-motion,
   rect/DPI glitches, rounded-corner compositing, render deadlock) are *interaction-logic and
   damage-tracking* bugs, not "we picked the wrong rendering primitive" bugs. The latest develop
   commit (`84f1d4b fix: six live bugs`) is exactly this class of fix. A toolkit swap does not make
   tab hit-testing, modal dismiss semantics, or DPI math disappear — it relocates them into someone
   else's abstraction and adds a dependency we must track forever.

2. **The hard constraint that eliminates almost every candidate is the wgpu/winit-sharing
   requirement.** glassy's terminal content is a custom instanced-quad wgpu renderer that must stay,
   sharing one `Device`/`Queue`/`Surface` and one winit event loop. Of eleven options evaluated,
   **only three can host that custom grid without a version fight or a parallel stack**: the
   hand-rolled layer (it *is* the stack), **egui** (pins wgpu 29.0 + winit 0.30.13 — an exact match
   to glassy — and exposes `egui_wgpu::CallbackTrait` that shares its `Device`/`Queue`), and **Vello**
   (pins wgpu ^29.0.3 — exact match — and is a caller-owns-the-device renderer). Iced and Slint are
   *technically* embeddable but force either a git-master dependency with a winit fork (Iced) or an
   explicitly-unstable API plus a Wayland idle-CPU regression (Slint). Xilem, Floem, Vizia, Makepad,
   GPUI, Freya, and Blitz each fail outright: wrong GPU API, owns the loop, blows the ~10 MB binary
   budget, or is pre-alpha.

3. **The 0%-idle invariant is a second filter that egui *passes* but several "beautiful" options
   fail.** glassy parks on `ControlFlow::Wait` and only flips to `ControlFlow::Poll` while a `gui::Anim`
   is unsettled (`src/app/event_loop.rs` — the chrome animation system already does exactly the
   "redraw only while animating" thing the prompt demands). egui's reactive mode honors this; GPUI
   renders at 120 FPS continuously (fail); Slint has a closed-but-unfixed Wayland idle-CPU bug.

4. **The binary budget is a third filter.** The current release binary is **12 MB** (measured:
   `target/release/glassy`, stripped, fat-LTO, `panic=abort`). We are already slightly over the
   ~10 MB aspiration. Skia-based toolkits (Vizia, Freya) add 15–30 MB and break the budget outright;
   Iced and Blitz add meaningfully. egui adds ~2–4 MB on top of a stack we already pay for; Vello
   ~2–5 MB. The hand-rolled layer adds **0 MB**.

**Why not "just adopt egui now"?** Because (a) the aesthetic that the owner wants — "frosted
deep-glass, edge-lit, rounded surfaces, gooey not Vim-looking" — is egui's *weakest* area; egui is
flat-by-default and a premium glass look requires custom widgets + a custom wgpu blur callback, i.e.
re-doing the exact bespoke work we already have; (b) glassy already *ships* a frosted-glass look
(theme tokens, SDF rounded rects with per-corner radii, translucency) that egui would force us to
rebuild; and (c) the chrome is small enough that porting it is comparable effort to fixing it, with
strictly more dependency risk. The recommendation is therefore: **fix the hand-rolled layer now;
hold egui-hybrid as a pre-vetted escape hatch** if interaction bugs keep recurring after the bug
class is properly addressed (shared retained interaction state + a single damage/dismiss model).

**Two framings (the prompt asks for both):**

- **(a) "Look native no matter the shell/platform":** *No Rust toolkit wins this* — none of them
  render true platform-native widgets the way Qt/GTK/AppKit do; they all draw their own. Adopting one
  to "look native" buys nothing. If true native look were the goal, the answer would be platform
  toolkits (Cocoa/Win32/GTK), which is out of scope and hostile to a single ultra-light binary.
- **(b) "Native OS title bar + tasteful custom body":** This is the *right* framing for glassy and
  the hand-rolled layer already targets it (the chrome is a custom body; the OS provides the window).
  Under this framing the winners are, in order: **hand-rolled**, **egui-hybrid**, **Vello-hybrid** —
  the three that compose a custom body over glassy's own wgpu surface.

**Decision triggers (what would flip this to "adopt egui-hybrid"):** see [What would change the
decision](#what-would-change-the-decision). In short: if, after consolidating the interaction model,
we still log recurring chrome bugs over a sustained period, or if the chrome scope grows into a
full settings/config app with dozens of real form widgets, egui-hybrid becomes the better tradeoff.

---

## glassy's actual integration points (verified, not assumed)

The spike brief assumed a `src/gui/*` + `src/app/event_loop.rs` + `src/renderer/*` layout. After
syncing this worktree to latest `develop`, that is exactly the structure (the worktree had been
sitting on a stale ancestor commit; it is now current at `84f1d4b`). Key facts, read from source:

- **Stack:** `Cargo.toml` pins `winit = "0.30.13"`, `wgpu = "29.0.3"`, `cosmic-text = "0.19.0"`,
  Rust edition 2024. One `EventLoop`, one `Renderer` owning one wgpu `Device`/`Queue`/`Surface`.
- **Terminal grid (must stay):** `src/renderer/` — an instanced two-pass pipeline (one bg quad per
  cell, one textured quad per glyph) fed by a dynamic R8 mask atlas + small RGBA color atlas, with
  per-row persistent instance storage and damage-driven partial rebuilds. This is the fast path and
  the reason glassy is "ultra fast vs ghostty." Untouchable.
- **Chrome (the "toolkit" under review):** `src/gui/mod.rs` is a self-described "lightweight
  immediate-mode GUI core" that "owns NO GPU state … emits the three renderer primitives
  (`push_overlay_px`, `push_overlay_rrect_px`, `push_overlay_glyph_px`) and returns interaction
  results." Persistent interaction state (`pressed`/`focused` `WidgetId`, animation map) lives in
  `App` and is threaded in per frame via `Ui::new(...)` (`src/gui/widgets.rs`). It already has a
  full widget vocabulary: button, toggle, slider, segmented, dropdown, list, scrollbar, text field.
- **Overlay compositing:** `src/renderer/overlay.rs` pushes into two channels — `overlay_quads`
  (premultiplied translucent quads) and `overlay_text` (glyphs **and** SDF rounded-rects via
  `flags==3` single-radius / `flags==4` per-corner). These draw in two passes that run *after* the
  grid + images, so chrome always lands on top, in the same encoder, same surface, same device
  (`src/renderer/frame.rs::render`). Per-corner radii already exist precisely so the active tab can
  round only its top corners — the rounded-corner compositing the prompt worries about is *solved*,
  it just had bugs.
- **Aesthetic tokens already shipping:** `src/gui/mod.rs` defines `glass_body`, `glass_raised`,
  `glass_active_tab`, `glass_float`, `rail`, `hairline`, theme-derived alphas (E1/E2/E3 surface
  levels), and a `state_fill` hover rule. Translucency + rounded + edge-lit is implemented.
- **0%-idle is already correct:** `src/app/event_loop.rs::about_to_wait` runs `ControlFlow::Poll`
  *only* while `gui::any_unsettled(&self.gui_anims)` is true, stepping animations and dropping
  settled ones, then returns to `Wait`. The comment is explicit: "This is the ONLY case where we run
  `ControlFlow::Poll`; once everything settles we fall back to `Wait` (0% idle)." This is the exact
  property a toolkit would have to preserve — and most don't, for free.
- **Binary baseline:** built `cargo build --release` in this worktree → **12 MB** stripped
  (`size`: text 11.09 MB, data 0.45 MB). 77 dependency lines in `Cargo.toml`.

**Implication:** the work being considered for outsourcing is small, dependency-free, already hits
the aesthetic and the idle invariant, and already shares one device + one loop (because it *is* the
device and the loop). Any toolkit must match all four of those properties just to break even.

---

## Comparison matrix

See [`gui-toolkit-matrix.md`](./gui-toolkit-matrix.md) for the full table. The two decisive columns,
distilled:

| Toolkit | Shares glassy's wgpu device + winit loop? | 0% idle at rest? | Binary add (on 12 MB base) | Verdict |
|---|---|---|---|---|
| Hand-rolled (current) | YES — it *is* the stack | YES | 0 | **keep & fix** |
| egui | **YES** — exact wgpu-29/winit-0.30 pin, `CallbackTrait` shares device | YES (reactive) | ~2–4 MB | **strong fit** |
| Vello | **YES** — exact wgpu-29 pin, caller owns device; no widgets | YES (no loop) | ~2–5 MB | **strong fit (hybrid renderer)** |
| Iced | PARTIAL — needs git-master + winit fork (stable pins wgpu 27) | YES | ~4–8 MB | viable, version-blocked |
| Slint | PARTIAL — unstable API; **Wayland idle-CPU bug** | mostly (Wayland bug) | 2.6 / 20 MB | weak |
| Xilem | **NO** — owns loop, embed PR rejected, wgpu 28 | likely | 8–15 MB | weak |
| Floem | **NO** — Vello-only paint cx, wgpu 27 + winit fork | yes | n/a | weak (dep conflict) |
| Vizia | **NO** — OpenGL/Skia, not wgpu | yes | **Skia 10–30 MB** | not viable |
| Makepad | **NO** — own GPU + own windowing | unconfirmed | moderate | not viable |
| GPUI | **NO** — owns loop, no external-wgpu API | **NO (120 FPS)** | 5–15 MB | not viable |
| Freya | **NO** — Skia not wgpu, owns loop | plausible | **Skia 20–30 MB** | not viable |
| Dioxus-native/Blitz | PARTIAL — experimental, regressed; owns loop | plausible | 10–25 MB | revisit at 1.0 |

---

## Per-toolkit deep-dive

### egui (+ egui-wgpu + egui-winit) — the recommended hybrid target

- **Version / pin:** egui 0.34.3 (May 2025). `egui-wgpu` pins `wgpu ^29.0.1`, `egui-winit` pins
  `winit ^0.30.13` — an **exact match** to glassy's `wgpu = 29.0.3`, `winit = 0.30.13`. Use the raw
  `egui-winit` + `egui-wgpu` crates, **not** `eframe` (eframe owns the loop; raw crates plug into
  glassy's `ApplicationHandler`).
- **Embedding (the crux):** **YES, two patterns, both share one device.**
  - *Pattern B (recommended for glassy): chrome-on-top.* glassy keeps its own terminal render pass
    exactly as today, then runs egui's pass into the same `CommandEncoder`/surface afterward,
    compositing chrome over the grid. `egui_wgpu::RenderState` exposes the shared `Device`/`Queue` —
    no second device. This maps directly onto glassy's current "grid passes, then overlay passes"
    structure: egui's pass simply replaces the two overlay passes.
  - *Pattern A: grid-inside-callback.* `egui_wgpu::CallbackTrait::{prepare, paint}` lets you inject
    glassy's instanced grid pipeline into egui's render pass, sharing device/queue
    (`custom3d_wgpu` demo). Not needed for glassy (Pattern B is cleaner) but proves the device is
    genuinely shared.
  - Sources: docs.rs/egui-wgpu `CallbackTrait`/`RenderState`; egui repo `custom3d_wgpu.rs`;
    discussion #4583.
- **Idle:** reactive — `Context::request_repaint_after(dt)` drives `ControlFlow::WaitUntil`; truly
  0% at rest. One caveat: issue #4499 — egui-winit currently sets `repaint=true` on any window
  event, so mouse motion *outside* a focused window can cause repaints; trivially filtered in
  glassy's existing `CursorMoved` handler. Net: compatible with the 0%-idle invariant.
- **Binary/RAM/startup:** lightest wgpu GUI by reputation and by the numbers (egui native apps
  ~3–5 MB *standalone*; marginal add on glassy's already-wgpu stack ≈ 2–4 MB). RAM overhead ~30 MB
  for egui's buffers above wgpu. Startup ~200–300 ms standalone; for glassy wgpu init is already
  paid.
- **Vibe (the weak spot — candid):** egui is **flat and tool-like by default** (the "Rerun/game
  overlay" look). It *can* be made beautiful — `Visuals` gives rounded corners, custom colors,
  custom fonts; the `Painter` API draws arbitrary beziers/textures/gradients; frosted-glass blur is
  achievable via a custom wgpu `PaintCallback` Gaussian pass (proven: mxs.dev egui-wgpu blur demo,
  ~200–400 lines of shader). But that is exactly the bespoke work glassy already did. egui buys us
  layout + input plumbing + a widget set; it does **not** hand us the glass aesthetic for free.
- **Cross-platform:** mature on Linux (X11 + Wayland; Wayland transparency has quirks but glassy
  controls its own clear color), macOS, Windows. IME has had Linux/Wayland bugs (#5544, #7485) but
  glassy's terminal input is custom — IME only matters for tab-rename/settings fields.
- **License:** MIT/Apache. Clean.
- **Maturity:** one of the most-used Rust GUIs (Rerun and many tools ship it). Author very active.
- **Effort:** **MED.** Wiring egui-winit/egui-wgpu into the existing loop is days; version alignment
  is zero-friction; re-creating the *glass* chrome to today's quality (custom widgets + blur
  callback) is weeks. Net effort is comparable to a serious hardening pass on the hand-rolled layer,
  with the upside of inheriting layout/focus/tab-order plumbing and the downside of a new dep.
- **Verdict:** **strong fit and the approved migration target** — exact version match, genuine
  device sharing, true idle — gated only by the fact that its aesthetic is the part we'd have to
  rebuild anyway.

### Vello — the runner-up hybrid (renderer, not a toolkit)

- **Version / pin:** Vello 0.9.0 (May 2026), `wgpu ^29.0.3` — **exact match**; no winit dependency.
- **Embedding:** **YES.** `Renderer::new(&device, opts)` takes glassy's existing device;
  `render_to_texture(&device, &queue, &scene, &tex, &params)` renders a chrome `Scene` into an
  intermediate `Rgba8Unorm+STORAGE_BINDING` texture each frame. glassy then composites that texture
  over the terminal grid using wgpu's `TextureBlitter` (~100–150 lines you write). Caller-owns-the-
  device is the *designed* usage. (`render_to_surface`/`render_to_encoder` were removed in 0.5, so
  the composite step is yours — fine, glassy already controls compositing.)
- **Idle:** no loop of its own; 0% at rest, driven by glassy's `Wait`.
- **Binary:** ~2–5 MB (compute-shader bundle); first-frame shader compile adds ~50–200 ms (no
  precompiled-shader path yet).
- **Vibe:** excellent vector + glyph rendering (gradients, strokes, fills, image compositing). **But
  backdrop blur is not yet shipped** (active work, Dec 2025), and **Vello has zero widgets/layout/
  input** — it is a drawing API, so you keep glassy's `Ui`/`Anim`/hit-testing layer and just swap
  the *drawing* primitives from glassy's SDF rrect to Vello's richer ones.
- **License:** Apache-2.0.
- **Maturity:** alpha but steadily maintained, tracks wgpu monthly, powers Xilem.
- **Effort:** **LOW–MED.** This is the smallest-delta option: keep glassy's entire interaction layer,
  replace `push_overlay_rrect_px`/glyph emission with Vello scene-building, add a blit pass.
- **Verdict:** **strong fit as a hybrid renderer** — but it gives less than egui (no widgets) for
  similar integration work, so it ranks behind egui as a migration target. It is, however, the
  *closest* thing to "upgrade the hand-rolled layer's drawing quality without adopting a framework,"
  and it does not blow up the architecture.

### Iced

- wgpu/winit: **stable 0.14 pins wgpu 27** (would force glassy *down* a major version); master
  (0.15-dev, unpublished) pins wgpu 29 but via a **winit git fork**, conflicting with glassy's
  upstream `winit 0.30.13`. cosmic-text 0.19 on master matches.
- Embedding: `shader::Primitive` shares Iced's single `Engine` device (confirmed), and the
  `integration` example shows owning your own winit loop + wgpu and letting Iced draw chrome on top —
  architecturally sound. But the realistic path today requires a git-master dependency on an
  unstable API plus winit-fork reconciliation.
- Idle: reactive since 0.14 (PR #2662) — 0% at rest. License MIT. Shipping in libcosmic/COSMIC
  (Pop!_OS 24.04). Vibe: themeable, window blur on mac/Linux, but not cinematic.
- **Verdict:** viable in principle, **blocked by version drift** — the integration is fine, the
  dependency situation is not, today. Heavier binary than egui. If Iced 0.15 ships on crates.io with
  wgpu 29 + upstream winit, re-evaluate.

### Slint

- wgpu: `unstable-wgpu-29` matches glassy, but the API is explicitly outside Slint's stability
  guarantees and you must lock `~1.17`.
- Embedding: PARTIAL — `WGPUConfiguration::Manual` lets Slint borrow glassy's device, and
  `set_rendering_notifier(BeforeRendering)` lets glassy inject the grid pass before Slint's chrome
  (the `servo` example does this). But either Slint owns the winit loop (`run_event_loop`) or you
  implement the `Platform` trait to keep owning it — substantial re-architecture either way.
- Idle: mostly, **but a confirmed Wayland 100%-CPU-at-idle bug (#5780, closed as "not planned")** on
  glassy's primary target. Vibe: themeable but **no first-class Wayland backdrop blur**. Binary:
  FemtoVG ~2.6 MB (Skia ~20 MB — avoid).
- **License:** the Royalty-Free v2.0 license *does* permit an MIT/Apache app for free (GPL is **not**
  forced); attribution required (AboutSlint widget or a badge). So the much-feared license trap is
  navigable — but the Wayland idle bug + loop re-architecture + unstable wgpu API make it weak
  regardless.
- **Verdict:** **weak** — the license is survivable, but the engineering (loop surrender, unstable
  API, Wayland idle regression on our main platform) is not worth it.

### Xilem / Floem / Vizia (full toolkits that fail the embed test)

- **Xilem** (0.4.0): owns the winit loop; maintainer **rejected** the external-renderer embed PR
  (#879) and the custom-wgpu-texture widget is unimplemented (#1319/#395 open); pins wgpu 28
  (mismatch); pre-1.0, no shipping app. **NO** on embedding.
- **Floem** (0.2.0): `PaintCx` exposes only Vello drawing, no raw-wgpu injection; pins **wgpu 27 +
  a custom winit fork** — a hard dependency conflict with glassy's stack; would require running a
  second wgpu device (defeats the purpose) or patching Floem. Strong production proof (Lapce) but
  the dep math doesn't work. **NO.**
- **Vizia** (0.4.0): renders with **OpenGL/Skia, not wgpu** — cannot share a wgpu device at all; and
  Skia adds 10–30 MB, breaking the binary budget. **NO.**

### Makepad / GPUI / Freya / Dioxus-native (parallel-stack or heavyweight)

- **Makepad** (1.0.0): own GPU abstraction (MPSL → Metal/D3D/GL) and own windowing — **no
  winit, no wgpu**. Full parallel stack; terminal grid would need a rewrite in MPSL. Beautiful, but
  **NO.**
- **GPUI** (0.2.2): owns its event loop and renderer (Metal/D3D natively; Linux wgpu only since Feb
  2026), **no upstream external-wgpu API**, and renders at **120 FPS continuously** (fails 0%-idle).
  Zed-first priorities. Stunning, but **NO.**
- **Freya** (0.3.4 / 0.4-rc): winit-based but **Skia, not wgpu** → compositing mismatch + ~20–30 MB
  Skia binary (breaks budget); owns the loop; solo-maintained, no production app. **NO.**
- **Dioxus-native / Blitz** (0.3.0-alpha.6): the only one architecturally aligned (winit + wgpu via
  Vello), but **pre-alpha**, owns the loop, the wgpu-texture-embed path is experimental and currently
  regressed between alphas, and the HTML/CSS (Stylo) layout model is the wrong abstraction for
  terminal chrome. **Revisit at 1.0**, not now.

---

## Integration sketch for the recommended path

### Path 0 (the recommendation): keep & fix the hand-rolled layer

The recurring bugs are interaction-model bugs, not rendering bugs. The durable fix is to *converge
the interaction model*, not to swap renderers:

1. **Single dismiss/damage model for all overlays.** Every modal/menu/popup should route through one
   "overlay session" abstraction that owns: the scrim, click-outside-to-dismiss, motion handling
   (the dismiss-on-motion bug came from menus reacting to motion they shouldn't), and the
   `force_full_redraw` contract documented in `overlay.rs` (the area under glass must be repainted
   this frame). Centralizing this kills the whole bug *class*.
2. **One hit-test/layout pass shared between draw and input.** Tab-rect/DPI glitches come from
   layout math duplicated between the painter and the click handler. Compute rects once
   (physical px, the `Rect`/`hit` helpers already exist in `gui/mod.rs`), store them, and have both
   draw and hit-test read the same rects.
3. **Keep the `Anim` + `Poll`-only-while-unsettled idle machinery** — it is correct and is the thing
   a toolkit would risk regressing.
4. Effort: low-to-medium and *fully removes a dependency-adoption risk*.

### Path 1 (escape hatch): egui-hybrid (chrome only; keep the wgpu grid)

If Path 0's bug rate stays unacceptable, migrate the chrome — and only the chrome — to egui:

- **Keep entirely:** `src/renderer/` cell-grid pipeline, atlas, damage tracking, images; `src/pty.rs`;
  `src/app/{event_loop,input,keys,mouse,selection,panes,multipane,tabs/*}.rs` *logic* (tab/pane/
  session state, keybindings, mouse reporting, selection). These are not GUI — they are terminal
  semantics and stay.
- **Replace:** `src/gui/*` (the immediate-mode widget toolkit) and the *chrome-painting* parts of
  `src/app/{chrome,settings,palette,search}.rs` + `src/app/render.rs`'s overlay paint calls. The
  `src/renderer/overlay.rs` overlay passes get retired in favor of egui's pass.
- **Wiring:**
  1. In `App::resumed`, after creating the wgpu device, build `egui_wgpu::Renderer` from the shared
     `RenderState` and an `egui_winit::State`.
  2. In `window_event`, forward events to `egui_winit::State::on_window_event`; respect its
     `EventResponse.consumed` to decide whether the terminal also sees the event (chrome gets first
     dibs, exactly like the current `strip_click`/overlay precedence).
  3. In `render` (`src/app/render.rs`): run the existing terminal grid passes first; then build the
     egui UI (tabs/settings/menus/help as egui widgets), tessellate, and run egui's pass into the
     same encoder/surface — composited on top.
  4. Drive idle with `Context::request_repaint_after` and keep glassy's `Wait`/`Poll` switch;
     filter the #4499 outside-window motion repaint in `CursorMoved`.
- **Aesthetic work (the real cost):** port the glass tokens (`glass_body`/`glass_raised`/`rail`/…)
  into an egui `Visuals` + a small set of custom `Widget`s, and port the SDF/blur look as a custom
  `PaintCallback` Gaussian pass behind floating panels.
- **Effort/risk:** MED effort, LOW architectural risk (device + loop genuinely shared), MED
  aesthetic risk (must rebuild the glass look). Reversible: the terminal core is untouched.

### Path 2 (alternative escape hatch): Vello-hybrid (upgrade drawing, keep interaction)

If the goal is "better-looking chrome with the *least* framework adoption," keep glassy's entire
`Ui`/`Anim`/hit-test/dismiss layer and swap only the drawing: build a Vello `Scene` from the same
widget code, `render_to_texture`, blit over the grid. Smaller conceptual change than egui, but you
keep maintaining the interaction layer yourself (so it does less to address the bug class than
Path 0's model-convergence does). Choose this only if the complaint is "looks," not "bugs."

---

## Risks and what would change the decision

### Risks of the recommendation (keep & fix)

- **The bug class might be deeper than interaction logic.** If, after converging the dismiss/damage
  and hit-test models, bugs persist, that is the signal to take the egui-hybrid escape hatch.
- **Scope creep.** If the settings/config surface grows into a real app (dozens of form widgets,
  text editing, validation), hand-rolling stops being cheap and egui's plumbing starts to pay off.
- **Maintainer bandwidth.** Hand-rolled means we own every pixel forever. That is acceptable while
  the chrome is small; it scales poorly.

### Risks of the egui escape hatch (so they're known in advance)

- **Aesthetic regression risk** — egui's default look is tool-like; hitting today's glass quality is
  real work (custom widgets + blur callback). Mitigation: prototype the glass `Visuals` + blur
  callback *before* committing.
- **The #4499 spurious-repaint bug** — minor, filterable, but must be handled to preserve 0%-idle.
- **A new dependency to track** across wgpu/winit bumps. egui's exact-match pin today is the best
  case; future wgpu majors require egui to keep pace (it historically does).

### What would change the decision

| If this becomes true… | …then switch to |
|---|---|
| Chrome interaction bugs keep recurring after the dismiss/damage + hit-test models are converged | **egui-hybrid** (Path 1) |
| Chrome scope grows into a full settings/config app needing many real form widgets + text editing | **egui-hybrid** (Path 1) |
| The complaint is purely visual (look), not buggy behavior, and we want richer drawing | **Vello-hybrid** (Path 2) |
| Iced 0.15 ships on crates.io with wgpu 29 + **upstream** winit (no fork) | re-evaluate **Iced** vs egui |
| Blitz reaches 1.0 with a stable shared-wgpu-device path | re-evaluate **Blitz** |
| Binary budget is relaxed and a true-native look becomes the priority | platform toolkits (out of current scope) |

A note on adversarial refutation of the *leading* recommendation: the strongest argument *against*
"keep & fix" is "we keep hitting bugs, so the approach is wrong." The refutation that survives is
that the bugs are concentrated in *interaction state and damage tracking* (dismiss-on-motion, pane
menu clamp, render deadlock, tab chrome, settings layout — see commits `238a68d` and `84f1d4b`),
which are *model* problems that a toolkit relocates rather than eliminates — and a toolkit that
could host our wgpu grid (egui) hands us layout/focus but *not* the glass aesthetic, so adopting it
now trades a known, dependency-free, already-pretty, already-idle layer for one where we redo the
pretty part and inherit a dependency. That trade is only worth it once the bug rate proves the model
can't be fixed in place — hence "fix first, with egui pre-vetted as the exit."

---

## Sources

Primary (glassy source, this worktree at `84f1d4b`): `Cargo.toml`; `src/gui/{mod,widgets,chrome,
help}.rs`; `src/renderer/{overlay,frame}.rs`; `src/app/{event_loop,render}.rs`; measured release
binary 12 MB via `cargo build --release` + `size`.

External (verified 2026-06-24):

- **egui:** github.com/emilk/egui (releases, Cargo.toml, ARCHITECTURE.md); docs.rs/egui-wgpu
  (`CallbackTrait`, `RenderState`); `custom3d_wgpu.rs`; discussions #4583, #2937; issues #4499,
  #5544, #7485; mxs.dev egui-wgpu blur demo; AN4T 2025 Rust GUI comparison; Kalbertodt 2023 perf
  benchmark; LICENSE-APACHE.
- **Vello:** crates.io/crates/vello (0.9.0, wgpu ^29.0.3 deps); docs.rs/vello (`Renderer`,
  `render_to_texture`); linebender.org blog (render_to_surface removal Feb 2025; blur progress Dec
  2025); vello `vision.md`.
- **Iced:** github.com/iced-rs/iced (releases; master Cargo.toml wgpu 29/cosmic-text 0.19; 0.14.0
  Cargo.toml wgpu 27); docs.rs/iced `widget::shader`; `wgpu/src/{primitive,engine}.rs`; examples
  `integration`, `custom_shader`; PR #2662 (reactive); window blur PRs #2728/#2900;
  window::Settings docs; libcosmic / Pop!_OS 24.04.
- **Slint:** github.com/slint-ui/slint (releases 1.17.0, CHANGELOG, FAQ); docs.slint.dev
  (backends/renderers, `RenderingState`, `FemtoVGWGPURenderer`, `wgpu_2x` modules, cargo features);
  examples `servo`, `opengl_underlay`; PR #8278; Royalty-Free License v2.0 + discussion #2706;
  idle-CPU issue #5780; blur discussion #5710 / issue #4121; memory discussion #3376.
- **Xilem/Floem/Vizia:** crates.io + raw Cargo.tomls (Xilem main wgpu 28; Floem main wgpu 27 +
  lapce/winit fork; Vizia main skia-safe 0.97, no wgpu); Xilem PR #879, issues #1319/#395; Floem
  `GpuResources`/`canvas`; Lapce v0.4.6; Vizia `application.rs` (ControlFlow::Wait); boringcactus
  2025 Rust GUI survey.
- **Makepad/GPUI/Freya/Blitz:** makepad.rs arch docs + HN 1.0 thread (MIT, own stack);
  crates.io/crates/gpui 0.2.2 + Zed PR #46758 (Linux wgpu Feb 2026) + discussion #45996 (no
  external-wgpu) + zed.dev rendering blog (120 FPS) + Apache-2.0; github.com/marc2332/freya +
  rust-skia binaries releases (19–24 MB Linux); blitz-shell 0.3.0-alpha.6 + roadmap #119 + Dioxus
  discussion #4362 / issue #4512 (wgpu-texture regressed).
