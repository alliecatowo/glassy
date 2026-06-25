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

[osc133]: https://gitlab.freedesktop.org/Per_Bothner/specifications/blob/master/proposals/semantic-prompts.md
