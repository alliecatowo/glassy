# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Inline images: kitty graphics protocol (PNG incl. 8/16-bit & palette, raw
  RGBA, chunked, `c=`/`r=` cell sizing, aspect-aware, `a=d` delete) and sixel,
  drawn on a dedicated GPU atlas; images clear on screen-clear/reset.
- In-app settings overlay (`Ctrl+,`): live font size / opacity / bell / theme,
  saved back to the config file (merge-preserving).
- Help overlay (`F1`): built-in keybinding cheat-sheet.
- Live theme switching across 8 built-in themes (Tokyo Night, Catppuccin
  Mocha/Macchiato, Gruvbox, Dracula, Nord, Solarized, Rosé Pine).
- Header bar: shows the active title with a single tab, a scrollback-position
  indicator, and activity dots on busy background tabs.

### Changed

- glassy now owns its PTY read loop (so it can tap image escape sequences).
- Softer, accent-tinted visual bell (was a stark white full-screen flash).
- Narrow-base emoji (e.g. the trans flag) render at full size.

### Performance

- Allocation-free redraw path (direct glyph-instance push, persistent
  flush-pass scratch, removed a redundant glyph-cache layer).
- Skip default-background cell quads; fewer GPU state rebinds for the image
  pass; dropped the `image` and `regex` dependency stacks (smaller binary).

## [0.1.0] - 2026-06-19

Initial release.

### Added

- GPU-accelerated rendering: an instanced `wgpu` renderer fed by a dynamic
  glyph atlas, with on-demand, damage-based redraw to stay idle when nothing
  changes.
- 24-bit truecolor support.
- Color emoji rendering with CJK font fallback.
- Procedural box-drawing characters for crisp, gap-free lines.
- Mouse support: reporting, text selection, and clipboard copy/paste.
- Scrollback buffer.
- Tabs.
- Configurable window decorations.
- Configurable cursor.
- Window translucency.
- Configuration file with theming support.
- OSC 8 hyperlink support.
- Terminal bell (visual; optional audible bell behind the `bell-audio`
  build feature).

[Unreleased]: https://github.com/alliecatowo/glassy/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/alliecatowo/glassy/releases/tag/v0.1.0
