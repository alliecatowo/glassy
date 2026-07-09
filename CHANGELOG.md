# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!--
Process: new entries land under `## [Unreleased]` (no version number) as they
land, not under a pre-guessed version. Only at the moment a release is
actually cut does that header get rewritten to `## [x.y.z] - YYYY-MM-DD` (the
real version + the real date, together, in the same version-bump commit) —
and a fresh empty `## [Unreleased]` goes in above it for whatever's next. This
is what keeps a header from being stuck saying "Unreleased" long after it
shipped.
-->

## [Unreleased]

### Fixed
- **Linux aarch64 release builds actually succeed now**, instead of silently failing best-effort on every release. The cross-compile container was missing `libdbus-1-dev` (needed by `notify-rust`'s `libdbus-sys`), and separately `pkg-config` refuses to run cross-compiled at all unless told to — both are now handled (`Cross.toml`, `PKG_CONFIG_ALLOW_CROSS`). Verified with a full local cross-build, not just inferred from the error message.

## [0.6.1] - 2026-07-08

### Fixed
- **macOS traffic-light window buttons rendered abnormally small in released builds** (not `cargo build` locally). The release CI runner's default-selected Xcode links the binary against an older macOS SDK than what's installed locally, and macOS's linked-SDK compatibility checks make an app linked against that older SDK use legacy-sized title-bar chrome on newer macOS versions, even though nothing in glassy's own window setup changed. `release.yml` now pins `xcode-version: latest-stable` (`maxim-lobanov/setup-xcode`) so the macOS build job links against the newest SDK actually available on the runner instead of its stale default.

## [0.6.0] - 2026-07-08

### Added

#### Keyboard and input
- **Natural text editing** in the legacy (non-kitty) input path, matching mainstream terminals so word/line motion works at a bare shell prompt with no shell config: `Opt+←/→` word back/forward (`ESC b`/`ESC f`), `Cmd+←/→` line start/end (`^A`/`^E`), `Cmd+Backspace` delete-to-line-start (`^U`), `Opt+Backspace` backward-kill-word, `Ctrl+←/→` word motion. Suppressed when the kitty keyboard protocol is active so full-screen apps still receive raw key events.
- **Shift+Enter** emits the modifyOtherKeys form `CSI 27 ; 2 ; 13 ~`, keeping it distinct from a submitting Enter, so prompts that recognise it (e.g. Claude Code) insert a newline instead of running the line.

#### Settings
- **Every w15 config key now has a settings-panel row**, not just a config-file entry: unfocused-pane dim strength (Panes > Focus slider), `opacity_scope` (Appearance, Background/Text segmented), `command_blocks` chrome level (Effects, Off/Badges/Cards segmented), pane header style + single-pane header (Panes, next to the existing Pane headers toggle), and the background-scrollback cap/idle-threshold steppers (Advanced). All round-trip through Save exactly like the settings that already had rows.
- **Rename and delete profiles** from Settings > Profiles: each profile row now carries inline **Rename** (edit in place, Enter to save) and **Delete** (two-click confirm) affordances. Deleting the active profile falls back to the base config cleanly.
- **Quake mode discoverability**: pressing the `quake_toggle` key (default F12) while quake mode is off now shows a toast pointing you at Settings > Quake instead of doing nothing, and the Settings > Quake section notes that Wayland users should bind a compositor key to `glassy toggle` (see `docs/quake-mode.md`). The command palette also always lists a Quake entry now, regardless of mode.

#### Menus
- **The ≡ hamburger menu is now the single home for app actions.** It expands from a two-item stub into a grouped menu — New tab / Split right / Split down · Command palette / Settings / Help · Toggle quake · About · Close tab — and the standalone `?` (Help) and `⚙` (Settings) top-right buttons were folded INTO it, leaving `+` and `≡` as the only standalone top-right buttons. Adds an **About** entry showing the version and repo (opens the Help overlay, which now leads with them).

#### Terminal and appearance
- **Borderless window with client-side decorations everywhere.** glassy now goes borderless on all platforms (not just quake mode), so its own themed top chrome is the only chrome — killing the thin OS/WM window border and the system-themed title bar that ignored the glassy theme. Empty top-chrome areas drag the window (Linux/Windows too now, not just macOS); window edges/corners resize it; and non-macOS builds gain minimize / maximize / close controls in the top-right (macOS keeps its native traffic lights). New **`decorations = true`** (default `false`) escape hatch restores the native OS frame.
- **Live system light/dark following on Linux/GNOME.** `follow_system` now reacts to the desktop switching color scheme without a restart, via the XDG desktop portal over D-Bus (reusing the already-linked `dbus` crate — no new dependency). It also fixes the startup case where Linux always resolved to `theme_dark` regardless of the real preference.
- **`opacity_scope = background | text`.** New key to make window opacity apply to terminal text too (glyphs, box-drawing, underlines), not just the background — the full-glass look, opt-in. Default `background` keeps text crisp.
- **`unfocused_dim` (0.0–0.9).** Configurable strength for the unfocused-split dim (default 0.28).

#### Status bar (first plugin surface)
- **Data-driven segments** with five new built-ins — tab count, pane zoom, active profile, shell-integration busy state, hostname.
- **`glassy @ set-segment <id> <text>` / `clear-segment <id>`** IPC verbs let external scripts push custom status-bar segments — the first Phase-1 plugin surface (see `docs/plugins.md`).

#### Shell integration and panes
- **OSC 633** (VS Code shell integration) parsed as a second prompt-mark source alongside OSC 133, and **click-to-select a command's whole output**. Opt-in `command_blocks = off | badges | cards` adds Warp-style glass bands behind completed commands.
- **Pane headers** render as an overlay strip that no longer steals terminal rows, with a `pane_header_style = full | compact` option, an optional single-pane header, and pane-index numbers. New `cycle_layout` action steps a split through row/column/main/grid presets.

#### Performance and tooling
- **Scrollback memory bounding** (`scrollback_background_cap` / `scrollback_background_idle_secs`) caps resident scrollback for idle/backgrounded panes without reducing the default visible history.
- **Benchmark suite**: criterion micro-benches for hot paths and a `scripts/bench.sh` vtebench harness comparing glassy vs alacritty vs ghostty (see `docs/benchmarks.md`). Internals moved behind a `[lib]` target so benches can reach them.

### Changed
- Display name is now **"Glassy"** (title-cased) on every user-facing surface — window title, macOS Cmd-Tab / Dock / menu bar (`CFBundleName` + `CFBundleDisplayName`, bundle renamed `Glassy.app`), desktop notifications, and the Linux `.desktop` entry. All identifiers stay lowercase (`glassy` binary, `TERM_PROGRAM`, bundle id, terminfo, config paths, Wayland `app_id`).

### Fixed
- **Kitty keyboard protocol negotiation** now actually works: `kitty_keyboard` is enabled at the root (via a shared `term_config_base()` so a resize or settings change can't silently reset it), so glassy answers the `CSI ? u` progressive-enhancement query and latches the mode flags. Previously the flag defaulted off, leaving the CSI-u encoder permanently inert and the query unanswered.
- **macOS menu bar** (Glassy / File / Edit / View / Window) now appears. winit's built-in default menu was overwriting glassy's during launch; disabled with `with_default_menu(false)`.
- **Links now read and behave as links.** Explicit OSC 8 hyperlinks are always underlined (previously nothing marked them). The hover underline is forced to repaint on the link row (it's a render overlay that carries no terminal damage, so damage-only frames — common under mouse-mode apps — skipped it). Inside apps that capture the mouse (Claude Code, vim, …) the affordance + click were fully suppressed; they now work while the link-open modifier is held (revealed immediately on modifier press under a stationary pointer). Link-open is **⌘+Click on macOS** (Ctrl elsewhere), matching iTerm2/ghostty — previously Ctrl+Click everywhere, which on macOS is a secondary click.
- **Run-command scratchpad** (palette `>`/`$`) now passes the command as a separate argv element (`eval "$1"`) instead of interpolating it into the wrapper script, so an unbalanced quote or paren in the command can no longer break the wrapper — the exit-status readout and "press any key to close" hold always run. The command text (possibly containing secrets) is also never written to the on-disk session state.
- **Bare hex color values** (`color.ansi0 = #15161e`) survive a Settings-panel Save: the leading `#` is no longer misread as a comment marker, which previously rewrote the line as `color.ansi0 = #ff0000  #15161e` and made the config fail to parse (the app then refused to start).
- **Scrollback** adjustments from the settings panel / command palette now clamp to the same 200,000-line cap the config file enforces, instead of an independent 1,000,000 ceiling that could push the live terminal past the memory bound.
- **Compact pane headers** are sized from the live cell height so their vertically-centered glyphs no longer render above the header band and bleed into the pane above (worse on HiDPI).
- **Single-pane header** (`pane_headers_single`) insets its right edge to clear the floating Help/Settings/Menu icon cluster when the tab strip is hidden, instead of washing over the icons.
- **Command-output selection** (gutter click-to-select) now lands on the correct rows when the scrollback is scrolled: the absolute→viewport→line translation applies the display offset twice, matching the rest of the selection math.
- **Persistent full-row artifact under visual effects.** When the cursor or IME-preedit row was skipped by the per-row content loop, the overlay fallback wiped that row's cached cells and repainted nothing, leaving a blank/partial line that survived until unrelated output re-dirtied it. The fallback no longer destroys content it won't rebuild.
- **Unfocused split pane appeared see-through instead of dimmed.** The dim overlay was drawn through the blend-less pass, replacing the pane's pixels (and punching the window alpha) rather than darkening them; it now composites in the overlay layer.
- **Config saves preserve inline trailing comments**, surface write failures as a toast instead of only a log line, and re-read the file before writing so a settings Save can't clobber a concurrent external edit.
- **GPU device loss** now registers a `device_lost` callback and degrades gracefully instead of relying on undocumented driver behavior.
- **`font_symbol_map`-routed glyphs flickered between their mapped flat glyph and a color emoji.** Ligature run-shaping always shaped with the primary font family, ignoring the per-codepoint `font_symbol_map` routing that the single-character path respects; when a mapped character got swept into a multi-cell run (e.g. a status-line icon next to changing text), it fell through to cosmic-text's own font cascade instead — which on macOS always includes Apple Color Emoji — flipping between flat and color depending on which path a given frame happened to take. Symbol-mapped characters are now excluded from ligature-run eligibility so they always take the single-character path.
- **Any BMP symbol could flicker between a flat glyph and color emoji, not just `font_symbol_map` entries.** The underlying issue was broader than routing: whenever a codepoint wasn't covered by the requested font family, both cosmic-text's internal fallback cascade (used by run-shaping) and glassy's own CoreText cascade (used by the single-character `.notdef` path) would accept *any* font's glyph for it — including Apple Color Emoji, which on macOS sits ahead of plain symbol fonts in cosmic-text's built-in fallback order and carries color art for far more codepoints than Unicode's `Emoji_Presentation=Yes` set (e.g. ⚙ ⌘ ⓘ ✕ are `Emoji=Yes`/`Emoji_Presentation=No` — default TEXT — yet Apple Color Emoji still has bitmaps for them). Both paths now gate color glyphs behind a bounded `Emoji_Presentation`-default table plus an explicit-VS16 check: a codepoint only renders as color art if the source text explicitly requests it (`U+FE0F`) or the codepoint's own Unicode default is emoji. Anything else renders flat, or blank if no font offers a non-color glyph — matching how other terminals (kitty/alacritty/ghostty) handle the same codepoints.
- **Holistic rework of the flat/color-emoji resolution policy** (`src/text/presentation.rs`), replacing the three previous ad-hoc, per-path patches above with one authoritative design. Ligature-run eligibility now requires the primary font to *actually cover* a character (`Renderer`/`Text::primary_font_covers`, a real charmap check — not just "the shaper didn't return notdef"), not merely that it isn't `font_symbol_map`-routed: a legitimate ligature (`->` → `→`) is, by construction, characters the same font's GSUB table substitutes, so this costs real ligatures nothing while keeping every character that needs fallback on the single-character path — the one path with deliberate, controlled resolution. That single-character path's own CoreText fallback is now presentation-aware too: it retries with an explicit VS15 (`U+FE0E`, "text presentation") before accepting a color result, the same signal Messages.app/Notes.app use to keep a symbol flat, and only falls through to blank if no font in the chain offers a legitimately flat or sanctioned-color glyph. Net effect: a given codepoint's resolution no longer depends on which of the three shaping paths happens to touch it that frame, so both symptoms — static inconsistency in chrome icons and frame-to-frame flicker in PTY content — are fixed by construction rather than by reacting to individual glyphs.
- **Default-text symbols the primary font lacks (☀ ✈ ⚙ …) rendered blank instead of flat.** These resolve to Apple Color Emoji in cosmic-text's cascade; the presentation gate correctly refuses the unsanctioned color, but the single-character path then emitted nothing. Glyph resolution across the single-character, cluster, and ligature-run paths is now unified into one funnel (`finalize_shaped_glyph`) feeding one ordered fallback chain (`recover_glyph`: CoreText flat glyph → sanctioned color → VS15-forced flat → a visible placeholder box as a last resort), so such a symbol renders flat rather than vanishing. This also closes a latent gap where a character routed by `font_symbol_map` or a bold/italic family override to a face that lacked it would silently blank (that path had no fallback at all), and removes the redundant, inconsistently-gated duplicate resolution logic the previous fix had smeared across five call sites.
- **Color emoji shattered into fragments or vanished when scrolling emoji-dense content, and stayed broken until scrolled away.** On glyph-atlas overflow the caches were cleared and both packers reset *mid-frame*, so emoji already emitted earlier in the same frame kept UVs pointing into atlas texels that were then overwritten (readily triggered because the color atlas was only 512², which a dense emoji view overflows). The repack is now deferred to the next frame boundary — the overflowing glyph is skipped for a single frame and everything repacks cleanly into a rewound atlas on the already-forced full rebuild — truncated packs are no longer cached (they'd otherwise persist as permanent blanks), and the color atlas is enlarged to 1024² so realistic emoji-dense views don't overflow in the first place.
- **macOS Dock / Cmd-Tab icon was off-center, and never appeared under `cargo run`.** The `.icns` art sat flush to the canvas's top-left (uneven transparent padding), so the tile read as shifted left in the Dock; it's regenerated centered on a square canvas with even ~10% padding (matching the macOS icon grid). Separately, the runtime icon was set before the event loop ran — when an unbundled binary isn't yet a `.regular` activation-policy app — so it only ever showed in the packaged `.app`; it's now set from `App::resumed` once the first window exists (so it shows under `cargo run` too), and skipped for the packaged `.app`, which keeps its identical `Info.plist` icon rather than a redundant runtime override.

#### Menus
- **Hamburger menu icons read as pictograms, not stray punctuation.** `|`/`-`/`»` were literal keyboard characters doing icon duty; replaced with a command key (Command palette), solid split-axis bars, etc. Settings now uses a proper gear ⚙: with the unified glyph-resolution fallback (see the Fixed entries above), a default-text symbol the primary font lacks resolves to a flat CoreText glyph instead of blanking, so ⚙ renders where it previously came back blank. About keeps a plain `i` (ⓘ U+24D8 still resolves to an oversized fullwidth glyph on this stack). The ≡ hamburger button itself is now drawn at 1.5× its glyph size within the same button box so it doesn't get lost next to the tab chips, and every menu icon is centered on its own ink box (not the shared text baseline), fixing inconsistent sizing/alignment between icons drawn from different Unicode blocks.
- **Shortcut hints matched the wrong platform.** The hamburger/context-menu hint column (`Ctrl+Shift+T`, etc.) was a single hardcoded table used on every OS, even though macOS's actual default binds are different chords entirely (`cmd+t`, no shift; see `mac_default_binds` in `config/keymap.rs`) — so the menu could show a modifier-and-chord combination that doesn't match what the key actually does. Now resolves per-platform to match the real default bind, using the same ⌘/⇧ HIG symbol convention the Help panel and command palette already use. (Still a static per-platform default, not the live keymap — a custom `[keybindings]` override won't be reflected here, unlike the Help panel/palette which read the live map.)

#### Settings panel
- **Row spacing collapsed after informational text.** An `Info` row (e.g. "Applies on restart — the running font stack isn't reloaded live.") reserved no trailing gap, so the next row sat flush against it while every other row type had one — read as overlapping/cramped in the Terminal and Appearance sections. `Info` rows now bake in the same gap as everything else.
- **Sidebar section labels could run into the content pane.** "Notifications" (13 chars) was drawn unclipped against a sidebar column sized for exactly 13 cells with no room left for its own leading padding, so it overflowed straight into the content pane (which starts with zero gap after the sidebar). Widened the column to actually fit it and clipped the label as a hard backstop.
- **A text field's placeholder could overflow past its own box** (e.g. the Symbol map field's `U+E000-U+F8FF:Symbols Nerd Font Mono` hint) — the placeholder branch drew unclipped while the real-text branch already scrolled/windowed to fit, so only an empty field showed the overflow. Both branches now respect the same boundary.

#### Light-theme chrome legibility
- **Toast and inline-peek cards** are now legible on light themes. Both hand-rolled a near-black card background (`bg*0.12+0.04`) that stayed dark on light themes while their foreground text followed the theme — leaving dark-on-dark text. They now use the shared theme-aware floating-surface fill. The peek card's document glyph also swapped from a non-BMP codepoint (which tofued on most terminal fonts) to a BMP-safe one.
- **Split-pane dividers** no longer vanish on light themes: the seam color is elevated via theme-aware math (darken on light backgrounds) instead of an additive lighten that clamped to white.
- **Folded command-output scrim** fades toward the terminal background instead of hard black, so the "… N lines hidden" summary stays readable on light themes.
- **Command-palette query field** uses the theme-aware recessed-track fill instead of a flat black box that was opaque and out of place on light themes.
- **Keyboard focus ring** on the accent-filled Save button no longer overpaints the accent body grey — the ring subtracts the button's own fill.

#### Chrome design tokens
- **Unified floating-surface elevation.** The E3 floating fill (dropdowns, dialogs, drag-ghost, toasts, inline peek, command palette) now derives from the theme background like the rest of the chrome instead of the selection color, so all elevation tiers share one hue and differ only by amount. The command palette also reuses the shared metric scale instead of re-deriving pad/gap/radius by hand.
- **Eased quake slide.** The quake / drop-down window now decelerates into its resting edge with a cubic ease-out in both directions instead of moving linearly, for a softer drop. The slide still advances event-driven and settles back to 0% idle CPU.

#### Depth and icons
- **Soft drop shadows** under floating surfaces. Toasts, the inline peek card and the command palette now cast a soft, theme-aware drop shadow so they read as lifted off the terminal — the app's first shadows. Rendered by a new SDF branch in the existing overlay shader (no new GPU pipeline, so idle cost is unchanged).
- **Settings-strip gear icon** uses the BMP `⚙` (U+2699, covered by the already-loaded symbol fallback fonts) instead of a Private-Use-Area codepoint that tofued unless a Nerd Font was configured.

---

## [0.4.4] - 2026-07-02

### Fixed

- **Window resize** no longer pushes the prompt/last row below the window when tab bar is hidden.
- **Command palette** now shows the real platform chord (`Cmd` on macOS, `Ctrl` elsewhere) instead of a hardcoded label.

## [0.4.3] - 2026-07-02

### Fixed

- **Homebrew Cask** now auto-strips quarantine attributes, so installation "just works" without manual `xattr` removal.

## [0.4.2] - 2026-07-02

### Added

- **Homebrew Cask** distribution: `brew install --cask glassy` installs a properly signed macOS app bundle.

### Fixed

- Code signing verification for Homebrew Cask distribution.

## [0.4.1] - 2026-07-02

### Added

- **macOS universal binary** (arm64/x86_64) distributed as per-architecture `.app` bundle and `.dmg` installer.
- **Prebuilt Homebrew binary** formula with SHA-256 verification.

---

## [0.4.0] - 2026-07-01

### Added

#### Effects and visual enhancements
- **Power Mode** typing effect (opt-in): particle bursts and screen shake on keystroke.
- **Custom window effects**: stack any combination of effects (CRT, scan, bloom, blur) with per-channel intensity sliders.
- **CRT barrel warp** effect with configurable curvature and scanline intensity.

#### Keyboard and pane management
- **Pane navigation chords**: multi-key leader sequences for split pane control.
- **macOS menu bar**: Glassy / File / Edit / View / Window menus with native shortcuts.
- **`⌘`-hold tab numbers**: hold Command while pressing a number to switch tabs on macOS.
- **Pane drag-reorder**: drag pane dividers to rearrange split layout; `swap`, `rotate`, and `equalize` pane commands.

#### Visual and input improvements
- **Better unfocused pane dimming**: more visible distinction (0.10 → 0.28 opacity).
- **SGR 53/55 overline** support: complementary to underline decorations.
- **SGR-Pixel mouse** mode (1016): fine-grained mouse position reporting.
- **Improved cursor**: arrow cursor over content (was I-beam); better icon set.
- **Variable-font axes**: per-style font families with OpenType axis control and symbol/codepoint mapping.

#### Configuration and palette
- **Sectioned settings window**: organized config UI with custom-theme editor.
- **Configurable palette/status bar segments**: opacity actions, effects toggles, scrollback save features.
- **Light/dark theme switching**: `follow_system` config with `theme_light` / `theme_dark` selection.

#### Remote control and notifications
- **IPC/remote control**: kitty-style remote-control commands.
- **Rich notifications**: OSC 9/777 desktop alerts and command-finish notifications via `notify-rust`.

#### Copy mode and clipboard
- **Keyboard copy mode** (vi-style navigation): hjkl/arrow keys to select text, Enter to copy.
- **HTML clipboard flavor**: paste rich text with formatting.

### Changed

- Settings window uses immediate-mode GUI (`src/gui/`) with animated feedback and keyboard navigation.
- PTY read loop now owned by glassy (pre-processes images/OSC/protocol sequences before alacritty_terminal).
- Visual bell is now softer, accent-tinted (previously stark white flash).
- Narrow-base emoji (e.g. trans flag) render at full size.
- Tab bar activity dots and busy spinner animate only during active background output (event loop parks at `ControlFlow::Wait`, 0% idle).

---

## [0.2.1] - 2026-06-25

### Fixed

- **Debian/Ubuntu dependency**: declared `libdbus-1-3` runtime dependency for desktop notifications.

---

## [0.2.0] - 2026-06-25

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

[0.5.0]: https://github.com/alliecatowo/glassy/compare/v0.4.1...HEAD
[0.4.4]: https://github.com/alliecatowo/glassy/compare/v0.4.1...fc5fb89
[0.4.3]: https://github.com/alliecatowo/glassy/compare/v0.4.1...25d529a
[0.4.2]: https://github.com/alliecatowo/glassy/compare/v0.4.1...94afcdd
[0.4.1]: https://github.com/alliecatowo/glassy/releases/tag/v0.4.1
[0.4.0]: https://github.com/alliecatowo/glassy/releases/tag/v0.4.0
[0.2.1]: https://github.com/alliecatowo/glassy/releases/tag/v0.2.1
[0.2.0]: https://github.com/alliecatowo/glassy/releases/tag/v0.2.0
[0.1.0]: https://github.com/alliecatowo/glassy/releases/tag/v0.1.0
