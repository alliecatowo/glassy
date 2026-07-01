//! winit ApplicationHandler implementation: lifecycle and window events.
//!
//! The `window_event` match arm dispatches each `WindowEvent` variant to a
//! focused handler function. Keyboard, cursor-motion, mouse-button, and
//! mouse-wheel handling live in `keys.rs`; lifecycle helpers (`resumed`,
//! `user_event`, `about_to_wait`) remain here.

use super::*;

/// Decode the embedded flat dot icon (256px PNG) into a winit window icon for
/// the taskbar / switcher. Returns `None` on any decode error (the window just
/// falls back to the platform default — never fatal). Non-macOS only.
#[cfg(not(target_os = "macos"))]
fn load_window_icon() -> Option<winit::window::Icon> {
    const ICON_PNG: &[u8] = include_bytes!("../../assets/icons/glassy-256.png");
    let decoder = png::Decoder::new(std::io::Cursor::new(ICON_PNG));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    // Our asset is 8-bit; normalise RGB→RGBA so winit always gets 4 channels.
    let rgba = match info.color_type {
        png::ColorType::Rgba => {
            buf.truncate(info.buffer_size());
            buf
        }
        png::ColorType::Rgb => buf[..info.buffer_size()]
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        _ => return None,
    };
    winit::window::Icon::from_rgba(rgba, info.width, info.height).ok()
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return; // resumed can fire repeatedly; init exactly once
        }

        let attrs = Window::default_attributes()
            .with_title("glassy")
            .with_inner_size(LogicalSize::new(960.0, 600.0))
            // Request a translucent window (the "glassy" namesake). The renderer
            // drives the backdrop alpha from its configured opacity when the
            // compositor supports a transparent surface; on platforms that don't,
            // this is a harmless no-op and the window stays opaque.
            .with_transparent(true)
            .with_visible(false); // shown after the first frame to avoid a flash

        // Non-macOS: hand winit a window icon so the X11 taskbar / Alt-Tab
        // switcher / any WM that reads _NET_WM_ICON shows the flat dot icon (the
        // same art the hicolor PNGs + .desktop install use), instead of a
        // stale/blue or generic fallback. macOS ignores window icons — it uses
        // the bundled .icns (set separately via the dock-icon path), so skip it
        // there. Wayland ignores it too but the call is a harmless no-op.
        #[cfg(not(target_os = "macos"))]
        let attrs = attrs.with_window_icon(load_window_icon());

        // Wayland has no per-window icon in the core protocol — GNOME/KDE resolve
        // the dock + switcher icon by matching the surface's app_id against an
        // installed .desktop file. Pin it to "glassy" so it matches
        // glassy.desktop (Icon=glassy → the hicolor PNGs) instead of winit's
        // derived default, which is what made the dock icon differ from the
        // Applications-grid icon.
        #[cfg(all(unix, not(target_os = "macos")))]
        let attrs = {
            use winit::platform::wayland::WindowAttributesExtWayland;
            WindowAttributesExtWayland::with_name(attrs, "glassy", "glassy")
        };

        // macOS: drop the separate OS title bar and let glassy's own content fill
        // the whole window (ghostty-style). The traffic-light buttons float over
        // the top-left; glassy's top chrome band insets past them (see
        // TRAFFIC_LIGHT_INSET). title_hidden removes the centered title text.
        #[cfg(target_os = "macos")]
        let attrs = {
            use winit::platform::macos::WindowAttributesExtMacOS;
            attrs
                .with_titlebar_transparent(true)
                .with_fullsize_content_view(true)
                .with_title_hidden(true)
        };
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("failed to create window: {e}");
                event_loop.exit();
                return;
            }
        };
        window.set_ime_allowed(true);

        // macOS: with the title bar hidden and content fullsize, AppKit's titlebar
        // would auto-drag the window when the user drags our tab chips. Disable
        // OS-driven window moving so tab drags reach glassy; empty chrome areas
        // move the window manually via `drag_window()` (see strip_click).
        #[cfg(target_os = "macos")]
        Self::disable_macos_window_drag(&window);

        let ms = |t: Instant| t.elapsed().as_secs_f64() * 1000.0;
        log::info!("startup: window created at {:.1} ms", ms(self.started));

        // Honor the system light/dark preference at startup (when follow_system is
        // on): pick theme_light/theme_dark before the renderer reads the clear
        // color, so the very first frame already matches the OS scheme.
        if self.apply_system_theme(window.theme()) {
            self.force_full_redraw = true;
        }

        // Query the monitor refresh rate for the frame-coalescing throttle.
        if let Some(hz) = window
            .current_monitor()
            .and_then(|m| m.refresh_rate_millihertz())
            && hz > 0
        {
            self.refresh = Duration::from_secs_f64(1000.0 / hz as f64);
        }

        let scale = window.scale_factor() as f32;
        let font_px = self.config.font_size * scale;
        self.base_font_px = Some(font_px);

        let mut renderer = match Renderer::new_with_fonts(
            window.clone(),
            crate::renderer::RendererFontConfig {
                font_family: self.config.font_family.clone(),
                font_px,
                font_features: self.config.font_features.clone(),
                font_bold: self.config.font_bold.clone(),
                font_italic: self.config.font_italic.clone(),
                font_bold_italic: self.config.font_bold_italic.clone(),
                font_symbol_map: self.config.font_symbol_map.clone(),
                font_variations: self.config.font_variations.clone(),
            },
            self.config.opacity,
        ) {
            Ok(r) => r,
            Err(e) => {
                log::error!("failed to initialize renderer: {e:#}");
                event_loop.exit();
                return;
            }
        };
        log::info!(
            "startup: renderer+GPU+font ready at {:.1} ms",
            ms(self.started)
        );
        // Apply an explicit padding override (logical px scaled to physical). A
        // value of 0 means "use the cell-derived default" (matching the settings
        // form, where 0 is the default sentinel) — without this guard, a config
        // with `padding = 0` (which the settings form writes) would force a
        // zero-margin grid that kisses the window edge.
        if let Some(pad) = self.config.padding
            && pad > 0.0
        {
            renderer.set_pad(pad * scale);
        }
        // Apply per-side padding overrides if configured.
        if let Some(pad_top) = self.config.padding_top {
            renderer.set_pad_top(pad_top * scale);
        }
        if let Some(pad_bottom) = self.config.padding_bottom {
            renderer.set_pad_bottom(pad_bottom * scale);
        }
        if let Some(pad_left) = self.config.padding_left {
            renderer.set_pad_left(pad_left * scale);
        }
        if let Some(pad_right) = self.config.padding_right {
            renderer.set_pad_right(pad_right * scale);
        }
        // Enable ligature run-shaping if the config requests it.
        renderer.set_ligatures(self.config.ligatures);

        // gpu-fx (both OFF by default): cursor trail/smear + CRT post-process.
        // Headless hooks GLASSY_CURSOR_TRAIL=1 / GLASSY_CRT=1 force them on for
        // capture verification regardless of the config.
        let cursor_trail =
            self.config.cursor_trail || std::env::var_os("GLASSY_CURSOR_TRAIL").is_some();
        renderer.set_cursor_trail(cursor_trail);

        // Window effect: GLASSY_EFFECT=<mode> overrides the config (headless capture
        // hook). The legacy GLASSY_CRT=1 still forces the CRT look. Otherwise the
        // resolved config `window_effect` (which already folds in `crt_effect`).
        let window_effect = if let Some(s) = std::env::var_os("GLASSY_EFFECT") {
            crate::renderer::WindowEffect::parse(&s.to_string_lossy())
        } else if std::env::var_os("GLASSY_CRT").is_some() {
            crate::renderer::WindowEffect::Crt
        } else {
            self.config.window_effect
        };
        self.config.window_effect = window_effect;
        renderer.set_window_effect(window_effect);

        let size = window.inner_size();
        renderer.resize(size.width, size.height);
        let m = renderer.cell_metrics();
        // At resume the first (single) tab does not yet exist, so the strip is
        // hidden in Auto mode — reserve 0 (plus the macOS traffic-light inset so
        // content clears the floating window buttons); reflowed when a 2nd tab opens.
        let strip_h = if self.tab_bar_visible() {
            tab_bar_h(m.height)
        } else {
            0.0
        }
        .max(self.chrome_top_inset());
        let (cols, rows) = Self::grid_for(
            size,
            m.width,
            m.height,
            renderer.pad_x(),
            renderer.pad_y(),
            self.config.status_bar,
            strip_h,
        );
        self.cols = cols;
        self.rows = rows;

        // First tab opens in the configured cwd (from `cwd` / an activated profile),
        // if any; otherwise the shell's default. Also seed `active_cwd` so a new
        // tab/split inherits it before the shell emits its first OSC 7.
        let initial_cwd = self.config.initial_cwd.clone();
        self.active_cwd = initial_cwd.clone();
        let pty = match Pty::spawn(
            self.proxy.clone(),
            0,
            cols,
            rows,
            m.width.round() as u16,
            m.height.round() as u16,
            self.config.shell.clone(),
            initial_cwd,
            self.config.scrollback,
            &self.config.word_separator,
            self.config.cursor_style.to_cursor_shape(),
            self.config.cursor_blink,
        ) {
            Ok(p) => p,
            Err(e) => {
                log::error!("failed to spawn shell: {e:#}");
                event_loop.exit();
                return;
            }
        };

        log::info!("startup: shell spawned at {:.1} ms", ms(self.started));
        self.window = Some(window);
        self.renderer = Some(renderer);
        self.pty = Some(pty);

        // Session restore (opt-in): replace the single initial tab with the saved
        // tabs/splits/cwds. Done before the watcher/headless hooks so the restored
        // tabs are the live set everything else operates on.
        if self.config.restore_session
            && let Some(saved) = crate::session::Session::load()
        {
            self.restore_session(saved, event_loop);
        }

        // Set up config file watcher for live reload. Uses notify crate to watch
        // the config file and send ConfigReload events when it changes (debounced).
        if let Some(config_path) = crate::config::path() {
            spawn_config_watcher(config_path, self.proxy.clone());
        }

        // Headless input/resize harness (used with GLASSY_CAPTURE for autonomous
        // verification of the custom PTY loop's write + resize paths):
        //   GLASSY_INPUT  - bytes to write through the real input channel; `\n`,
        //                   `\t`, `\e`/`\x1b` (ESC) escapes are honored, so a
        //                   capture can drive raw VT sequences (e.g. SGR 53
        //                   overline, OSC 1337 inline images, DECSET 1016 SGR-Pixel
        //                   mouse) end-to-end. Exercises the loop's `write_all` on
        //                   the blocking master fd round-trip.
        //   GLASSY_RESIZE - "COLSxROWS" to drive a grid resize (LoopMsg::Resize
        //                   -> on_resize) before the capture deadline.
        if let Some(pty) = &self.pty {
            if let Ok(spec) = std::env::var("GLASSY_RESIZE")
                && let Some((c, r)) = spec.split_once('x')
                && let (Ok(cols), Ok(rows)) = (c.parse::<usize>(), r.parse::<usize>())
            {
                let m = self.renderer.as_ref().unwrap().cell_metrics();
                pty.resize(cols, rows, m.width as u16, m.height as u16);
                self.cols = cols;
                self.rows = rows;
                self.force_full_redraw = true;
            }
            if let Ok(input) = std::env::var("GLASSY_INPUT") {
                let bytes = super::headless_input::decode_input_escapes(&input);
                pty.write(bytes);
            }
        }
        // Headless: rebuild the keymap for a chosen platform so the help panel
        // shows that platform's real chords (e.g. ⌘T) on a Linux capture build.
        //   GLASSY_KEYMAP_PLATFORM=mac|linux|windows
        // Pair with GLASSY_HELP=1 (+ GLASSY_HELP_PLATFORM=mac for HIG rendering)
        // to screenshot the macOS keybinding reference from CI on Linux.
        if let Ok(spec) = std::env::var("GLASSY_KEYMAP_PLATFORM") {
            use crate::config::Platform;
            let p = match spec.trim().to_ascii_lowercase().as_str() {
                "mac" | "macos" | "darwin" => Platform::Mac,
                "windows" | "win" => Platform::Windows,
                _ => Platform::Linux,
            };
            self.config.keymap = crate::config::keymap::default_keymap(p);
            self.force_full_redraw = true;
        }
        // Headless: seed an IME preedit (composition) overlay at startup so the
        // underlined in-progress composition can be captured. The value is the
        // composition string (defaults to a CJK sample). Re-asserted just before
        // the capture render (winit's own IME init clears the early seed).
        self.reassert_headless_preedit();
        // Headless: open an overlay at startup for capture verification.
        if std::env::var_os("GLASSY_HELP").is_some() {
            self.help_open = true;
            self.force_full_redraw = true;
        }
        if std::env::var_os("GLASSY_SETTINGS").is_some() {
            // Use the real open path so the sectioned window's custom-theme palette
            // and profile list are seeded for capture. GLASSY_SETTINGS_SECTION picks
            // the starting sidebar section (0=General .. 5=Advanced, or a name).
            self.open_settings();
            if let Ok(sec) = std::env::var("GLASSY_SETTINGS_SECTION") {
                let idx = sec.parse::<usize>().ok().unwrap_or_else(|| {
                    crate::gui::SettingsSection::ALL
                        .iter()
                        .position(|s| s.label().eq_ignore_ascii_case(sec.trim()))
                        .unwrap_or(0)
                });
                self.settings_set_section(idx);
            }
            // Headless: pre-select a custom-theme color so the editor card is
            // captured. GLASSY_SETTINGS_CUSTOM=<entry index 0..20>.
            if let Ok(idx) = std::env::var("GLASSY_SETTINGS_CUSTOM")
                && let Ok(i) = idx.parse::<usize>()
            {
                self.select_custom_color(i);
            }
            self.force_full_redraw = true;
        }
        // Headless: open settings with the cursor-cfg fields visible so the
        // feature can be captured. GLASSY_CURSOR_CFG=beam|underline|block
        // also pre-sets the cursor_style config so it shows in the form.
        if let Ok(style) = std::env::var("GLASSY_CURSOR_CFG") {
            use crate::app::CursorStyleConfig;
            self.config.cursor_style = match style.to_ascii_lowercase().as_str() {
                "beam" => CursorStyleConfig::Beam,
                "underline" => CursorStyleConfig::Underline,
                _ => CursorStyleConfig::Block,
            };
            self.open_settings();
            self.force_full_redraw = true;
        }
        if std::env::var_os("GLASSY_MENU").is_some() {
            self.menu_open = true;
            self.force_full_redraw = true;
        }
        // Headless: open the right-click terminal context menu near top-left so the
        // full rich menu (Copy/Paste/Select all/Clear/Search/Split/New tab/…) is
        // captured. Seeds a fake pointer position first.
        if std::env::var_os("GLASSY_CTXMENU").is_some() {
            self.mouse_px = (60.0, 80.0);
            self.open_context_menu(event_loop);
            self.force_full_redraw = true;
        }
        // Headless: open the TAB right-click context menu for tab 0.
        if std::env::var_os("GLASSY_TABMENU").is_some() {
            self.mouse_px = (40.0, 8.0);
            self.open_tab_menu(0, event_loop);
            self.force_full_redraw = true;
        }
        // Headless: open the command palette at startup; GLASSY_PALETTE's value (if
        // non-empty) pre-fills the query so the fuzzy filter can be captured.
        // GLASSY_PALETTE_HIST / GLASSY_PALETTE_CWD seed the history + cwd sources
        // (newline-separated entries) so the dynamic palette rows can be captured
        // without a live OSC 133 / OSC 7 producing shell.
        if let Some(q) = std::env::var_os("GLASSY_PALETTE") {
            if let Some(h) = std::env::var_os("GLASSY_PALETTE_HIST") {
                for line in h.to_string_lossy().split('\n').filter(|l| !l.is_empty()) {
                    self.record_command_history(line.to_string());
                }
            }
            if let Some(c) = std::env::var_os("GLASSY_PALETTE_CWD") {
                for line in c.to_string_lossy().split('\n').filter(|l| !l.is_empty()) {
                    self.record_cwd_history(std::path::PathBuf::from(line));
                }
            }
            self.open_palette(event_loop);
            let q = q.to_string_lossy().to_string();
            if !q.is_empty()
                && let Some(p) = self.palette.as_mut()
            {
                p.edit.set_text(&q);
                self.refilter_palette();
            }
            self.force_full_redraw = true;
        }
        // Headless: open the find bar at startup; GLASSY_SEARCH's value (if
        // non-empty) is the query, so match highlighting can be captured.
        if let Some(q) = std::env::var_os("GLASSY_SEARCH") {
            self.open_search(event_loop);
            let q = q.to_string_lossy().to_string();
            if !q.is_empty()
                && let Some(st) = self.search.as_mut()
            {
                st.edit.set_text(&q);
            }
            self.recompute_search();
            self.force_full_redraw = true;
        }
        // Headless: open N tabs at startup to capture the multi-tab toolbar.
        if let Ok(n) = std::env::var("GLASSY_TABS")
            && let Ok(n) = n.parse::<usize>()
        {
            for _ in 1..n.min(12) {
                self.new_tab(event_loop);
            }
        }
        // Headless: arm the modifier-HOLD numbered tab overlay so the number
        // badges can be captured without holding ⌘/Ctrl. Seeds the hold start far
        // enough in the past to be already past the dwell.
        if std::env::var_os("GLASSY_TAB_NUMBERS").is_some() {
            self.mod_hold_since =
                Some(Instant::now() - super::modhold::MOD_HOLD_DWELL - Duration::from_millis(1));
            self.force_full_redraw = true;
        }
        // Headless: inject a fake foreground process name on every open tab so the
        // process-aware tab label + window title can be captured without launching
        // a real child. Value = the comm to show (e.g. "vim"); empty falls back to
        // the shell name path.
        if let Ok(name) = std::env::var("GLASSY_PROCNAME")
            && !name.trim().is_empty()
        {
            let name = name.trim().to_string();
            if let Some(p) = self.pty.as_mut() {
                p.pane_info.foreground_comm = Some(name.clone());
            }
            for s in &mut self.background {
                s.pty.pane_info.foreground_comm = Some(name.clone());
            }
            self.update_window_title();
            self.force_full_redraw = true;
        }
        // Headless: inject a toast notification at startup so the toast overlay
        // can be captured (GLASSY_TOAST=1 shows a default message; any non-empty
        // value uses the value as the message text).
        if let Ok(msg) = std::env::var("GLASSY_TOAST") {
            let text = if msg.trim().is_empty() {
                "Test toast notification".to_string()
            } else {
                msg
            };
            self.push_toast(text);
            self.force_full_redraw = true;
        }
        // Headless: apply one or more remote-control requests at startup so the
        // `glassy @ <cmd>` surface can be exercised without a second process +
        // socket round-trip. GLASSY_REMOTE holds `;`-separated request lines
        // (e.g. "open-tab;split horizontal;send-text echo hi\\n;ls"); each is
        // parsed + applied via the same `apply_control` path the IPC listener
        // uses, and the reply is surfaced as a toast so it is capturable.
        if let Ok(spec) = std::env::var("GLASSY_REMOTE") {
            for line in spec.split(';').map(str::trim).filter(|l| !l.is_empty()) {
                match crate::ipc::control::parse_request(line) {
                    Ok(command) => {
                        let (req, rx) = crate::ipc::control::ControlRequest::new(command);
                        self.apply_control(&req, event_loop);
                        if let Ok(reply) = rx.try_recv() {
                            self.push_toast(reply.to_wire());
                        }
                    }
                    Err(e) => self.push_toast(format!("remote parse error: {e}")),
                }
            }
            self.force_full_redraw = true;
        }

        // Headless: open hints mode at startup so the labelled-overlay can be
        // captured (GLASSY_HINTS=1). Scans the visible grid for URLs/paths/SHAs/IPs
        // and labels each; a no-op (toast) when the screen has no targets yet.
        if std::env::var_os("GLASSY_HINTS").is_some() {
            self.open_hints(event_loop);
            self.force_full_redraw = true;
        }

        // Headless: enter keyboard copy-mode ("vi mode") at startup with a visual
        // selection so the keyboard cursor + range can be captured (GLASSY_VIMODE
        // =1 → charwise; =line / =block pick the other visual kinds).
        self.maybe_headless_vimode(event_loop);

        // Headless: enable the scrollback minimap strip and seed enough scrollback
        // that the downsampled overview has content to draw. GLASSY_MINIMAP=1
        // turns it on; the seq fills the buffer with coloured lines so the strip
        // shows structure in a capture.
        if std::env::var_os("GLASSY_MINIMAP").is_some() {
            self.config.minimap = true;
            self.minimap_cache = Default::default();
            if let Some(pty) = &self.pty {
                // A burst of coloured output so the buffer has history to map.
                pty.write(
                    b"for i in $(seq 1 300); do printf '\\033[3%dm line %d \\033[0m\\n' \
                      $((i%7+1)) $i; done\n"
                        .to_vec(),
                );
            }
            self.force_full_redraw = true;
        }

        // Headless: split the active tab at startup to capture the multi-pane path.
        //   v = one vertical (left|right) split, h = one horizontal (top/bottom),
        //   grid = both (a 2x2 quad).
        if let Ok(spec) = std::env::var("GLASSY_SPLIT") {
            match spec.as_str() {
                "v" => self.split_pane(pane::Dir::Vertical, event_loop),
                "h" => self.split_pane(pane::Dir::Horizontal, event_loop),
                "grid" => {
                    self.split_pane(pane::Dir::Vertical, event_loop);
                    self.split_pane(pane::Dir::Horizontal, event_loop);
                    self.focus_pane(pane::Move::Left, event_loop);
                    self.split_pane(pane::Dir::Horizontal, event_loop);
                }
                _ => {}
            }
        }
        // Headless: split the active tab (vertical) with dim-unfocused forced on, so
        // the dimmed-content path (subtle dark overlay over the non-focused tile)
        // can be captured. Splits first if needed so there is something to dim.
        if std::env::var_os("GLASSY_DIM").is_some() {
            self.config.dim_unfocused = true;
            if !self.is_split() {
                self.split_pane(pane::Dir::Vertical, event_loop);
            }
            self.force_full_redraw = true;
        }
        // Headless: split the active tab and zoom the focused pane at startup so
        // the pane-zoom path (focused pane maximized, others hidden, ZOOM badge)
        // can be captured. Splits first if needed so there is something to zoom.
        if std::env::var_os("GLASSY_ZOOM").is_some() {
            if !self.is_split() {
                self.split_pane(pane::Dir::Vertical, event_loop);
            }
            self.toggle_zoom(event_loop);
            self.force_full_redraw = true;
        }
        // Headless: enable broadcast input at startup (splits the tab first so
        // there are multiple panes to broadcast to, and turns the status bar on
        // so the BCAST indicator is visible) so the indicator + the multi-pane
        // fan-out can be captured.
        if std::env::var_os("GLASSY_BROADCAST").is_some() {
            if !self.is_split() {
                self.split_pane(pane::Dir::Vertical, event_loop);
            }
            self.broadcast_input = true;
            self.config.status_bar = true;
            self.force_full_redraw = true;
        }

        // Headless: show an inline file-peek card at startup so the preview overlay
        // can be captured. GLASSY_PEEK=<path> peeks that file; GLASSY_PEEK=1 (or an
        // empty value) falls back to this source file so there's always something.
        if let Ok(spec) = std::env::var("GLASSY_PEEK") {
            let path = if spec.trim().is_empty() || spec == "1" {
                "src/app/peek.rs".to_string()
            } else {
                spec
            };
            self.show_peek(std::path::Path::new(&path));
            self.force_full_redraw = true;
        }

        // Headless: generate and apply a theme from an image path at startup so the
        // resulting colour palette can be captured.
        //   GLASSY_THEME_GEN_IMAGE=/path/to/wall.png   — apply a generated theme
        if let Ok(path) = std::env::var("GLASSY_THEME_GEN_IMAGE")
            && !path.is_empty()
        {
            self.apply_theme_from_image_path(&path, event_loop);
        }

        // Headless: inject synthetic OSC-133 command blocks so the Warp-style
        // exit-status/duration badges (and a folded block) can be captured without
        // a real shell-integration script. GLASSY_CMDBLOCK=1 seeds a few blocks.
        if std::env::var_os("GLASSY_CMDBLOCK").is_some() {
            self.inject_demo_command_blocks();
            self.force_full_redraw = true;
        }

        // Headless: inject a synthetic kitty inline image into the active pane's
        // image store so the kitty-graphics renderer path can be captured without a
        // real image-producing program. GLASSY_IMGDEMO=1 synthesises a small RGBA
        // colour swatch (8×8, rainbow gradient) and places it at grid row 2, col 2.
        // This exercises the full render path (ImageStore -> renderer draw quads)
        // without touching the PTY byte stream. Hook name: GLASSY_IMGDEMO.
        if std::env::var_os("GLASSY_IMGDEMO").is_some() {
            self.inject_demo_inline_image();
            self.force_full_redraw = true;
        }

        // Quake / dropdown mode: reconfigure the window to be borderless,
        // top-anchored, always-on-top, and start a slide-in. Done before the first
        // render so the window is already positioned + sized when shown. A no-op
        // (leaving normal windowed mode intact) unless `config.quake` is set.
        // `GLASSY_QUAKE=1` forces it on for headless capture even if the config is
        // off, and `GLASSY_QUAKE_OPEN=1` snaps it fully open (progress=1) so the
        // capture frame shows the dropped window rather than mid-slide.
        if std::env::var_os("GLASSY_QUAKE").is_some() {
            self.config.quake = true;
        }
        self.init_quake(event_loop);
        if std::env::var_os("GLASSY_QUAKE_OPEN").is_some()
            && let Some(q) = self.quake.as_mut()
        {
            q.progress = 1.0;
            q.shown = true;
            q.animating = false;
        }

        // Draw the first frame, then reveal the window (avoids a white flash).
        self.next_frame = Instant::now();
        self.render();
        if let Some(window) = &self.window {
            // In quake mode the slide animation owns visibility (a slide-in reveals
            // it); a settled snapped-open quake state still needs an explicit show.
            // Otherwise (normal mode) reveal unconditionally as before.
            match self.quake.as_ref() {
                Some(q) if q.animating => {
                    // The first step_quake will set_visible(true) on the slide-in.
                    window.set_visible(true);
                }
                Some(q) => window.set_visible(q.shown),
                None => window.set_visible(true),
            }
        }

        if self.capture.is_some() {
            // Delay before capturing so the shell + prompt (e.g. zsh + starship)
            // have time to initialize. Override with GLASSY_CAPTURE_MS.
            let ms: u64 = std::env::var("GLASSY_CAPTURE_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(700);
            let deadline = Instant::now() + Duration::from_millis(ms);
            self.capture_deadline = Some(deadline);
            event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
        }

        // Scripted-input harness: when GLASSY_SCRIPT is set, drive the real
        // mouse/keyboard/render handlers from the parsed command list (one step per
        // about_to_wait wake) and exit when done. Armed last so it can ride on top
        // of any overlay opened above (GLASSY_SETTINGS/MENU/…). The normal path
        // leaves `self.script` None, so the 0%-idle invariant is untouched.
        self.maybe_start_script(event_loop);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        // Dispatch to the extracted handler in user_event.rs (keeps this file
        // under the 700-line limit while preserving the trait impl here).
        user_event::dispatch(self, event_loop, event);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                if let Some(pty) = &self.pty {
                    pty.shutdown();
                }
                event_loop.exit();
            }
            WindowEvent::Focused(focused) => {
                self.focused = focused;
                // DECSET 1004 focus reporting: notify each pane's child that asked
                // for it. \x1b[I on focus-in, \x1b[O on focus-out. Per-PTY because a
                // split tab runs an independent program in every pane.
                let seq: &[u8] = if focused { b"\x1b[I" } else { b"\x1b[O" };
                self.report_focus(seq);
                // Restart the blink solid-on so a freshly-focused window shows the
                // cursor immediately; the cadence resumes from about_to_wait.
                self.reset_blink();
                self.mark_dirty(event_loop);
            }
            WindowEvent::ThemeChanged(scheme) => {
                // The system light/dark color-scheme changed at runtime. When
                // `follow_system` is on, swap to `theme_light`/`theme_dark` to match
                // — glassy now ships real LIGHT themes, so Light mode actually goes
                // light. When following is off we keep the pinned `theme` but still
                // re-assert it (safe, repeatable) so winit's re-themed CSD titlebar
                // stays coherent with our palette.
                if !self.apply_system_theme(Some(scheme))
                    && let Some(theme) = color::theme_by_name(&self.config.theme)
                {
                    color::set_theme(theme);
                }
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
            }
            WindowEvent::ModifiersChanged(mods) => {
                self.mods = mods.state();
                // Arm/disarm the modifier-hold numbered tab overlay (⌘-hold on
                // macOS / Ctrl-hold elsewhere). Idle-safe: only schedules a single
                // dwell wake; cleared on release or any non-modifier key.
                self.update_mod_hold(event_loop);
            }
            WindowEvent::KeyboardInput {
                event,
                is_synthetic,
                ..
            } => {
                // Synthetic events are injected on focus change for held keys.
                if is_synthetic {
                    return;
                }
                self.handle_keyboard(event, event_loop);
            }
            // CJK / dead-key composition. All four Ime sub-events are handled by
            // the dedicated state machine in ime.rs (keeps this file additive and
            // under the line limit).
            WindowEvent::Ime(ime) => self.handle_ime(ime, event_loop),
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved(position, event_loop);
            }
            WindowEvent::MouseInput { state, button, .. } => {
                self.handle_mouse_input(state, button, event_loop);
            }
            WindowEvent::MouseWheel { delta, phase, .. } => {
                self.handle_mouse_wheel(delta, phase, event_loop);
            }
            WindowEvent::Resized(size) => self.handle_resize(event_loop, size),
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                // Moving to a different-DPI monitor changes the logical->physical
                // ratio. Reload the font at the new physical px first (otherwise
                // glyphs stay rasterized at the old DPI), then let handle_resize
                // reproject the grid against the new surface.
                let scale = scale_factor as f32;
                let font_px = self.config.font_size * scale;
                if let Some(r) = self.renderer.as_mut() {
                    r.set_font_size(font_px);
                    self.base_font_px = Some(font_px);
                }
                if let Some(w) = &self.window {
                    self.handle_resize(event_loop, w.inner_size());
                }
                // A DPI / monitor change moves the quake anchor; re-derive + reposition.
                self.quake_refresh_geometry(event_loop);
            }
            WindowEvent::RedrawRequested => self.render(),

            // Window moved to a different position (e.g. dragged to another
            // monitor). In quake mode the drop anchor is tied to the current
            // monitor's top-left; re-derive + reposition so a window dragged
            // across monitors doesn't slide off the wrong monitor edge.
            // In normal mode this is a no-op (quake is None).
            WindowEvent::Moved(_) => {
                self.quake_refresh_geometry(event_loop);
            }

            // The compositor reveals the window after it was fully hidden.
            // Force a full repaint so the restored content is fresh. When the
            // window is hidden (occluded=true) we do nothing — the 0%-idle
            // `Wait` path already suppresses GPU submits.
            WindowEvent::Occluded(false) => {
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
            }

            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Periodically refresh /proc-based pane info (cwd + foreground process).
        // Only done for panes of the active tab; background tabs refresh on focus.
        // This is cheap (a few symlink reads) and keeps the header/status bar live.
        // Track the active pane's process name AND cwd across the refresh so a
        // change (launching vim, returning to the prompt, or `cd`-ing) re-derives
        // the window title + tab label and schedules a repaint — without this a
        // process-aware/cwd title would only update on OSC/tab events. The cwd
        // check matters for shells that don't emit OSC 7: the 2 s poll picks up the
        // new cwd and this refreshes the title.
        let snapshot = |app: &Self| {
            app.pty
                .as_ref()
                .map(|p| (p.pane_info.foreground_comm.clone(), p.pane_info.cwd.clone()))
        };
        let before = snapshot(self);
        if let Some(pty) = self.pty.as_mut() {
            Self::maybe_refresh_proc_info(pty);
        }
        let after = snapshot(self);
        if before != after {
            // Only the derived title needs updating when no OSC/custom title is set;
            // update unconditionally (cheap) and repaint so the tab chip re-shapes.
            self.update_window_title();
            self.force_full_redraw = true;
            self.mark_dirty(event_loop);
        }
        if let Some(g) = self.panes.as_mut() {
            for pty in g.others.values_mut() {
                Self::maybe_refresh_proc_info(pty);
            }
        }

        // Flush a pending session re-persist (tab/split structure changed). Gated on
        // `restore_session`; cheap (a small JSON write) and coalesced to once per
        // settle. The authoritative save also happens in `exiting`.
        if self.session_dirty {
            self.session_dirty = false;
            self.save_session();
        }

        // Scripted-input harness: advance one command per wake, driving the real
        // handlers. While a script is in flight we stay on `Poll`; once it runs
        // out of commands the loop exits. Checked before the capture path so a
        // script can own the run entirely (it has its own `capture` command).
        if self.script.is_some() {
            if self.step_script(event_loop) {
                event_loop.set_control_flow(ControlFlow::Poll);
            } else {
                event_loop.exit();
            }
            return;
        }

        // Headless capture path: at the deadline, render the latest content,
        // dump it to disk, and exit.
        if let Some(deadline) = self.capture_deadline {
            if Instant::now() >= deadline {
                // Headless search verification: the GLASSY_SEARCH hook runs in
                // `resumed()` before the shell's output lands, so recompute the
                // match list against the now-populated grid before capturing.
                if self.search.is_some() {
                    self.recompute_search();
                }
                // Headless IME verification: winit emits its own Ime::Enabled +
                // empty Ime::Preedit on window init, which clears the preedit the
                // GLASSY_IME hook seeded in resumed(). Re-assert it here (right
                // before the capture render) so the composition overlay is present
                // in the captured frame, anchored at the now-settled cursor.
                self.reassert_headless_preedit();
                // Headless tab-number overlay: winit's initial empty
                // ModifiersChanged clears the GLASSY_TAB_NUMBERS-armed hold, so
                // re-arm it (past the dwell) right before the capture render.
                if std::env::var_os("GLASSY_TAB_NUMBERS").is_some() {
                    self.mod_hold_since = Some(
                        Instant::now() - super::modhold::MOD_HOLD_DWELL - Duration::from_millis(1),
                    );
                }
                let split = self.is_split();
                self.render();
                if let (Some(renderer), Some(path)) =
                    (self.renderer.as_mut(), self.capture.as_ref())
                {
                    // A split tab builds the multi-pane instance lists; capture
                    // those, otherwise the single-grid path.
                    let res = if split {
                        renderer.capture_multi(path)
                    } else {
                        renderer.capture(path)
                    };
                    match res {
                        Ok(()) => log::info!("captured frame to {}", path.display()),
                        Err(e) => log::error!("capture failed: {e:#}"),
                    }
                }
                event_loop.exit();
                return;
            }
            event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
            return;
        }

        let now = Instant::now();

        // Real-GUI chrome animations: while any widget animation (hover fade,
        // toggle slide) is unsettled, advance it and keep the frame dirty so the
        // chrome repaints. This is the ONLY case where we run `ControlFlow::Poll`;
        // once everything settles we fall back to `Wait` (0% idle).
        let gui_active = if gui::any_unsettled(&self.gui_anims) {
            let dt = (now - self.gui_anim_last).as_secs_f32().min(0.1);
            gui::step_anims(&mut self.gui_anims, dt, 12.0);
            self.gui_anim_last = now;
            self.dirty = true;
            // Drop settled animations so the map can't grow without bound across a
            // long session (every transient hover/press inserts an entry). A widget
            // re-creates its entry lazily on next access via `or_insert_with`, which
            // seeds it AT the current target — so pruning a resting entry causes no
            // animation restart / flicker.
            self.gui_anims.retain(|_, a| !a.is_settled());
            true
        } else {
            // Everything has settled: prune the whole map in one pass (same
            // no-flicker rationale as above).
            if !self.gui_anims.is_empty() {
                self.gui_anims.retain(|_, a| !a.is_settled());
            }
            self.gui_anim_last = now;
            false
        };

        // Quake slide: while a drop/retract is in flight, advance it and keep the
        // frame dirty so the window repositions every frame. Like the GUI anims it
        // runs the loop on `Poll`; once the slide settles `step_quake` returns false
        // and we fall back to `Wait` (0% idle). A no-op in normal windowed mode.
        let quake_active = if self.quake_animating() {
            self.step_quake(now)
        } else {
            false
        };

        // Cursor blink: only runs while focused and the child asked for a blinking
        // cursor. When that holds, advance the phase at each `blink_at` deadline and
        // mark dirty so the cursor redraws; otherwise the cursor stays solid and we
        // never schedule a wakeup for it (preserving the 0%-idle `Wait` path).
        let blink_active = self.focused && self.cursor_blinks;
        if blink_active {
            if now >= self.blink_at {
                self.blink_on = !self.blink_on;
                self.blink_at = now + BLINK_INTERVAL;
                self.dirty = true;
            }
        } else {
            // Settle to the solid (visible) phase so re-focusing shows the cursor.
            self.blink_on = true;
        }

        // gpu-fx cursor trail (config `cursor_trail`, off by default): while the
        // cursor is mid-glide, advance the eased position one step and keep the
        // frame dirty so the smear repaints. A full redraw is forced so the smear
        // (which spans pixels across several grid rows on a multi-row jump) clears
        // cleanly each frame. When the trail settles `step_cursor_trail` returns
        // false and we stop scheduling — back to `Wait`, 0% idle. Entirely dormant
        // when the feature is off (the renderer reports not-animating).
        let trail_active = if let Some(r) = self.renderer.as_mut() {
            if r.cursor_trail_animating() {
                r.step_cursor_trail();
                self.dirty = true;
                self.force_full_redraw = true;
                true
            } else {
                false
            }
        } else {
            false
        };

        // Text blink (SGR 5/6): runs while the active session has blinking cells.
        // Drives a periodic phase flip at the same cadence as the cursor blink so
        // the UI redraws and the render path can suppress blinking cells. When the
        // window loses focus we freeze in the visible phase (cells always shown).
        if self.text_blink_active {
            if self.focused {
                if now >= self.text_blink_at {
                    self.text_blink_on = !self.text_blink_on;
                    self.text_blink_at = now + BLINK_INTERVAL;
                    self.dirty = true;
                }
            } else {
                // Unfocused: freeze visible so nothing flickers in background tabs.
                self.text_blink_on = true;
            }
        }

        // Visual-bell flash: while the flash window is open, keep redrawing so the
        // overlay is painted; once it elapses, restore (a full rebuild drops the
        // tint from every cell) and repaint one last frame. This is a short, finite
        // wake; idle returns to `Wait` afterward.
        let flash_active = match self.bell_flash_until {
            Some(until) if now < until => true,
            Some(_) => {
                // Flash just ended: clear it and force the restore frame.
                self.bell_flash_until = None;
                self.force_full_redraw = true;
                self.dirty = true;
                false
            }
            None => false,
        };

        // Tab busy-spinner: while any tab is busy, advance one glyph at each
        // `spinner_at` deadline and repaint so the chip animates. Once a session's
        // busy window lapses, clear it (so its chip stops spinning) and repaint one
        // last frame. This is a finite, self-extending wake; when nothing is busy
        // we never schedule a spinner wakeup and idle returns to `Wait`.
        let mut busy_lapsed = false;
        if self.active_busy_until.is_some_and(|t| now >= t) {
            self.active_busy_until = None;
            busy_lapsed = true;
        }
        for s in &mut self.background {
            if s.busy_until.is_some_and(|t| now >= t) {
                s.busy_until = None;
                busy_lapsed = true;
            }
        }
        let spin_active = self.any_tab_busy(now);
        if spin_active {
            if now >= self.spinner_at {
                self.spinner_frame = self.spinner_frame.wrapping_add(1);
                self.spinner_at = now + SPINNER_INTERVAL;
                self.dirty = true;
            }
        } else {
            // Settle the phase so the next busy burst starts on the first frame.
            self.spinner_frame = 0;
        }
        if busy_lapsed {
            self.dirty = true;
        }

        // Toast notifications: include their next deadline in the wakeup schedule
        // so fade animations keep ticking even when nothing else is dirty.
        let toast_deadline = crate::app::toast::next_deadline(&self.toasts);
        if toast_deadline.is_some_and(|deadline| now >= deadline) {
            // A toast phase transition happened; force a repaint.
            self.dirty = true;
        }

        // Modifier-hold numbered tab overlay: while armed-but-not-yet-shown we
        // wake at the dwell deadline to paint the badges once. `mod_hold_deadline`
        // returns None the moment it becomes visible, so this is a single finite
        // wake; idle returns to `Wait`.
        let mod_hold_deadline = self.mod_hold_deadline();
        if self.mod_hold_since.is_some() && mod_hold_deadline.is_none() {
            // Just crossed the dwell: the overlay is now visible — repaint it.
            self.force_full_redraw = true;
            self.dirty = true;
        }

        if !self.dirty {
            // Idle: stay parked on `Wait` (0% CPU) unless a blink flip, a flash
            // boundary, or a spinner frame is pending — then wake at the earliest.
            // A live GUI animation or quake slide overrides everything with `Poll`
            // until it settles.
            if gui_active || quake_active {
                event_loop.set_control_flow(ControlFlow::Poll);
            } else {
                let wake = [
                    self.next_wake(blink_active, flash_active, spin_active),
                    toast_deadline,
                    mod_hold_deadline,
                ]
                .into_iter()
                .flatten()
                .min();
                match wake {
                    Some(at) => event_loop.set_control_flow(ControlFlow::WaitUntil(at)),
                    None => event_loop.set_control_flow(ControlFlow::Wait),
                }
            }
            return;
        }

        // Deferred confirm-close execution: the render path sets this flag when the
        // "Close" button is clicked; we execute here where ActiveEventLoop is available.
        if self.pending_confirm_execute {
            self.pending_confirm_execute = false;
            let pending = self.confirm_close.take();
            match pending {
                Some(ConfirmClose::ActiveTab) => self.close_active_tab(event_loop),
                Some(ConfirmClose::ActivePane) => self.close_pane(event_loop),
                None => {}
            }
        }

        if now >= self.next_frame {
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            self.next_frame = now + self.refresh;
            // RedrawRequested will clear `dirty`. Keep a wakeup scheduled for the
            // next blink flip, flash boundary, or spinner frame; else wait for an
            // event. A live GUI animation, an in-flight cursor trail, OR a quake
            // slide keeps us on `Poll` until it settles (all hard-stop to `Wait`
            // once done).
            if gui_active || trail_active || quake_active {
                event_loop.set_control_flow(ControlFlow::Poll);
            } else {
                match self.next_wake(blink_active, flash_active, spin_active) {
                    Some(at) => event_loop.set_control_flow(ControlFlow::WaitUntil(at)),
                    None => event_loop.set_control_flow(ControlFlow::Wait),
                }
            }
        } else {
            event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
        }
    }

    /// Persist (or clear) the session on a clean exit. When `restore_session` is on,
    /// write the current tabs/splits/cwds so the next launch restores them; when
    /// off, remove any stale state file so a prior session never resurrects.
    /// Also saves the wgpu pipeline cache so the next launch avoids shader
    /// recompilation (meaningful on Vulkan; a no-op on other backends).
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if self.config.restore_session {
            self.save_session();
        } else {
            crate::session::Session::clear();
        }
        if let Some(renderer) = &self.renderer {
            renderer.save_pipeline_cache();
        }
    }
}

#[cfg(target_os = "macos")]
impl App {
    /// Turn off AppKit's automatic title-bar window dragging. With the title bar
    /// hidden + fullsize content view, AppKit would move the window whenever the
    /// user drags anywhere in the top band — including our tab chips, which should
    /// reorder instead. glassy re-implements window dragging for *empty* chrome
    /// areas via `Window::drag_window()` (see `strip_click`), so the only behavior
    /// removed here is the auto-drag that was stealing tab-reorder gestures.
    fn disable_macos_window_drag(window: &Window) {
        use objc2_app_kit::NSView;
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

        let Ok(handle) = window.window_handle() else {
            return;
        };
        if let RawWindowHandle::AppKit(h) = handle.as_raw() {
            // SAFETY: AppKit window handles carry a valid, retained NSView pointer
            // for the window's lifetime; we only read its containing NSWindow.
            unsafe {
                let view: &NSView = &*(h.ns_view.as_ptr() as *const NSView);
                if let Some(ns_window) = view.window() {
                    ns_window.setMovable(false);
                }
            }
        }
    }
}

// Free-function helpers (`fire_desktop_notification`, `spawn_config_watcher`)
// live in helpers.rs so this file stays under the line-count goal.
