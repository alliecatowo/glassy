//! winit ApplicationHandler implementation: lifecycle and window events.
//!
//! The `window_event` match arm dispatches each `WindowEvent` variant to a
//! focused handler function. Keyboard, cursor-motion, mouse-button, and
//! mouse-wheel handling live in `keys.rs`; lifecycle helpers (`resumed`,
//! `user_event`, `about_to_wait`) remain here.

use super::*;

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
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("failed to create window: {e}");
                event_loop.exit();
                return;
            }
        };
        window.set_ime_allowed(true);
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

        let mut renderer = match Renderer::new(
            window.clone(),
            self.config.font_family.clone(),
            font_px,
            self.config.opacity,
            self.config.font_features.clone(),
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
        // Apply an explicit padding override (logical px scaled to physical).
        if let Some(pad) = self.config.padding {
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

        let size = window.inner_size();
        renderer.resize(size.width, size.height);
        let m = renderer.cell_metrics();
        let (cols, rows) = Self::grid_for(
            size,
            m.width,
            m.height,
            renderer.pad(),
            self.config.status_bar,
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
        //   GLASSY_INPUT  - bytes to write through the real input channel; `\n`
        //                   and `\t` escapes are honored. Exercises the loop's
        //                   `write_all` on the blocking master fd round-trip.
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
                let bytes = input.replace("\\n", "\n").replace("\\t", "\t").into_bytes();
                pty.write(bytes);
            }
        }
        // Headless: open an overlay at startup for capture verification.
        if std::env::var_os("GLASSY_HELP").is_some() {
            self.help_open = true;
            self.force_full_redraw = true;
        }
        if std::env::var_os("GLASSY_SETTINGS").is_some() {
            self.settings_open = true;
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
        if let Some(q) = std::env::var_os("GLASSY_PALETTE") {
            self.open_palette(event_loop);
            let q = q.to_string_lossy().to_string();
            if !q.is_empty()
                && let Some(p) = self.palette.as_mut()
            {
                p.query = q;
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
                st.query = q;
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

        // Draw the first frame, then reveal the window (avoids a white flash).
        self.next_frame = Instant::now();
        self.render();
        if let Some(window) = &self.window {
            window.set_visible(true);
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
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Title(id, title) => {
                // The focused pane drives the chip/window title. Non-focused panes
                // of the active tab have their title stored in `others_titles` for
                // the pane title-bar headers (Wave 3). After a split the focused
                // leaf id != active_id, so resolve via active_focused_id().
                if id == self.active_focused_id() {
                    // Also keep the focused pane's title in others_titles so the
                    // header painter can look up any pane id uniformly.
                    if let Some(g) = self.panes.as_mut() {
                        g.others_titles.insert(id, title.clone());
                    }
                    self.active_title = title;
                    self.update_window_title();
                } else if let Some(g) = self.panes.as_mut()
                    && g.others.contains_key(&id)
                {
                    // Non-focused pane of the active tab.
                    g.others_titles.insert(id, title);
                } else if let Some(s) = self.background.iter_mut().find(|s| s.id == id) {
                    // Parked tab (single-pane or its focused pane).
                    s.title = title.clone();
                    // Also store in background tab's pane group if it's multi-pane.
                    if let Some(g) = s.panes.as_mut() {
                        g.others_titles.insert(id, title);
                    }
                }
            }
            UserEvent::ChildExit(id) => {
                self.handle_child_exit(id, event_loop);
                return;
            }
            UserEvent::Bell(id) => {
                // Ring for any pane of the active tab (the visible one).
                if self.id_in_active_tab(id) {
                    self.trigger_bell();
                }
            }
            // A background tab produced output: its terminal state updated
            // silently; no redraw needed until it becomes active.
            UserEvent::Wakeup(id) => {
                // (Re)arm this session's busy window: a wakeup means it just emitted
                // output, so its chip spins until BUSY_LINGER elapses with no more.
                // about_to_wait advances the spinner and clears the deadline (and
                // keeps a finite wakeup scheduled) exactly like the bell flash.
                let busy = Instant::now() + BUSY_LINGER;
                // Output from a NON-focused pane of the ACTIVE tab is visible, so
                // mark the active tab busy and repaint just like the focused pane.
                if id != self.active_id && self.id_in_active_tab(id) {
                    self.active_busy_until = Some(busy);
                    self.mark_dirty(event_loop);
                    return;
                }
                if id != self.active_id {
                    // A background tab produced output (in any of its panes): flag
                    // its chip for the activity dot. Only repaint on the false->true
                    // edge so a busy background tab doesn't spam redraws.
                    let owner = self
                        .tab_pos_of_pane(id)
                        .and_then(|p| self.tab_order.get(p).copied());
                    if let Some(owner) = owner
                        && let Some(s) = self.background.iter_mut().find(|s| s.id == owner)
                    {
                        let was_busy = s.busy_until.is_some_and(|t| Instant::now() < t);
                        s.busy_until = Some(busy);
                        if !s.activity || !was_busy {
                            s.activity = true;
                            self.mark_dirty(event_loop);
                        }
                    }
                    return;
                }
                self.active_busy_until = Some(busy);
            }
            UserEvent::PtyWrite(id, text) => {
                // Route the VT reply back to the exact pane that produced it (any
                // tab, any split pane); not a visual change, so no repaint.
                let bytes = text.into_bytes();
                if let Some(pty) = self.pty_by_id(id) {
                    pty.write(bytes);
                }
                return;
            }
            UserEvent::Cwd(id, path) => {
                // OSC 7: record the reporting pane's cwd so new tabs/splits inherit
                // it. Only a tab's FOCUSED pane drives the inherited cwd (mirrors
                // the title handling); not a visual change, so no repaint.
                if self.id_in_active_tab(id) {
                    if id == self.active_focused_id() {
                        self.active_cwd = Some(path);
                    } else {
                        // A non-focused active-tab pane reports its own cwd: the
                        // focused pane's stays the tab's inherited cwd, but we still
                        // record the per-pane cwd for session persistence.
                        self.active_pane_cwds.insert(id, path);
                    }
                } else {
                    // A pane of a parked tab. The focused pane (id == tab id) drives
                    // last_cwd; a non-focused pane records into pane_cwds.
                    for s in self.background.iter_mut() {
                        if s.id == id {
                            s.last_cwd = Some(path);
                            break;
                        }
                        if s.panes.as_ref().is_some_and(|g| g.others.contains_key(&id)) {
                            s.pane_cwds.insert(id, path);
                            break;
                        }
                    }
                }
                return;
            }
            UserEvent::ClipboardStore(_id, _ty, mut text) => {
                // OSC 52 copy: write to the OS clipboard on the UI thread (arboard
                // must not run on the PTY thread). arboard exposes only the standard
                // clipboard, so a Selection store also lands there. Not visual.
                // Cap the payload (~1 MiB) so a hostile / runaway program can't push
                // the clipboard to an unbounded size; truncate on a char boundary.
                const OSC52_MAX: usize = 1 << 20;
                if text.len() > OSC52_MAX {
                    let mut end = OSC52_MAX;
                    while end > 0 && !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    text.truncate(end);
                    log::debug!("OSC 52 store truncated to {end} bytes");
                }
                if let Some(cb) = self.clipboard()
                    && let Err(e) = cb.set_text(text)
                {
                    log::debug!("OSC 52 clipboard store failed: {e}");
                }
                return;
            }
            UserEvent::ClipboardLoad(id, _ty, formatter) => {
                // OSC 52 read: read the clipboard, format the reply, and write it
                // back to the requesting pane over the PtyWrite path. Not visual.
                let text = self.clipboard().and_then(|cb| match cb.get_text() {
                    Ok(t) => Some(t),
                    Err(e) => {
                        log::debug!("OSC 52 clipboard load failed: {e}");
                        None
                    }
                });
                if let Some(text) = text
                    && let Some(pty) = self.pty_by_id(id)
                {
                    pty.write(formatter.0(&text).into_bytes());
                }
                return;
            }
            UserEvent::SemanticMark(_id, _mark) => {
                // OSC 133 semantic mark: the PromptTracker on the Pty already
                // recorded the row offset for 'A' marks. No redraw needed; this
                // event is a notification hook for future UI use (e.g. prompt-count
                // in the status bar or Shift+Up/Down keybind wiring in app/input.rs).
                return;
            }
            UserEvent::Notification(_id, text) => {
                // OSC 9 / OSC 777: fire a desktop notification when the window is
                // not focused so background jobs can alert the user. When the window
                // is focused the shell's own output is visible, so skip to avoid
                // spurious notifications from noisy programs.
                if !self.focused {
                    fire_desktop_notification("glassy", &text);
                }
                return;
            }
            UserEvent::ConfigReload => {
                // Config file changed; reload from disk and apply live-reloadable settings.
                match crate::config::Settings::resolve(std::iter::empty()) {
                    Ok(Some(settings)) => {
                        self.apply_config_reload(&settings.config);
                    }
                    Ok(None) => {
                        log::debug!("config reload: --help/--version");
                    }
                    Err(e) => {
                        log::warn!("config reload failed: {e}");
                    }
                }
                return;
            }
            UserEvent::ModifyOtherKeys(id, level) => {
                // xterm modifyOtherKeys level changed by the running application
                // (CSI > 4 ; N m intercepted in the PTY loop). Update the field so
                // subsequent encode_key calls emit the correct encoding for modified
                // printable keys. Only the active focused pane's level applies
                // (keystrokes route to self.pty, whose id is active_focused_id()).
                if id == self.active_focused_id() {
                    self.modify_other_keys = level;
                }
                return;
            }
            UserEvent::Progress(id, state) => {
                // OSC 9;4 progress report: update the active session's indicator.
                // Non-active sessions' progress is ignored (only the focused session
                // renders a progress bar). On Remove, clear the indicator. Resolve
                // the focused pane via active_focused_id() (split-aware).
                if id == self.active_focused_id() {
                    self.active_progress = match state {
                        crate::image::ProgressState::Remove => None,
                        other => Some(other),
                    };
                }
                // Progress changes are visual — mark dirty so the status bar repaints.
            }
            UserEvent::TextBlinkPresent(id) => {
                // SGR 5/6 (text blink) detected in the byte stream of the active
                // session. Arm the text-blink timer so `about_to_wait` drives phase
                // flips and periodic redraws (like the cursor-blink timer). Only the
                // active focused pane can have its blinking cells visible on screen.
                if id == self.active_focused_id() && !self.text_blink_active {
                    self.text_blink_active = true;
                    self.text_blink_on = true;
                    self.text_blink_at = Instant::now() + BLINK_INTERVAL;
                }
                // Already active or not our session: still mark dirty for the redraw.
            }
        }
        self.mark_dirty(event_loop);
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
            WindowEvent::Ime(winit::event::Ime::Commit(text)) => {
                // Committed IME text is input like any keystroke: reset the blink,
                // drop the selection, snap to the prompt, and repaint even if the
                // child stays quiet.
                self.reset_blink();
                self.clear_selection();
                if let Some(pty) = &self.pty {
                    pty.term.lock().scroll_display(Scroll::Bottom);
                    pty.write(text.into_bytes());
                }
                self.mark_dirty(event_loop);
            }
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
            }
            WindowEvent::RedrawRequested => self.render(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Periodically refresh /proc-based pane info (cwd + foreground process).
        // Only done for panes of the active tab; background tabs refresh on focus.
        // This is cheap (a few symlink reads) and keeps the header/status bar live.
        if let Some(pty) = self.pty.as_mut() {
            Self::maybe_refresh_proc_info(pty);
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

        if !self.dirty {
            // Idle: stay parked on `Wait` (0% CPU) unless a blink flip, a flash
            // boundary, or a spinner frame is pending — then wake at the earliest.
            // A live GUI animation overrides everything with `Poll` until it settles.
            if gui_active {
                event_loop.set_control_flow(ControlFlow::Poll);
            } else {
                match self.next_wake(blink_active, flash_active, spin_active) {
                    Some(at) => event_loop.set_control_flow(ControlFlow::WaitUntil(at)),
                    None => event_loop.set_control_flow(ControlFlow::Wait),
                }
            }
            return;
        }

        if now >= self.next_frame {
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            self.next_frame = now + self.refresh;
            // RedrawRequested will clear `dirty`. Keep a wakeup scheduled for the
            // next blink flip, flash boundary, or spinner frame; else wait for an
            // event. A live GUI animation keeps us on `Poll` until it settles.
            if gui_active {
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
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if self.config.restore_session {
            self.save_session();
        } else {
            crate::session::Session::clear();
        }
    }
}

// Free-function helpers (`fire_desktop_notification`, `spawn_config_watcher`)
// live in helpers.rs so this file stays under the line-count goal.
