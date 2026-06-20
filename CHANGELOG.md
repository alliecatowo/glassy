# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
