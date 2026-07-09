# Plugins

glassy has no plugin runtime today, and won't gain one lightly — a 2-D glyph
renderer that fits in ~10 MB stripped is a deliberate tradeoff, and embedding
an interpreter or a WASM engine works against that. Instead of designing a
plugin API up front and hoping it's the right shape, extensibility is being
grown in phases, each one justified by what people actually script against
glassy, not by what a plugin system "should" have.

This doc tracks all three phases and their status. See [ROADMAP.md](../ROADMAP.md)
for how each phase fits the overall release plan.

| Phase | What | Status |
| --- | --- | --- |
| 1 | Automation API: user themes dir + extended `glassy @` IPC | This wave (w14) |
| 2 | `[hooks]` config section, shells out on events | Next |
| 3 | WASM component plugins (opt-in, sandboxed) | Speculative |

---

## Phase 1 — automation, not plugins (this wave)

Phase 1 adds two things that let you *script* glassy from outside the
process. Neither one runs inside glassy: there is no plugin lifecycle, no
loaded code, no sandboxing model to design, because nothing untrusted ever
executes in-process. This is deliberately the cheapest possible extensibility
story, and it's worth being explicit that it's automation, not a plugin
runtime — the two get conflated a lot.

### Install a theme without code: the user themes dir

Themes now resolve through a single-source registry that checks
`~/.config/glassy/themes/` before falling back to the ~40 built-ins. Drop a
theme file in that directory and `theme = <name>` in `glassy.conf` (or
`glassy @ set-theme <name>`, below) picks it up — no rebuild, no PR against
this repo required to add a theme to your own setup.

### Script glassy: extended `glassy @` IPC

glassy already exposes a kitty-style remote-control socket (see
[`src/ipc/mod.rs`](../src/ipc/mod.rs) and [`src/ipc/control.rs`](../src/ipc/control.rs)
for the implementation). A second `glassy` invocation connects to the running
instance's Unix socket, writes one request line, and reads one reply line
back:

```text
C→S:  @ <verb> [args…]\n
S→C:  OK [text]\n   |   ERR <message>\n
```

The existing verbs (`ls`, `open-tab`, `split`, `send-text`, `set-theme`,
`set-color`, `focus-tab`) already let a script drive tabs, panes, and input.
Phase 1 extends the same grammar with verbs aimed at configuration and
scripted reactions, in the same line-oriented, ASCII, shell-pipe-friendly
style as the rest of the protocol:

```text
glassy @ get-config <key>         # OK <value>  |  ERR unknown key '<key>'
glassy @ set-config <key> <value> # OK  |  ERR ...  — writes + applies live
glassy @ list-themes              # OK tokyo-night, catppuccin-mocha, ...
glassy @ reload-config            # OK  — re-reads glassy.conf from disk
glassy @ run-action <name>        # OK  |  ERR unknown action '<name>'
```

- **`get-config <key>`** — reads a key's *current live value* by looking it
  up in the declarative `SAVED_KEYS` table (`src/app/settings_save.rs`) — the
  same table `App::save_settings`/the settings overlay's Save button writes —
  plus a `font_size` special case (its live value lives in the renderer's
  effective px, not `Config::font_size`, which only reflects the size at
  startup; that's also why `SAVED_KEYS` itself excludes it). Because the read
  is of the live config, a key that `reload-config` can't apply live (see
  below) reads back its *startup* value even after a `set-config` wrote a
  new one to disk. An unrecognized key replies `ERR unknown key '<key>'`.
- **`set-config <key> <value>`** — a deliberately simple write-through design
  with no per-key live setter to keep in sync: the key must be one
  `get-config` can also read; the value is dry-run parsed through the same
  parser `glassy.conf` loading uses (`config::validate_kv` /
  `config::parse::apply_kv`) and rejected with `ERR` only if it fails to parse
  at all (e.g. a non-numeric `opacity`, an invalid `cursor_style` word). A
  value that parses but is out of a field's valid range is **persisted
  verbatim and silently clamped at apply time**, not rejected —
  `set-config opacity 5` succeeds, writes `opacity = 5` to `glassy.conf`, and
  the *effective* value (live now, and again on every future load of that
  file) is `1.00`. Once validated, the value is persisted via `config::save`
  (the exact mechanism the settings overlay's Save button uses) and then
  applied live through the identical path `reload-config` uses (below) — so
  `set-config` **always writes to `glassy.conf`** on success, unlike a
  hypothetical apply-without-persisting verb. `glassy @ set-config opacity
  0.8` from a shell script is equivalent to opening the settings overlay,
  changing Opacity, and clicking Save. A multi-word value needs no quoting
  gymnastics: everything after the key is the value, spaces included
  (`glassy @ set-config font_features calt=0 liga`), mirroring `send-text`'s
  rest-of-the-line handling.
- **`list-themes`** — enumerates the resolved registry (built-ins + user
  themes dir), so a script can validate a theme name before calling
  `set-theme`, or build its own theme picker.
- **`reload-config`** — forces the same reload path the file-watcher already
  triggers on save (`App::apply_config_reload`), useful when a script edits
  `glassy.conf` directly instead of going through `set-config`. Note this
  reload path only ever applied a *curated subset* of keys live (opacity,
  window effect, bell flags, status bar, pane headers, command-history
  capacity, word separator, theme/`follow_system`, command-finish
  notification settings, command folding) — this predates Phase 1 and isn't
  changed by it. Keys outside that subset (`font_family`, `scrollback`,
  `cursor_style`, `padding`, …) are written to `glassy.conf` correctly by
  `set-config` but only take visual effect after a restart, exactly as
  editing them by hand and using `reload-config` would.
- **`run-action <name>`** — invokes a named command-palette/keybinding action
  (`config::keymap::parse_action`) through `App::run_key_action`, the *exact*
  same dispatch a keychord or (on macOS) a menu-bar click uses — see
  `src/app/mac_menu.rs`'s module doc for the sibling case of routing a
  non-keyboard trigger through that one path. An unrecognized name replies
  `ERR unknown action '<name>'`. This is the escape hatch for anything Phase 1
  doesn't expose a dedicated verb for.

Every verb replies on the same request/reply cycle as the existing ones —
`OK [text]` or `ERR <message>` — so a script gets a clean success/failure
signal without parsing terminal output.

### Custom status-bar segments: `set-segment` / `clear-segment` (w15)

Every verb above either reads glassy's state or mutates it the same way a
keybinding would. `set-segment` is different: it's the first Phase-1 verb
whose whole purpose is pushing content *into* glassy's UI from the outside —
a CI status, a build result, a background job's progress — without glassy
knowing anything about where that text came from.

```text
glassy @ set-segment <id> <text...>  # OK  — shows/updates a custom segment
glassy @ clear-segment <id>          # OK  — removes it (no-op if unset)
```

- **`set-segment <id> <text...>`** — pushes (or updates, if `id` is already
  set) the display text for a custom status-bar segment. `id` is an arbitrary
  caller-chosen name (lower-cased), scoped only to this glassy instance; `text`
  is the rest of the line verbatim, spaces included, mirroring `send-text`/
  `set-config`'s rest-of-line handling. Two bounds keep an external script from
  growing the bar without limit: at most 8 distinct ids at once (a `set-segment`
  for a *new* id past that replies `ERR too many custom segments (max 8)`;
  updating an existing id always succeeds, even at the cap), and each segment's
  text is silently truncated to 64 chars (nothing clips segment text at paint
  time, so this is the only guard against a long string pushing everything
  else off-screen).
- **`clear-segment <id>`** — removes a segment set by `set-segment`. Always
  replies `OK`, even if `id` was never set or was already cleared.

A custom segment shows in the status bar in one of two ways: if
`status_bar_segments` includes the `custom` token, every active custom
segment renders at that position (in the order they were first set); if it
doesn't, any active custom segment(s) are appended at the end of the left
side anyway, so `set-segment` output isn't silently dropped just because the
user hasn't edited their `status_bar_segments` config.

**Worked example** — a build script that reports its own status:

```sh
#!/bin/sh
glassy @ set-segment build "building..."
if make; then
    glassy @ set-segment build "build ok"
else
    glassy @ set-segment build "build FAILED"
fi
# Clear it a few seconds later so it doesn't linger forever:
sleep 5 && glassy @ clear-segment build &
```

With `status_bar_segments = cwd git_branch custom time` in `glassy.conf`, the
bar shows `~/proj  main  building...  14:32` while the script runs, updating
live as `set-segment` calls land — no glassy restart, no config edit per
update.

Like every Phase 1 verb, this is plain IPC: the segment is just a string
`App` holds and the status-bar painter draws, not a hook into anything
running inside glassy. See "What Phase 1 explicitly is not" below.

### What Phase 1 explicitly is not

- **No plugin runtime.** Nothing is loaded into the glassy process. A script
  calling `glassy @ ...` is just another client of the same socket the quake
  `toggle`/`show`/`hide` verbs use — there's no extension point *inside*
  glassy for Phase 1 to hang off of.
- **No sandboxing.** There's nothing to sandbox: the client process is a
  normal OS process with normal OS privileges, talking to glassy over a
  socket exactly like any other IPC client would.
- **No lifecycle.** No install/enable/disable/uninstall states, no plugin
  manifest, no versioning story. A "plugin" in Phase 1 is a shell script that
  calls `glassy @ ...` — it has whatever lifecycle the script's author gives
  it.

### Trust boundary: same-user Unix socket

The control socket lives at `$XDG_RUNTIME_DIR/glassy.sock` (or a `/tmp`
fallback keyed by username) and is chmod'd `0600` after bind — see
`start_server` in `src/ipc/mod.rs`. That means:

- Only the same OS user that owns the running glassy process can connect.
- Anything that user can run (a shell script, a cron job, another program)
  can drive glassy fully — open tabs, send keystrokes, rewrite the config.
- There is no additional authentication layer, and Phase 1 does not add one.

This is a deliberate decision, not an oversight: the socket already grants
this level of trust for the quake `toggle`/`show`/`hide` verbs, and Phase 1
just extends the surface reachable through the same boundary. If glassy ever
needs a *cross-user* or *cross-machine* control surface, that's a different
problem (authentication, transport security) that Phase 1's same-user socket
doesn't attempt to solve.

---

## Phase 2 — config-declared hooks (next)

Phase 1 lets you drive glassy from the outside. Phase 2 lets glassy call
*out* to you when something happens, still with no embedded interpreter —
just declarative shell-outs, configured like any other glassy setting:

```ini
[hooks]
on_command_finish = ~/.local/bin/notify-build.sh "%command" %exit %duration_ms
on_theme_change   = ~/.local/bin/sync-theme.sh %theme
on_tab_open       = ~/.local/bin/log-tab.sh %cwd
```

Each hook is a command line with `%`-substitution tokens, run via the same
mechanism glassy would use to spawn any child process — no scripting
language embedded in glassy, no `eval`.

- **`on_command_finish`** rides the existing OSC 133 semantic-mark flow.
  glassy already tracks command start/end per pane (`PromptTracker`, driven
  by shell-integration `A`..`D` marks) and already turns a finished, tracked
  command into a `UserEvent::CommandFinished { id, command, exit, duration }`
  event — today that event only drives a desktop notification when the
  window is unfocused (`App::notify_command_finished` in
  `src/app/user_event.rs`). Phase 2 adds a second consumer of the same event:
  substitute `%command`/`%exit`/`%duration_ms` into the configured hook
  command and spawn it. No new terminal-protocol plumbing is needed — the
  data already exists.
- **`on_theme_change`** fires whenever the active theme changes (live
  edit, `set-theme` control command, or `follow_system` flipping light/dark).
- **`on_tab_open`** fires when a new tab is created, with the tab's initial
  cwd substituted in.

None of this requires an interpreter inside glassy: every hook is "run this
external command with these values filled in," which is the same trust model
as a shell alias.

---

## Phase 3 — WASM component plugins (speculative)

Phase 3 is the version of "plugin system" people usually mean when they ask
for one: code that runs *inside* glassy, sees live terminal state, and can
draw its own UI. It is speculative — not designed in detail — and gated
behind an opt-in Cargo feature so it never affects the default build's size
or attack surface. The rough shape under consideration:

- A WASM engine (`wasmtime` or the lighter-weight `wasmi`) compiled in only
  under a Cargo feature flag, off by default. The ~10 MB stripped-binary
  target is a headline number for this project; a WASM runtime is a real
  departure from it, so it stays opt-in rather than becoming the default
  build.
- Plugins get **read access** to damage/cell events (the same per-dirty-row
  data the renderer already collects) and a **push surface** for overlay
  primitives — i.e., a plugin can react to what's on screen and draw
  additional overlay content, but doesn't get arbitrary write access to the
  grid or the PTY.
- Sandboxing, capability scoping, plugin manifest/lifecycle, and the actual
  host-function ABI are all open questions. None of it is designed yet.

Phase 3 is intentionally being deferred until there's real usage data from
Phases 1 and 2 — what people actually script against `glassy @` and what
they reach for `[hooks]` to do is the input that should shape whether Phase 3
happens at all, and if so, what surface it actually needs to expose.
