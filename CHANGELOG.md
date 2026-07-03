# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added (w14 wave)

#### Themes
- **42 new built-in themes** (60 total): 12 true light themes (GitHub Light, Solarized Light, One Half Light, Tokyo Night Day, Kanagawa Lotus, PaperColor Light, Modus Operandi, Flexoki Light, Vitesse Light, Dayfox, Selenized Light, Alabaster) and 30 dark staples (GitHub Dark, Monokai (+ Pro), Material (+ Darker), Night Owl, Snazzy, Horizon, Oceanic Next, Palenight, Zenburn, Iceberg, Nightfox, Vitesse Dark, Flexoki Dark, Everblush, Melange, Synthwave '84, Catppuccin Frappé, Tokyo Night Storm, Gruvbox Material, One Half Dark, Ayu Mirage, Rosé Pine Moon, Kanagawa Dragon, Solarized Osaka, Poimandres, Andromeda, Aura, Challenger Deep). Every palette verified against upstream sources.
- **Single-source theme registry**: one `BUILTIN_THEMES` table replaces four hand-maintained lookup functions; adding a theme is one entry.
- **User themes directory**: drop flat `key = value` theme files into `~/.config/glassy/themes/` — they appear in the theme dropdown, shadow same-named builtins, and need no rebuild.
- **Theme import formats**: `--import-theme` now reads iTerm2 `.itermcolors`, Kitty, and Ghostty configs in addition to Alacritty TOML and base16 YAML, dispatching on file extension.

#### Settings & profiles
- **Every config key is now in the settings UI**: new Effects, Terminal, Quake, Notifications, and Profiles sections (power mode, dim-unfocused, copy-as-HTML, quake geometry/animation, notification thresholds, command folding, hint chars, per-style font overrides, symbol map, font variations, status-bar segments/time format, per-side padding, wallpaper theme; shell/cwd shown read-only). Restart-only keys are labeled as such.
- **Profiles UI**: the active profile is highlighted, a "(default)" row switches back to the base config (previously impossible without a restart), and "duplicate current settings as a new profile" writes a `[profile.NAME]` section from the live config.
- **Scrollable dropdown popups**: long dropdowns (e.g. 60 themes) scroll instead of truncating.

#### Input & automation
- **File drag-and-drop**: dropped files paste as shell-quoted paths (multi-file drops coalesce into one bracketed paste); a subtle highlight shows while hovering.
- **Remote-control automation verbs**: `glassy @ list-themes`, `@ get-config <key>`, `@ set-config <key> <value>` (write-through), `@ reload-config`, `@ run-action <name>` — plugin system Phase 1 (see `docs/plugins.md`).
- **`select_all` action**: bindable everywhere; default ⌘A on macOS.
- **macOS menu bar**: Edit > Select All, View > font-size controls, Window > Minimize/Zoom, File > Close Tab.

### Fixed (w14 wave)
- **Light themes are legible**: raised panels/cards/active tab/dropdowns now darken on light backgrounds instead of additive-lightening into pure white; default `follow_system` light theme is now One Light.
- **Settings Save persists everything**: ~11 live-toggled settings (minimap, copy-on-select, cursor trail, title toggles, light/dark theme picks, word separator, font features, command badges) plus the custom-effect sliders no longer silently revert on restart; a coverage test keeps the save table complete.
- **Config writes no longer corrupt profiles**: saving a setting used to append it at end-of-file — inside `[profile.X]`/`[keybindings]` if one was last — silently turning a global setting into a profile-only override.
- **Theme import no longer silently imports Tokyo Night** for unrecognized files; parsers fail honestly and content-sniffing works.
- **Render CPU scales with damage**: the renderer no longer collects the entire visible grid every frame — only dirty rows are gathered (split panes get the allocation fix too); also fixes a latent ligature-run damage-attribution edge case.

### Added

#### Terminal protocol
- **Kitty keyboard protocol** levels 2–5: REPORT_EVENT_TYPES (release/repeat), REPORT_ALTERNATE_KEYS, REPORT_ALL_KEYS_AS_ESC (required by Helix/Neovim), REPORT_ASSOCIATED_TEXT. (Level 1 / DISAMBIGUATE_ESC_CODES was already present.)
- **modifyOtherKeys** (XTMODKEYS, `CSI > 4 ; N m`) levels 0–2: modified printable keys emit `CSI 27 ; mods ; code ~` as legacy TUIs expect.
- **Synchronized output** (DECSET/DECRST 2026): terminal output is buffered during `?2026h…?2026l` brackets and the UI wakes only once per completed frame, preventing mid-render paints.
- **OSC 7** shell CWD tracking: new tabs and pane splits inherit the shell's reported working directory.
- **OSC 9 / OSC 777** desktop notifications forwarded to the OS via `notify-rust`.
- **OSC 9;4** progress state: a subtle progress indicator rendered in the status bar when a running application reports progress.
- **OSC 52** clipboard read/write: applications can read and write the system clipboard via escape sequences.
- **OSC 133** shell-integration semantic marks (A/B/C/D prompt and command boundaries), enabling jump-to-prompt navigation.
- **Plain-text URL detection**: plain URLs in the grid are hoverable and `Ctrl+Click`-able, not just OSC 8 hyperlinks.

#### Split panes
- **Split panes**: `Ctrl+Shift+E` splits vertically (left | right), `Ctrl+Shift+O` splits horizontally (top / bottom); arbitrary recursive tiling.
- **Pane resize**: drag the divider gutter to resize; gutter shows hover feedback.
- **Pane focus**: `Alt+Arrow` moves focus between adjacent panes.
- **Pane headers**: per-pane title bar showing the shell's foreground process and working directory (sourced from `/proc` + OSC 7); includes a close box and a `⋮` split menu. Toggleable via `pane_headers` config key or `Ctrl+Shift+B` via the palette.
- **Close pane**: `Ctrl+Shift+W` closes the focused pane; falls back to closing the whole tab when only one pane remains.
- **Incremental split render**: each pane redraws only its own damage, not the whole surface.

#### Overlays and chrome
- **Command palette** (`Ctrl+Shift+P`): fuzzy-searchable list of every action and setting; type to filter, arrow/Enter to invoke. Covers tabs, panes, font, themes, scrollback, toggles.
- **In-terminal search** (`Ctrl+Shift+F`): regex find bar at the bottom, all-match highlighting in the viewport, `Enter`/`Shift+Enter` for next/prev, pre-fills from an active selection.
- **Real-GUI settings form** (`Ctrl+,`): extended with font family dropdown, scrollback stepper, status bar toggle, pane headers toggle; Tab/arrow navigation; saves to config file.
- **Help overlay** (`F1`): now scrollable; includes split-pane bindings, palette and search, window shortcuts.
- **Status bar** (`Ctrl+Shift+B`): optional bottom bar with OSC 9;4 progress indicator; off by default; toggled via config, CLI, or the command palette.

#### Tabs
- **Tab drag-reorder**: drag a tab chip to reorder tabs.
- **Tab rename**: double-click a tab chip to open an inline rename editor; Enter commits, Esc cancels; custom title overrides the OSC title.
- **Touchpad swipe**: a horizontal touchpad swipe over the tab bar cycles one tab per gesture.

#### Configuration and profiles
- **Config hot-reload**: glassy watches the config file with `notify` and applies changes without a restart.
- **Named profiles** (`[profile.NAME]` sections): activated at launch with `--profile NAME`; CLI flags still override the profile.
- **Per-side padding**: `padding_top`, `padding_bottom`, `padding_left`, `padding_right` override the uniform `padding`.
- **`follow_system`**: tracks the OS light/dark color scheme; `theme_light` / `theme_dark` pick the theme per mode.
- **`restore_session`**: persists tabs, pane layouts, and per-pane cwds to `$XDG_STATE_HOME/glassy/session.json`; restored on next launch when the key is set.
- **`word_separator`**: additional characters treated as word boundaries during double-click selection.
- **`cwd`**: initial working directory for the first tab's shell.
- **`status_bar`** and **`pane_headers`** config keys.
- **`--import-theme <path>`**: load an Alacritty TOML or base16 YAML color theme at startup.
- **Custom color overrides**: `color.fg`, `color.bg`, `color.cursor`, `color.selection_bg`, `color.ansi0`–`color.ansi15` override any named theme's colors in-place.

#### Text and font
- **Ligature shaping**: opt-in (`ligatures = true`) OpenType GSUB `liga` shaping across full cell runs.
- **`font_features`**: force-enable or disable individual OpenType feature tags (e.g. `ss01, calt=0`).
- **Procedural Powerline glyphs** (`E0B0`–`E0B3`): rendered as pixel-perfect filled polygons via the Nerd Font default font, gap-free at all sizes.
- **Nerd Font wide-icon promotion**: single-codepoint Nerd Font icons that are logically wide are promoted to two-cell width.

#### Images
- **Inline images** — kitty graphics protocol (PNG incl. 8/16-bit and palette, raw RGBA, chunked `f=` transfers, `c=`/`r=` cell sizing, aspect-aware, `a=d` delete) and **sixel**, drawn on a dedicated GPU atlas; images clear on screen-clear / reset.

#### Themes
- Added **Rosé Pine Dawn** (light) and **Catppuccin Latte** (light): 10 built-in themes total, including two light themes.

#### Packaging and distribution
- **Debian / Ubuntu `.deb`**, **Fedora / RHEL / openSUSE `.rpm`**, **Arch AUR** (`glassy` and `glassy-bin`), **macOS `.dmg`** (universal binary), **Flatpak** manifest, **Homebrew tap** skeleton.
- **`curl | bash` installer** (`scripts/install.sh`): downloads the latest binary, verifies SHA-256, installs to `~/.local/bin`.
- **Release CI**: GitHub Actions publishes all package artifacts on tag.

#### Performance
- Allocation-free redraw path: direct glyph-instance push, persistent flush-pass scratch, removed a redundant glyph-cache layer.
- Skip default-background cell quads; fewer GPU state rebinds for the image pass.
- Dropped the `image` and `regex` crate stacks (smaller binary).
- **iGPU by default**: renderer selects the low-power (integrated) GPU adapter; override with `GLASSY_GPU=high`.

### Changed

- glassy now owns its PTY read loop (taps image/OSC/protocol sequences before alacritty_terminal sees them).
- Softer, accent-tinted visual bell (was a stark white full-screen flash).
- Narrow-base emoji (e.g. the trans flag) render at full size.
- The settings overlay uses an immediate-mode GUI layer (`src/gui/`) with animated hover/toggle feedback and real keyboard navigation.
- Tab bar activity dots and busy spinner animate only while a background tab is producing output; the event loop parks at `ControlFlow::Wait` (0% idle) otherwise.

---

## [0.1.0] - 2026-06-19

Initial release.

### Added

- GPU-accelerated rendering: an instanced `wgpu` renderer fed by a dynamic
  glyph atlas, with on-demand, damage-based redraw to stay idle when nothing
  changes.
- 24-bit truecolor and 256-color support.
- Color emoji rendering with CJK font fallback.
- Procedural box-drawing characters for crisp, gap-free lines.
- Text decorations: underline, double, curly, dotted, dashed, strikethrough,
  SGR 58 colored underlines. Cursor shapes (block / bar / underline) and blink.
- Mouse support: SGR reporting, text selection, and clipboard copy/paste.
  Scrollback buffer.
- Tabs with a slim title bar and scrollback indicator.
- OSC 8 hyperlinks (`Ctrl+Click` to open).
- Inline images: kitty graphics protocol (PNG, raw RGBA) and sixel.
- In-app settings overlay (`Ctrl+,`): font size, opacity, bell, theme.
- Help overlay (`F1`): keybinding cheat-sheet.
- 8 built-in themes live-switchable: Tokyo Night, Catppuccin Mocha/Macchiato,
  Gruvbox Dark, Dracula, Nord, Solarized Dark, Rosé Pine.
- Configurable window opacity, decorations, cursor.
- Terminal bell: visual flash; optional audible beep (`bell-audio` build feature).
- Configuration file (`KEY=VALUE`) with theming support.
- Kitty keyboard protocol level 1 (DISAMBIGUATE_ESC_CODES).
- DECCKM application cursor-key mode.

[Unreleased]: https://github.com/alliecatowo/glassy/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/alliecatowo/glassy/releases/tag/v0.1.0
