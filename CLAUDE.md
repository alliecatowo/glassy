# CLAUDE.md

Guidance for AI agents (and humans) working in the **glassy** repo.

## What glassy is

A minimal, fast, GPU-accelerated terminal emulator in Rust (`winit` + `wgpu`, with
`alacritty_terminal` driving the VT state machine). Targets macOS (Metal) and Linux
(Vulkan). The stripped release binary is ~10 MB; rendering is on-demand and damage-based
(0% idle CPU).

## Build, test, lint

- `cargo build` / `cargo run` — debug build (unbundled binary).
- `cargo test` — full suite. **Must be green before committing.**
- `cargo fmt` — **must be clean** (CI runs `cargo fmt --check`).
- `cargo clippy --all-targets` — **must be clean**.
- `make` covers the Linux install; the macOS `.app`/`.dmg` and OS packages are assembled in
  `.github/workflows/release.yml`.

## Architecture pointers

- `src/input.rs` — key event → PTY byte encoding (kitty protocol, modifyOtherKeys, legacy).
  Pure and unit-tested; drive changes here through the existing tests.
- `src/app/` — the winit `ApplicationHandler`: event loop, keymap dispatch (`keys.rs`),
  overlays, panes, window chrome, and the macOS menu bar (`mac_menu.rs`).
- `src/pty/` — the PTY read/parse loop. `term_config_base()` in `mod.rs` is the **single
  source** for the `alacritty_terminal` config — spread it, never `Config::default()`.
- `src/renderer/` — wgpu glyph/cell/image rendering.
- `src/config/` — config parsing, keymap defaults (`keymap.rs`), themes.

## Conventions that bite

- **The kitty keyboard protocol is progressive-enhancement**: off until an app negotiates it
  (`CSI > 1 u`). glassy sends *legacy* encodings by default and kitty CSI-u encodings only
  once negotiated. Keep that split — new editing-key defaults go in the non-kitty path so
  full-screen apps still get raw key events.
- Match the surrounding code's idiom and comment density — this codebase documents the *why*
  heavily. Don't refactor adjacent code that isn't part of your task.

## Changelog — REQUIRED

`CHANGELOG.md` follows [Keep a Changelog](https://keepachangelog.com). **Every user-facing
change must add a bullet under `## [Unreleased]`** in the correct subsection (`Added` /
`Changed` / `Fixed`), grouped under the existing `#### ` topic headings where one fits. Do it
in the same change as the code — don't defer it. Purely internal refactors with no
user-visible effect may be skipped.

## Git

- Branch off `main`. PRs default to **draft** unless asked otherwise.
- Commit only when asked; keep commits focused and logically grouped.
