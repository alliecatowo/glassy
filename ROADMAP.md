# Roadmap

Where glassy is headed, grouped by how settled the work is. **Now** is in
flight this wave and should land as-is. **Next** is scoped and likely, but
not started. **Later** is directionally right but not scoped. **Ideas** are
things worth wanting, not yet things worth promising.

This is a snapshot, not a contract — see [CHANGELOG.md](CHANGELOG.md) for
what's actually shipped. The w14 wave (theme registry + 42 themes, full
settings exposure, profiles UI, per-dirty-row rendering, drag-and-drop,
macOS menu, automation IPC) is in the changelog now, not here.

---

## Now (w15 wave, in flight)

### Split view and glass correctness

- **Unfocused-pane dim actually dims now.** The dim overlay was drawn
  through the blend-less bg pipeline, which *replaced* the pane's pixels
  with `rgba(0,0,0,0.28)` — erasing glyphs and punching the framebuffer
  alpha down so compositors rendered the pane as a see-through hole. It now
  composites in the premultiplied overlay layer, with a new `unfocused_dim`
  strength key (0–0.9, live-settable).
- **`opacity_scope = background | text`.** Window opacity historically only
  applied to backgrounds (text composites opaque for crispness). The new
  `text` scope folds opacity into terminal foreground colors too — glyphs,
  box/block/powerline cells, underlines — for the full-glass look, as an
  explicit opt-in. Chrome text and the cursor stay opaque.
- **Persistent row-line artifact fix.** When the cursor/IME-preedit row was
  skipped by the per-row content loop, the overlay fallback called
  `begin_row` — which wipes the row's cached instances — and repopulated
  nothing, leaving a blank row (or a bare cursor bar) that survived until
  unrelated output re-dirtied it. Effects made it more likely only by
  forcing more frames. The fallback now never destroys content it won't
  rebuild.

### Following the desktop

- **Live GNOME/Linux light↔dark follow.** winit never delivers
  `ThemeChanged` on Linux and `Window::theme()` doesn't reflect GNOME's
  preference, so `follow_system` silently resolved to `theme_dark` there.
  A watcher thread now reads and subscribes to the XDG desktop portal's
  `color-scheme` setting over D-Bus — using the `dbus` crate that
  notify-rust already links, so zero new dependencies — and feeds the
  existing live theme-apply path, at startup and on every switch.
- **Own the window chrome everywhere (kill the white border).** glassy
  never draws a window-edge border — the thin light line (and the
  system-themed title bar that ignores glassy's theme) is the OS/WM
  client-side decoration, since decorations were only ever disabled in
  quake mode. This wave goes borderless on all platforms: glassy's own
  chrome becomes the only chrome, with window drag extended beyond macOS,
  edge drag-resize, and close/min/max controls in the top bar on
  Linux/Windows. A `decorations = true` escape hatch stays for people who
  want their WM's frame back.

### Status bar as the first plugin surface

- **Data-driven segments.** The 12 hand-coded match-arm segments in
  `paint_status_bar` become a declarative segment table, plus new built-ins
  (tab count, pane zoom, active profile, shell-integration busy state,
  hostname) that all come from data glassy already has.
- **`glassy @ set-segment` / `clear-segment`.** External scripts can push
  custom status-bar segments over the existing control socket — the first
  real Phase-1 plugin surface from [docs/plugins.md](docs/plugins.md).

### Command palette power

- **Quake mode is discoverable.** The palette only registered its quake
  entry when the window already *was* the quake window, and the F12 bind
  was silently inert otherwise — the exact "I can't find quake anywhere"
  trap. Palette entries now exist unconditionally, and an inert F12 gets an
  explanatory toast instead of nothing.
- **Run-command scratchpad.** `>` or `$`-prefixed palette input runs the
  command in a transient PTY session that shows output and closes on
  keypress — a quick one-off runner without committing a whole tab.

### Panes

- **Headers stop taxing the grid.** Pane headers cost 22px of PTY rows per
  pane, which is why nobody enables them; they become an overlay strip
  (no grid theft), with a `compact` style, an optional single-pane header,
  a pane-index ordinal, and clamped cwd/command annotations.
  (Drag-to-resize dividers and pane swap already exist and stay as-is.)
- **Layout preset cycle.** A `cycle_layout` action steps the current tab's
  split tree through presets (rows / columns / main-vertical / grid),
  kitty-style, preserving pane order.

### Command blocks (Warp-adjacent, the glassy way)

- **Prompt-aware output selection** (select a command's whole output as one
  unit), **OSC 633 support** as a second mark source alongside the existing
  OSC 133 stack, and an opt-in **`command_blocks = cards`** presentation
  that draws subtle glass bands behind completed commands — presentation
  over the existing CommandBlock data, no new plumbing.

### Memory, robustness, benchmarks

- **Scrollback memory bounding** without touching alacritty's Term/Grid
  internals (the config allows 1M lines/pane; every pane keeps a fully
  resident grid today), plus a FairMutex `lock()` vs `lock_unfair()` audit
  on the render hot path.
- **Config save integrity.** Saves preserve inline trailing comments, save
  failures surface as toasts instead of log lines, and a save that races a
  fresh external edit re-reads and re-applies instead of clobbering it.
  GPU `device_lost` gets a graceful-rebuild callback.
- **A real benchmark suite.** A `[lib]` target so internals are
  bench-reachable at all, criterion micro-benches for the hot paths
  (`collect_display_row`, config parse, theme lookup, damage reads), a
  self-checking `scripts/bench.sh` vtebench harness for glassy vs alacritty
  vs ghostty, honest numbers in [docs/benchmarks.md](docs/benchmarks.md),
  and the unused `serde` feature dropped from alacritty_terminal.

### GUI design system (from the w15 design audit)

The audit found a good token/animation foundation that only ~40% of
surfaces use: two parallel design systems, 9+ corner radii, hand-rolled
card colors that break on light themes (toasts, peek), an invisible
pane divider on light themes, zero shadows anywhere, hover states that
never fade outside `Ui` widgets, a focus ring that overpaints accent
buttons, and a stale 20-item keyboard tab order for an 11-section
settings window. This wave lands the fix in phases: correctness (light
themes, focus ring, single luma), token adoption (spacing/radius/elevation
scales, one menu implementation), motion (duration+easing on the existing
event-driven `Anim`, hover fades everywhere, enter animations), and depth
(a soft-shadow SDF primitive under floating surfaces) — all without
touching the 0%-idle `ControlFlow::Wait` invariant. Settings rows for
every new w15 key land alongside, and `chrome.rs` / `settings_panel.rs`
get split by responsibility at the end of the wave.

---

## Next

- **Criterion CI job with saved baselines.** The bench suite lands this
  wave; wiring a ubuntu-runner regression gate (workflow_dispatch or
  nightly, not per-PR) is the follow-up.
- **Per-row damage for split panes.** Multipane still rebuilds a changed
  pane fully (deliberately, to keep the single-pane fast path simple);
  worth revisiting now that per-dirty-row collection has settled.
- **In-app keybinding rebind UI.** The Keys settings section is
  display-only today.
- **Window fade-on-blur.** Optionally dim/fade the whole window when it
  loses focus (kitty/wezterm have this) — distinct from per-pane dim, and
  cheap now that the dim path composites correctly.
- **Tab tear-off / New Window.** Dragging a tab out into its own OS window
  (kitty/wezterm) — gated on the multi-window scoping pass under **Later**,
  but called out here because it keeps coming up.
- **Import-theme writes into the user themes dir**, **theme dir
  hot-reload**, **settings search**, **color-picker widget**,
  **pane-at-drop-position routing**, **scrollback search perf** — all
  carried from w14, still right, still next.
- **Drag-and-drop preview.** A small thumbnail/name chip while a file drag
  hovers, before drop (wezterm shows one; we show a plain overlay today).
- **Command snippet library.** Saved, named, parameterized commands
  surfaced through the palette (Warp's "workflows") — the palette registry
  and the run-command scratchpad landing this wave are the substrate.
- **Windows CI target promotion** — unchanged from w14: not a supported
  platform until the release job is green and tested.

---

## Later

- **Multi-window support.** Still the prerequisite for tab tear-off and a
  true macOS "New Window"; needs a real scoping pass (window-to-instance
  model, single-instance socket implications).
- **Alacritty-replacement doctrine (from the w15 usage audit).** glassy
  uses alacritty_terminal 0.26 across 31 files; the PTY event loop is
  already in-house, and VT gaps are patched by byte-stream scanning rather
  than forking. The standing position: **never** rewrite the vte parser /
  Handler, `tty` spawn, Selection, or RegexSearch (large, fuzzed,
  correctness-critical, not bottlenecks). Do consider, in order and only
  with measurements in hand: (1) scrollback memory bounding — landing this
  wave; (2) a combined-damage layer *on top of* TermDamage if profiling
  shows damage merging matters; (3) a page-based scrollback store with
  style dedup (ghostty-style, ~12 vs ~24 bytes/cell) only if real-world
  memory numbers demand it. The `OverlineTracker` side-table pattern is the
  standard mechanism for new per-cell attributes without forking.
- **True row elision.** The prerequisite for real block-collapse and a
  Warp-style transient prompt: rendering a scrollback where folded rows
  occupy zero height. Explicitly flagged in-code as unbuilt; the
  `command_blocks = cards` work this wave is presentation-only and does not
  attempt it.
- **tmux positioning.** Running tmux *inside* a pane is the supported path;
  a tmux control-mode (`-CC`) integration is deliberately out of core. If
  demand materializes it belongs behind the plugin layer, not in the
  renderer. Session persistence beyond layout restore (true
  detach-and-keep-running) would require a daemon architecture glassy does
  not have and is not currently worth it.
- **SSH/remote domains.** A wezterm-style "local render, remote shell"
  domain abstraction. Big, real, unscoped.
- **Plugin Phase 2 — config-declared event hooks**, **global menu/DBusMenu
  on Linux**, **large-image streaming**, **shared I/O reactor** — unchanged
  from w14.

---

## Ideas

Not scoped, not promised — things worth having a position on:

- **Workspaces / session concept spanning windows** (kitty os-windows,
  wezterm workspaces) — only meaningful after multi-window.
- **Auto-update / version check.** ghostty and wezterm ship one; glassy
  installs via Homebrew/package managers today, so a low-key "new version
  available" toast (opt-in, no self-update) is the most this should ever be.
- **Plugin Phase 3 — WASM component plugins**, **ligature-aware
  selection**, **minimap zoom**, **session sync across machines** —
  unchanged from w14.
