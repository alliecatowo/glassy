# Quake / dropdown mode

Glassy can run as a **quake terminal** (a.k.a. dropdown / drop-down / Guake /
Yakuake style): a borderless window anchored to the top of the screen that slides
down when toggled and slides back up when dismissed, floating above your other
windows.

```
┌──────────────────────────── monitor ────────────────────────────┐
│ ░░░░░░░░░░░░░░░░░░░░  glassy (slides down)  ░░░░░░░░░░░░░░░░░░░░░ │  ← quake_height
│ ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░ │
├──────────────────────────────────────────────────────────────────┤
│                                                                  │
│                   your other windows underneath                  │
│                                                                  │
└──────────────────────────────────────────────────────────────────┘
```

## Enabling it

In `~/.config/glassy/glassy.conf`:

```conf
quake = true                 # borderless, top-anchored, slide-down window
quake_height = 0.5           # fraction of the monitor height (0.1 .. 1.0)
quake_animation_ms = 180     # slide duration in ms (0 = instant snap)
```

or on the command line:

```sh
glassy --quake --quake-height 0.4 --quake-animation-ms 150
```

When quake mode is on, launching `glassy` opens the window and slides it down once.
After that you toggle it with a hotkey (below) or the in-app key.

## Toggling — and the Wayland global-hotkey limitation

A dropdown terminal lives or dies by its **global hotkey** — the whole point is
that one keypress summons it from anywhere. Here is the hard truth:

> **Wayland has no portable global-hotkey API.** Unlike X11 (`XGrabKey`) there is
> no cross-compositor protocol a regular application can use to register a
> system-wide accelerator. On Wayland, owning a global shortcut is the
> *compositor's* job, by design (it's a security boundary). The
> `org.freedesktop.portal.GlobalShortcuts` XDG portal exists but is not yet
> universally implemented, and even where present it routes through a permission
> prompt — it is not something glassy can rely on across Hyprland / Sway / GNOME /
> KDE today.

Glassy therefore does **not** fake a global hotkey. Instead it splits the problem:

1. The running glassy instance listens on a per-user **Unix socket**
   (`$XDG_RUNTIME_DIR/glassy.sock`, or `/tmp/glassy-$USER.sock` as a fallback).
2. A second invocation is a thin **client**:

   ```sh
   glassy toggle    # slide in if hidden, out if shown
   glassy show      # slide in (idempotent)
   glassy hide      # slide out (idempotent)
   ```

   (also accepted as `glassy --toggle` / `--show` / `--hide`.)

You bind `glassy toggle` to a key **in your compositor** — the one layer that
*can* own a true global hotkey on Wayland. That client connects to the socket,
tells the running instance to toggle, and exits. If no instance is running it
prints a hint and exits non-zero (so your bind can fall back to launching glassy).

There is also an **in-app** key — `quake_toggle`, bound to **F12** by default —
that slides the window away from inside the terminal (handy for dismissing it
without leaving the keyboard).

### Compositor bind recipes

**Hyprland** (`~/.config/hypr/hyprland.conf`):

```conf
# Launch glassy once at startup (so the socket exists), then bind the toggle.
exec-once = glassy --quake
bind = , F12, exec, glassy toggle
# Or a more conventional grave/backtick dropdown key:
# bind = SUPER, grave, exec, glassy toggle
```

**Sway** (`~/.config/sway/config`):

```conf
exec glassy --quake
bindsym F12 exec glassy toggle
```

**GNOME** (Settings → Keyboard → View and Customize Shortcuts → Custom Shortcuts):

- Name: `glassy toggle`
- Command: `glassy toggle`
- Shortcut: press `F12` (or your preference)

Start glassy in quake mode at login via Settings → Apps → Startup, or run
`glassy --quake &` from your session autostart.

**KDE Plasma** (System Settings → Shortcuts → Custom Shortcuts → New → Global
Shortcut → Command/URL):

- Trigger: `F12`
- Action: `glassy toggle`

Add `glassy --quake` to *System Settings → Autostart* so the instance is running.

### X11

On X11 you *can* register a true global hotkey from a hotkey daemon
(`sxhkd`, `xbindkeys`, your WM's config). Bind it to `glassy toggle` exactly as
above; the same socket mechanism is used, so the behavior is identical to Wayland —
glassy doesn't grab the key itself either way, keeping a single code path.

`sxhkd` example (`~/.config/sxhkd/sxhkdrc`):

```
F12
    glassy toggle
```

## Notes & limitations

- **Multi-monitor:** the window drops onto whichever monitor it currently belongs
  to (its `current_monitor`), spanning that monitor's full width at
  `quake_height` of its height. A DPI / monitor change re-pins it to the top.
- **Always-on-top:** quake windows request the always-on-top window level so they
  float over normal windows when dropped. Some compositors honor this only for
  un-decorated windows (glassy's quake window is borderless, so this is fine).
- **Focus:** showing the window also focuses it. Whether a freshly-mapped
  always-on-top window *steals* focus is ultimately the compositor's call; most
  honor it for an explicit user toggle.
- **Idle cost:** the slide animation runs only while in flight. Once the window
  settles (fully shown or fully hidden) glassy returns to its 0%-idle `Wait`
  state — the quake feature adds no background CPU.
- **Single instance:** the first glassy process owns the socket. A second
  `glassy` launched while one is running will still open its own window (glassy is
  not forced single-instance for normal launches); only the `toggle/show/hide`
  *subcommands* talk to the existing instance.
