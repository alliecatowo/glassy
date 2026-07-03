# Roadmap

Where glassy is headed, grouped by how settled the work is. **Now** is in
flight this wave and should land as-is. **Next** is scoped and likely, but
not started. **Later** is directionally right but not scoped. **Ideas** are
things worth wanting, not yet things worth promising.

This is a snapshot, not a contract — see [CHANGELOG.md](CHANGELOG.md) for
what's actually shipped.

---

## Now (w14 wave, in flight)

### Themes and chrome

- **Single-source theme registry + user themes dir.** Themes currently
  resolve through a hardcoded `theme_by_name` lookup in `src/color.rs`.
  This wave consolidates built-ins and user themes behind one registry that
  also checks `~/.config/glassy/themes/` before falling back to the
  built-ins — drop a theme file there and `theme = <name>` picks it up, no
  rebuild required. Details in [docs/plugins.md](docs/plugins.md#install-a-theme-without-code-the-user-themes-dir).
- **~40 new built-in themes, including true light themes.** Today's 18
  (12 dark, 6 light) grow substantially, with more attention paid to light
  variants specifically — see the chrome fix below for why that matters.
- **Light-mode chrome fix.** The glass chrome fills (`glass_body`,
  `glass_raised`, `glass_active_tab`, `glass_float` in `src/gui/mod.rs`) are
  built by additively lightening the theme background (`lighten(bg, 0.12)`,
  `0.22`, etc.). On a near-white light-theme background that math clips at
  white, so raised surfaces, active-tab chips, and floating panels all
  collapse to the same flat white and lose their separation. The fix changes
  how those surfaces derive their fill on light backgrounds so the layering
  survives near-white themes instead of washing out.

### Settings

- **Save no longer drops live-toggled keys.** `save_settings` in
  `src/app/settings.rs` currently writes a fixed, hand-maintained list of
  ~17 keys (`font_size`, `opacity`, `theme`, `cursor_style`, …) to
  `glassy.conf`. Config has 40+ fields, and several of them — reachable and
  live-previewable from the settings overlay — quietly weren't in that list,
  so toggling them and hitting Save didn't persist the change. This wave
  replaces the fixed list with a declarative `SAVED_KEYS` table so "settable
  live" and "saved on Save" can't drift apart again; ~11 keys that were
  silently dropped are now written.
- **Every config key gets a settings-UI home.** The overlay's sections today
  are General / Appearance / Themes / Keys / Panes / Advanced
  (`SettingsSection` in `src/gui/settings_panel.rs`); Effects already exists
  as a heading tucked inside Appearance (minimap toggle, window-effect
  dropdown, custom sliders). Quake (`quake`, `quake_height`,
  `quake_animation_ms`) and notifications (`notify_command_finish`,
  `notify_command_threshold_ms`) have no settings UI at all today —
  config-file-only. This wave promotes Effects to its own top-level section
  and adds new Terminal, Quake, and Notifications sections so those
  previously config-file-only keys get a UI home too.
- **Profiles UI gets an active indicator and duplicate-as-profile.** The
  Advanced section already lists `[profile.NAME]` sections as clickable rows
  (`RowKind::Profile`, wired to `profile_pick`), but doesn't currently
  highlight which profile is active or offer a one-click way to fork the
  current settings into a new named profile. Both land this wave.

### Rendering and panes

- **Per-dirty-row cell collection.** The single-pane render path
  (`src/app/render.rs`) walks every visible cell in the grid each frame and
  checks a per-row `dirty[]` flag to decide whether to push it — the skip is
  cheap, but the walk itself is still O(total cells), not O(dirty cells).
  This wave changes collection to iterate only the dirty rows' ranges
  directly.
- **Multipane allocation fix.** The split-pane render path
  (`src/app/multipane.rs`) currently `.collect()`s a fresh `Vec` of the
  entire grid's display cells per pane, per frame (and a second `Vec` for
  live pane IDs) — this wave removes the avoidable per-frame allocations.
  Note this is independent of per-row damage for splits, which multipane
  still doesn't have — see **Next**.

### Input and files

- **File drag-and-drop.** Not implemented at all today (no `DroppedFile`
  handling anywhere in the event loop) — this wave adds it: dropped paths
  are shell-quoted before insertion, multi-file drops batch into one paste,
  and the paste goes through bracketed-paste so shells and TUIs that care
  don't misinterpret it as typed input.

### macOS

- **Menu bar expansion.** The current `NSMenu` (`src/app/mac_menu.rs`) covers
  the essentials (New Tab, Split, Copy/Paste, Find, Palette, Fullscreen, Zoom
  Pane, tab navigation, Quit) but is missing **Select All**, **font
  size** (in/out/reset), and the standard **Minimize** / **Zoom** window
  controls under the Window menu. This wave adds them so the menu bar covers
  what a native macOS app is expected to.

### Theme import

- **New formats: iTerm2 (`.itermcolors`), Kitty, Ghostty.** Import currently
  covers Alacritty-compatible TOML and base16 YAML
  (`src/config/theme_import.rs`); `.itermcolors` is actually an XML property
  list (not YAML, despite what the current doc comment implies), so it needs
  its own parser, as do Kitty's and Ghostty's own theme file formats.
- **Silent-default bugfix.** An unresolvable theme name
  (`src/config/parse.rs`) already logs a warning and falls back to Tokyo
  Night, but a `log::warn!` line most users never see is effectively silent
  in practice — you get the wrong theme with no visible signal why. This
  wave surfaces the fallback somewhere a user will actually notice it.

### Automation (plugin Phase 1)

- **Extended `glassy @` remote control + the user themes dir above** are
  Phase 1 of glassy's plugin story: script glassy from the outside via
  `get-config` / `set-config` / `list-themes` / `reload-config` /
  `run-action`, no code loaded into the process. Full writeup, including why
  this explicitly isn't a plugin runtime, in
  **[docs/plugins.md](docs/plugins.md)**.

---

## Next

- **Per-row damage for split panes.** Multipane's render loop
  (`src/app/multipane.rs`) still rebuilds the entire visible pane every
  frame — deliberately, per its own comment, to keep the fast single-pane
  path untouched while splits stay a less-optimized secondary path. Worth
  revisiting once the single-pane per-dirty-row work above has settled.
- **In-app keybinding rebind UI.** The Keys settings section is display-only
  today; you can see your bindings but changing one still means hand-editing
  `glassy.conf`.
- **Import-theme writes into the user themes dir.** `--import-theme` parses
  a theme file and applies it for that session; it doesn't yet drop a copy
  into `~/.config/glassy/themes/` so it shows up in `list-themes` /
  the theme picker on the next launch without re-importing.
- **Theme dir hot-reload.** The config file already hot-reloads
  (`notify`-watched); `~/.config/glassy/themes/` doesn't yet get the same
  treatment, so a new or edited theme file needs a restart (or `glassy @
  reload-config`, once that lands) to be picked up.
- **Settings search.** The overlay's sections are browsable but not
  searchable — useful once Terminal/Effects/Quake/Notifications push the
  section count up.
- **Color-picker widget for the custom-theme editor.** Custom colors are
  currently edited as a swatch + hex input; a real HSV/wheel picker is a
  natural next step.
- **Pane-at-drop-position routing for drag-drop.** This wave's file
  drag-and-drop always targets the focused pane; routing to whichever pane
  the cursor is actually over when a file is dropped is a follow-up.
- **Scrollback search perf.** In-terminal search (`Ctrl+Shift+F`) works
  today; making it fast on very large scrollback buffers is unaddressed.
- **Windows CI target promotion.** `.github/workflows/release.yml` already
  has a `build-windows` job, but it's `continue-on-error: true` and labeled
  "TODO: not yet green" — Vulkan SDK / fontconfig-windows deps aren't wired
  up yet, and the CI test matrix (`ci.yml`) doesn't run on Windows at all
  (`ubuntu-latest` + `macos-latest` only). Promoting Windows to a required,
  tested target is gated on it actually being a supported platform, which it
  isn't yet (see [README.md](README.md) install matrix: macOS + Linux only).

---

## Later

- **Multi-window support.** glassy is single-window today (tabs and splits
  live inside one OS window); this blocks a true "New Window" macOS menu
  item, among other things. A real scoping pass (window-to-instance model,
  IPC implications for the existing single-instance socket) needs to happen
  before this is a "Next," not a "Later."
- **Plugin Phase 2 — config-declared event hooks.** `[hooks]` section
  (`on_command_finish`, `on_theme_change`, `on_tab_open`, …) shelling out to
  user commands with substitution, no embedded interpreter. Scoped in
  [docs/plugins.md](docs/plugins.md#phase-2--config-declared-hooks-next).
- **Global menu / DBusMenu on Linux desktops.** macOS gets a native menu bar
  (`src/app/mac_menu.rs`); an equivalent global-menu integration for
  GNOME/KDE via DBusMenu doesn't exist and isn't scoped.
- **Sixel/kitty-image large-image streaming.** The OSC 1337 path is capped
  by the 1 MiB observation buffer (`src/image/store.rs`), which covers
  typical `imgcat`-sized icons/screenshots but not very large inline images;
  kitty/sixel remain the path for those today, but true streaming support
  for large images isn't designed yet.
- **Shared I/O reactor**, if hundreds-of-panes-per-window ever becomes an
  actual target. Today it's one OS thread per PTY (with a capped 256 KiB
  stack, see [docs/benchmarks.md](docs/benchmarks.md)), which is fine at the
  scale glassy is actually used at (a handful of tabs/splits) and not worth
  the complexity of a shared reactor until that stops being true.

---

## Ideas

Not scoped, not promised — things worth having a position on:

- **Plugin Phase 3 — WASM component plugins.** `wasmtime`/`wasmi` behind an
  opt-in Cargo feature, with read access to damage/cell events and an
  overlay-primitive push surface. Explicitly flagged as a major departure
  from the ~10 MB stripped-binary target, so it stays opt-in rather than
  default, and stays speculative until Phase 1/2 usage data says it's worth
  designing in detail. See
  [docs/plugins.md](docs/plugins.md#phase-3--wasm-component-plugins-speculative).
- **Ligature-aware selection.** Selecting inside a shaped ligature run
  currently operates on the underlying cell grid, not the shaped glyph runs;
  making selection ligature-aware is a real but fiddly correctness question.
- **GPU-accelerated scrollback minimap zoom.** The minimap (`minimap` config
  key, `src/app/minimap.rs`) already renders an incrementally-cached,
  downsampled overview strip of the whole buffer without breaking the
  0%-idle invariant; it has no zoom interaction today. A zoomable minimap is
  an idea, not a plan.
- **Session sync across machines.** `restore_session` persists tabs/splits/
  cwds locally (`$XDG_STATE_HOME/glassy/session.json`); syncing that across
  machines is a different, much bigger feature with real design questions
  (transport, auth, conflict resolution) that haven't been thought through.
