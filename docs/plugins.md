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

- **`get-config <key>` / `set-config <key> <value>`** — read or write any
  config key by name, applied live and persisted to `glassy.conf` the same
  way the settings overlay's Save button does (both go through the
  declarative `SAVED_KEYS` table landing alongside this in `src/app/settings.rs`
  — see [ROADMAP.md](../ROADMAP.md)'s Now section). This is what makes
  `glassy @ set-config opacity 0.8` from a shell script equivalent to opening
  the settings overlay.
- **`list-themes`** — enumerates the resolved registry (built-ins + user
  themes dir), so a script can validate a theme name before calling
  `set-theme`, or build its own theme picker.
- **`reload-config`** — forces the same reload path the file-watcher already
  triggers on save, useful when a script edits `glassy.conf` directly instead
  of going through `set-config`.
- **`run-action <name>`** — invokes a named command-palette action (the same
  action registry the fuzzy palette searches) without going through a
  keybinding. This is the escape hatch for anything Phase 1 doesn't expose a
  dedicated verb for.

Every verb replies on the same request/reply cycle as the existing ones —
`OK [text]` or `ERR <message>` — so a script gets a clean success/failure
signal without parsing terminal output.

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
