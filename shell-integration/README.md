# glassy shell integration

These scripts make your shell emit [OSC 133][osc133] semantic prompt marks so
glassy can:

- **group terminal output into command blocks** (prompt → command → output),
- show an **exit-status badge** (`✓` / `✗ <code>`) next to each prompt,
- show how long each command took (**duration**),
- jump between prompts and **fold** a command's output (collapse/expand).

They also emit **OSC 7** (working directory) so new tabs and splits open in the
same directory you were in.

## What the marks mean

| Mark            | Meaning                                              |
| --------------- | ---------------------------------------------------- |
| `OSC 133 ; A`   | prompt start                                         |
| `OSC 133 ; B`   | prompt end / command start (where you type)          |
| `OSC 133 ; C`   | command executed — output begins here                |
| `OSC 133 ; D ; <exit>` | command finished, carrying its exit code      |

glassy records the row of each mark plus the wall-clock time of `C` (start) and
`D` (end), so it can compute each command's duration and exit status.

## Installing

Pick the file for your shell and source it from your shell rc. Each script is a
no-op outside glassy (it checks `TERM_PROGRAM == glassy`) and is safe to source
unconditionally.

### bash — `~/.bashrc`

```bash
[ -n "$GLASSY_VERSION" ] && source /usr/share/glassy/shell-integration/glassy.bash
```

### zsh — `~/.zshrc`

```zsh
[[ -n "$GLASSY_VERSION" ]] && source /usr/share/glassy/shell-integration/glassy.zsh
```

### fish — `~/.config/fish/config.fish`

```fish
if set -q GLASSY_VERSION
    source /usr/share/glassy/shell-integration/glassy.fish
end
```

Adjust the path to wherever the scripts are installed (the repo ships them under
`shell-integration/`; packages install them under
`/usr/share/glassy/shell-integration/`).

### Forcing activation

To test the scripts in another terminal, set `GLASSY_FORCE_INTEGRATION=1` before
sourcing — they will then activate regardless of `TERM_PROGRAM`.

## Notes

- The scripts only *add* marks; they never replace your prompt. Your `PS1` /
  `fish_prompt` is preserved verbatim between the `A` and `B` marks.
- If you use a prompt framework (starship, powerlevel10k, oh-my-posh) that
  already emits OSC 133 marks, you do not need these — glassy reads whatever
  marks arrive.
- glassy also recognizes **OSC 633** (the VS Code / iTerm2-flavored superset of
  133, using the identical `A`/`B`/`C`/`D` mark set) as a second, equally valid
  mark source. If you already have a `vscode-shell-integration` snippet
  sourced — or a framework that emits 633 instead of 133 — you get command
  blocks for free with no glassy-specific script installed. glassy also reads
  633's `E` extension (`633;E;<cmdline>;<nonce>`), the literal command line the
  shell is about to run, as a more reliable alternative to reading it back off
  the grid. If a session somehow emits both 133 and 633, glassy honors
  whichever protocol's mark arrives first for that session and ignores the
  other, so commands are never double-counted.
- Opt in to a Warp-style card look for finished command blocks with
  `command_blocks = cards` in `glassy.conf` (default `badges`, today's
  appearance; `off` is reserved for a future full-disable switch). See the
  `[Configuration]` doc comment in `src/config/mod.rs` for the full key.

[osc133]: https://gitlab.freedesktop.org/Per_Bothner/specifications/blob/master/proposals/semantic-prompts.md
