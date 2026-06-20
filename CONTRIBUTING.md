# Contributing to glassy

Thanks for your interest in glassy! It's a small, focused GPU terminal
emulator, and contributions of all sizes are welcome — bug fixes, docs, tests,
and well-scoped features alike.

## Getting set up

You'll need a recent stable Rust toolchain (via [rustup](https://rustup.rs)) and
a working GPU stack (Vulkan / Metal / DX12, as appropriate for your platform).

```sh
git clone https://github.com/alliecatowo/glassy
cd glassy
cargo build
```

For the optional audible bell you'll also need audio dev libraries
(`alsa-lib-devel` / `libasound2-dev`) and the feature flag:

```sh
cargo build --features bell-audio
```

## Running

```sh
cargo run                 # debug build
cargo run --release       # optimized build
make run                  # same, via the Makefile
```

Useful flags while hacking: `--font-size`, `--opacity`, `--theme`, and
`-e <cmd>` to launch a specific program instead of your shell. See
`extra/glassy.1` (the man page) or `--help` for the full list.

## The checks (please run these before opening a PR)

CI enforces formatting and a clippy run with **warnings denied**, so your PR
won't merge until these are clean. Run them locally first:

```sh
cargo build
cargo test
cargo fmt --all
cargo clippy --all-targets -- -D warnings
```

Or, via the Makefile shortcuts:

```sh
make build
make test
make fmt
make clippy
```

## Code style

- **Formatting:** `cargo fmt --all` is the source of truth. Don't hand-format.
- **Lints:** keep `cargo clippy --all-targets -- -D warnings` clean. If a lint
  is genuinely wrong for a case, prefer a narrowly-scoped `#[allow(...)]` with a
  short comment over disabling it broadly.
- **Comments:** explain *why*, not *what*. The existing code leans on
  module-level doc comments to explain intent — match that tone.
- **Scope:** keep PRs focused. One logical change per PR is much easier to
  review than a grab-bag.
- **Tests:** add or update tests when you change behavior, especially for the
  config parser and other pure logic that's easy to cover.

## Architecture (the short version)

glassy is a deliberately thin stack:

- **Rendering** runs on `wgpu` with an instanced draw and a dynamic glyph atlas;
  text shaping/rasterization go through `cosmic-text` + `swash`.
- **Windowing & input** use `winit`.
- **PTY / VT parsing** currently lean on `alacritty_terminal`, which is being
  incrementally replaced with hand-rolled pieces. The PTY and color boundaries
  are kept swappable on purpose.

If you're planning a larger change to the renderer, PTY, or input paths, please
open an issue first — some of those areas have work in flight and it's worth
coordinating before you dig in.

## Submitting changes

1. Fork and create a topic branch.
2. Make your change; run the checks above.
3. Open a pull request using the template, describing what and why.
4. Be ready for a round or two of review — it's a small project and we like to
   keep it tidy.

By contributing, you agree that your contributions are licensed under the
project's [MIT License](LICENSE).
