//! winit ApplicationHandler implementation: lifecycle and window events.

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
        ) {
            Ok(r) => r,
            Err(e) => {
                log::error!("failed to initialize renderer: {e:#}");
                event_loop.exit();
                return;
            }
        };
        log::info!("startup: renderer+GPU+font ready at {:.1} ms", ms(self.started));
        // Apply an explicit padding override (logical px scaled to physical).
        if let Some(pad) = self.config.padding {
            renderer.set_pad(pad * scale);
        }

        let size = window.inner_size();
        renderer.resize(size.width, size.height);
        let m = renderer.cell_metrics();
        let (cols, rows) = Self::grid_for(size, m.width, m.height, renderer.pad(), self.config.status_bar);
        self.cols = cols;
        self.rows = rows;

        let pty = match Pty::spawn(
            self.proxy.clone(),
            0,
            cols,
            rows,
            m.width.round() as u16,
            m.height.round() as u16,
            self.config.shell.clone(),
            None,
            self.config.scrollback,
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
                // the pane title-bar headers (Wave 3).
                if id == self.active_id {
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
                    let owner = self.tab_pos_of_pane(id).and_then(|p| self.tab_order.get(p).copied());
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
                    if id == self.active_id {
                        self.active_cwd = Some(path);
                    }
                    // A non-focused active-tab pane reports its own cwd; we keep the
                    // focused pane's as the tab's inherited cwd, so ignore it.
                } else if let Some(s) = self.background.iter_mut().find(|s| s.id == id) {
                    s.last_cwd = Some(path);
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
                    && let Some(theme) = color::theme_by_name(&self.config.theme) {
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

                // F11 toggles borderless fullscreen. Handled first so it works
                // whether or not an overlay owns the keyboard, and never reaches
                // the child.
                if event.state.is_pressed()
                    && matches!(&event.logical_key, Key::Named(NamedKey::F11))
                {
                    if let Some(w) = self.window.as_ref() {
                        let fs = if w.fullscreen().is_some() {
                            None
                        } else {
                            Some(winit::window::Fullscreen::Borderless(None))
                        };
                        w.set_fullscreen(fs);
                    }
                    return;
                }

                // The command palette and the find bar own the keyboard while
                // open: every key is routed to them (query edit, list nav, jump,
                // Esc) and never reaches the child or the chrome shortcuts below.
                // Checked before the Ctrl+Shift block so typing letters into the
                // query isn't stolen by the clipboard/tab combos.
                if event.state.is_pressed() && self.palette.is_some() {
                    if self.handle_palette_key(&event.logical_key, event_loop) {
                        return;
                    }
                    return; // consume everything while the palette is up
                }
                if event.state.is_pressed() && self.search.is_some() {
                    if self.handle_search_key(&event.logical_key, event_loop) {
                        return;
                    }
                    return; // consume everything while the find bar is up
                }

                // Ctrl+Shift clipboard combos are consumed by glassy and never
                // reach the child. Intercepted before `encode_key` so the control
                // byte for C/V isn't sent to the PTY.
                if event.state.is_pressed()
                    && self.mods.control_key()
                    && self.mods.shift_key()
                    && let Key::Character(s) = &event.logical_key
                {
                    match s.as_str() {
                        "C" | "c" => {
                            self.copy_selection();
                            return;
                        }
                        "V" | "v" => {
                            self.paste_clipboard();
                            self.mark_dirty(event_loop);
                            return;
                        }
                        "T" | "t" => {
                            self.new_tab(event_loop);
                            return;
                        }
                        "W" | "w" => {
                            // Close the focused pane; falls back to closing the tab
                            // when the tab has only a single pane.
                            self.close_pane(event_loop);
                            return;
                        }
                        // Ctrl+Shift+P opens the command palette (fuzzy action +
                        // settings list). Ctrl+Shift+F opens the in-terminal find
                        // bar. Both own the keyboard while open; handled below.
                        "P" | "p" => {
                            self.open_palette(event_loop);
                            return;
                        }
                        "F" | "f" => {
                            self.open_search(event_loop);
                            return;
                        }
                        // Split the focused pane: E = vertical (left|right),
                        // O = horizontal (top/bottom). Mirrors common terminals.
                        "E" | "e" => {
                            self.split_pane(pane::Dir::Vertical, event_loop);
                            return;
                        }
                        "O" | "o" => {
                            self.split_pane(pane::Dir::Horizontal, event_loop);
                            return;
                        }
                        _ => {}
                    }
                }

                // Alt+Arrow moves focus between tiled panes (no-op when not split,
                // so a single-pane tab passes Alt+Arrow through to the child).
                if event.state.is_pressed()
                    && self.mods.alt_key()
                    && !self.mods.control_key()
                    && self.is_split()
                    && let Key::Named(named) = &event.logical_key
                {
                    let m = match named {
                        NamedKey::ArrowLeft => Some(pane::Move::Left),
                        NamedKey::ArrowRight => Some(pane::Move::Right),
                        NamedKey::ArrowUp => Some(pane::Move::Up),
                        NamedKey::ArrowDown => Some(pane::Move::Down),
                        _ => None,
                    };
                    if let Some(m) = m {
                        self.focus_pane(m, event_loop);
                        return;
                    }
                }

                // Ctrl+Tab / Ctrl+Shift+Tab cycle between tabs.
                if event.state.is_pressed()
                    && self.mods.control_key()
                    && let Key::Named(NamedKey::Tab) = &event.logical_key
                {
                    let delta = if self.mods.shift_key() { -1 } else { 1 };
                    self.cycle_tab(delta, event_loop);
                    return;
                }

                // Ctrl +/-/0 adjusts the font size at runtime (and Ctrl 0 resets
                // to the configured size). Intercepted before `encode_key` so the
                // control bytes for these keys never reach the child. Matches the
                // de-facto terminal/browser zoom convention. Shift is allowed (so
                // Ctrl+Shift+'=' i.e. Ctrl+'+' works) but not required.
                if event.state.is_pressed()
                    && self.mods.control_key()
                    && !self.mods.alt_key()
                    && let Key::Character(s) = &event.logical_key
                {
                    let step = match s.as_str() {
                        "+" | "=" => Some(FontStep::Inc),
                        "-" | "_" => Some(FontStep::Dec),
                        "0" => Some(FontStep::Reset),
                        _ => None,
                    };
                    if let Some(step) = step {
                        self.resize_font(step);
                        self.mark_dirty(event_loop);
                        return;
                    }
                }

                // Ctrl+Shift+B toggles the status bar (with Shift the char arrives
                // upper-case on most layouts, so accept either case).
                if event.state.is_pressed()
                    && self.mods.control_key()
                    && self.mods.shift_key()
                    && let Key::Character(s) = &event.logical_key
                    && matches!(s.as_str(), "b" | "B")
                {
                    self.toggle_status_bar();
                    self.mark_dirty(event_loop);
                    return;
                }

                // Shift + PageUp/PageDown/Home/End drives glassy's own scrollback
                // (the primary screen only) and is consumed before the child sees
                // it. This mirrors the de-facto terminal convention.
                if event.state.is_pressed()
                    && self.mods.shift_key()
                    && !self.term_mode().contains(TermMode::ALT_SCREEN)
                    && let Key::Named(named) = &event.logical_key
                {
                    let scroll = match named {
                        NamedKey::PageUp => Some(Scroll::PageUp),
                        NamedKey::PageDown => Some(Scroll::PageDown),
                        NamedKey::Home => Some(Scroll::Top),
                        NamedKey::End => Some(Scroll::Bottom),
                        _ => None,
                    };
                    if let Some(scroll) = scroll {
                        if let Some(pty) = &self.pty {
                            pty.term.lock().scroll_display(scroll);
                        }
                        self.mark_dirty(event_loop);
                        return;
                    }
                }

                // While the dropdown is open, Up/Down/Enter/Esc navigate it.
                // All other keys close it and pass through to the normal handler.
                if event.state.is_pressed() && self.menu_open {
                    let key = &event.logical_key;
                    if self.handle_menu_key(key, event_loop) {
                        return;
                    }
                    // Any key that didn't navigate the menu closes it.
                    self.close_menu(event_loop);
                    // Fall through: let the keypress reach the child below.
                }

                // While the pane ⋮ menu is open, Up/Down/Enter/Esc navigate it.
                if event.state.is_pressed() && self.pane_menu_open.is_some() {
                    let n = Self::PANE_MENU_ITEMS.len();
                    let key = &event.logical_key;
                    match key {
                        Key::Named(NamedKey::ArrowUp) => {
                            self.pane_menu_sel = (self.pane_menu_sel + n - 1) % n;
                            self.mark_dirty(event_loop);
                            return;
                        }
                        Key::Named(NamedKey::ArrowDown) => {
                            self.pane_menu_sel = (self.pane_menu_sel + 1) % n;
                            self.mark_dirty(event_loop);
                            return;
                        }
                        Key::Named(NamedKey::Enter) => {
                            let idx = self.pane_menu_sel;
                            self.invoke_pane_menu_action(idx, event_loop);
                            return;
                        }
                        Key::Named(NamedKey::Escape) => {
                            self.pane_menu_open = None;
                            self.mark_dirty(event_loop);
                            return;
                        }
                        _ => {
                            // Any other key closes the menu; let it fall through.
                            self.pane_menu_open = None;
                            self.mark_dirty(event_loop);
                        }
                    }
                }

                // While an overlay is open it owns the keyboard — nothing reaches
                // the child. Esc / F1 / Ctrl+, close it; settings handles nav/edit.
                if event.state.is_pressed() && (self.help_open || self.settings_open) {
                    let key = &event.logical_key;
                    let toggle_settings = self.mods.control_key()
                        && matches!(key, Key::Character(s) if s.as_str() == ",");
                    // Esc inside settings closes an open dropdown first; only a
                    // second Esc (or F1 / Ctrl+,) closes the whole panel.
                    if self.settings_open
                        && matches!(key, Key::Named(NamedKey::Escape))
                        && self.settings_drop != gui::SettingsDrop::None
                    {
                        self.settings_drop = gui::SettingsDrop::None;
                        self.force_full_redraw = true;
                        self.mark_dirty(event_loop);
                        return;
                    }
                    if matches!(key, Key::Named(NamedKey::Escape | NamedKey::F1))
                        || toggle_settings
                    {
                        self.help_open = false;
                        self.settings_open = false;
                        self.settings_drop = gui::SettingsDrop::None;
                        self.force_full_redraw = true;
                        self.mark_dirty(event_loop);
                        return;
                    }
                    if self.settings_open {
                        self.handle_settings_key(key.clone(), event_loop);
                    }
                    return; // consume all other keys while an overlay is up
                }

                // Open an overlay (only when none is up).
                if event.state.is_pressed() {
                    if let Key::Named(NamedKey::F1) = &event.logical_key {
                        self.help_open = true;
                        self.force_full_redraw = true;
                        self.mark_dirty(event_loop);
                        return;
                    }
                    if self.mods.control_key()
                        && matches!(&event.logical_key, Key::Character(s) if s.as_str() == ",")
                    {
                        self.open_settings();
                        self.force_full_redraw = true;
                        self.mark_dirty(event_loop);
                        return;
                    }
                }

                // When the application has enabled the kitty keyboard protocol,
                // encode modified keys in CSI-u form so it can disambiguate them
                // (this is what makes Shift+Enter distinct from Enter).
                let kitty = self
                    .term_mode()
                    .contains(TermMode::DISAMBIGUATE_ESC_CODES);
                // DECCKM: arrows/Home/End go out as SS3 (ESC O X) for full-screen
                // apps (vim, less, ncurses) that enable application cursor-key mode.
                let app_cursor = self.term_mode().contains(TermMode::APP_CURSOR);
                if let Some(bytes) = encode_key(&event, self.mods, kitty, app_cursor) {
                    // Typing resets the blink to solid-on so the cursor doesn't
                    // wink out mid-keystroke, matching every mainstream terminal.
                    self.reset_blink();
                    // Typing dismisses any active selection, matching the de-facto
                    // terminal convention.
                    self.clear_selection();
                    if let Some(pty) = &self.pty {
                        // A typed key snaps the view back to the prompt, matching
                        // every mainstream terminal.
                        pty.term.lock().scroll_display(Scroll::Bottom);
                        pty.write(bytes);
                    }
                    // The snap-to-bottom (and the cursor/selection reset above) are
                    // visual changes even when the child emits nothing back — e.g.
                    // typing while scrolled up into a paused/blocked program. Repaint
                    // unconditionally so the view never stays frozen in scrollback.
                    self.mark_dirty(event_loop);
                }
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
                self.mouse_px = (position.x, position.y);
                // Any open GUI overlay (settings, dropdown/context menu, help panel,
                // pane ⋮ menu) owns the pointer: its immediate-mode widgets compute
                // hover / press / slider-drag from `mouse_px` during paint, so every
                // motion must trigger a repaint for those highlights to track the
                // pointer. It also means motion must NOT fall through to drive
                // tab-drag, gutter-drag, terminal hover, or text selection beneath
                // the overlay. Mirror the settings treatment for all of them.
                // The dropdown / context menu (`gui::menu`) highlights the row under
                // the pointer. Mirror the hovered row into `menu_sel` so mouse hover
                // and keyboard nav share one selection, and repaint only when that
                // row actually changes — not every pixel of motion across the panel.
                if self.menu_open && !self.settings_open && !self.help_open {
                    if let Some(action) = self.menu_hit_test(position.x, position.y) {
                        let items: &[MenuAction] =
                            self.menu_items.as_deref().unwrap_or(MenuAction::ALL);
                        if let Some(idx) = items.iter().position(|&a| a == action)
                            && idx != self.menu_sel
                        {
                            self.menu_sel = idx;
                            self.mark_dirty(event_loop);
                        }
                    }
                    return;
                }
                if self.settings_open
                    || self.help_open
                    || self.pane_menu_open.is_some()
                    || self.palette.is_some()
                    || self.search.is_some()
                {
                    self.mark_dirty(event_loop);
                    return;
                }
                let cell = self.px_to_cell(position.x, position.y);
                let moved = cell != self.mouse_cell;
                self.mouse_cell = cell;

                // Drag-to-reorder a tab: while a tab chip is held, move it under
                // the pointer's pixel position and lift it as a drag-ghost. Takes
                // priority over selection/hover; repaint on any motion so the ghost
                // tracks the pointer.
                if self.dragging_tab.is_some() {
                    let _ = self.drag_tab_to(position.x as f32, position.y as f32);
                    self.force_full_redraw = true;
                    self.mark_dirty(event_loop);
                    return;
                }

                // Dragging a pane resize gutter: re-tile under the pointer. Takes
                // priority over hover/selection; repaint so the divider + content
                // follow. The OS resize cursor stays set for the drag's duration.
                if self.dragging_gutter.is_some() {
                    if self.drag_gutter_to(position.x, position.y) {
                        self.mark_dirty(event_loop);
                    }
                    return;
                }

                // Gutter hover: over a split's divider band, switch the OS cursor to
                // a resize arrow and draw the divider transiently fat/bright. Only
                // costs a hit-test on motion; off any gutter restores the default.
                {
                    let new_gutter = self.gutter_at(position.x, position.y);
                    if new_gutter != self.hovered_gutter {
                        self.apply_gutter_cursor(new_gutter.as_ref());
                        self.hovered_gutter = new_gutter;
                        self.mark_dirty(event_loop);
                    }
                    // Over a gutter, suppress tab-bar/selection hover handling below.
                    if self.hovered_gutter.is_some() {
                        return;
                    }
                }

                // Pane header hover: repaint only on an enter/leave or ⋮-button
                // edge, not on every pixel of motion — otherwise dragging the
                // pointer across a header queues a frame per event for no visual
                // change. Track the hovered header and diff it.
                if self.is_split() {
                    let new_hover = self.pane_header_at(position.x, position.y);
                    if new_hover != self.hovered_pane_header {
                        self.hovered_pane_header = new_hover;
                        self.mark_dirty(event_loop);
                    }
                } else if self.hovered_pane_header.is_some() {
                    self.hovered_pane_header = None;
                }

                // Tab-bar hover highlighting: track the item under the pointer (only
                // while over the bar's pixel band), repaint when it changes.
                {
                    let bar_h = self
                        .renderer
                        .as_ref()
                        .map(|r| tab_bar_h(r.cell_metrics().height) as f64)
                        .unwrap_or(0.0);
                    let new_hover = if position.y < bar_h {
                        self.strip_item_at_px(position.x as f32, position.y as f32)
                    } else {
                        None
                    };
                    if new_hover != self.hovered_strip_item {
                        self.hovered_strip_item = new_hover;
                        self.mark_dirty(event_loop);
                    }
                }

                // Extend an in-progress glassy text selection while dragging.
                if self.selecting {
                    self.update_selection();
                    self.mark_dirty(event_loop);
                } else if moved {
                    // Motion reports drive hover highlighting (e.g. the Claude
                    // Code TUI highlights the element under the pointer, which
                    // needs any-motion mode 1003 with no button held).
                    let mode = self.term_mode();
                    if let Some(button) = motion_button(mode, self.held_button) {
                        self.report_mouse(button, true, true, mode);
                    } else if !mode.intersects(TermMode::MOUSE_MODE) {
                        // Track the hovered OSC8 hyperlink so it can be underlined.
                        let (c, r) = self.mouse_cell;
                        let link = self.cell_hyperlink(c, r);
                        if link != self.hovered_link {
                            self.hovered_link = link;
                            self.mark_dirty(event_loop);
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let base = match button {
                    MouseButton::Left => 0u8,
                    MouseButton::Middle => 1,
                    MouseButton::Right => 2,
                    _ => return,
                };
                let pressed = state == ElementState::Pressed;
                // Track the held button for drag reports regardless of mode.
                self.held_button = if pressed { Some(base) } else { None };
                // Real-GUI chrome: capture the left press→release as a click edge for
                // the next chrome paint, and release the press latch on button-up.
                if button == MouseButton::Left {
                    if pressed {
                        self.gui_click_edge = false;
                    } else {
                        // Set the press→release edge; the press latch (`gui_pressed`)
                        // is cleared only AFTER the next paint consumes this edge so
                        // the release frame can still resolve a click on the latched
                        // widget (see the click-edge reset in `render`).
                        self.gui_click_edge = true;
                    }
                    self.mark_dirty(event_loop);
                }

                // While the settings form is open it owns the mouse: the form's
                // immediate-mode widgets resolve hits during paint (from the click
                // edge captured above), so consume the event here and never fall
                // through to the terminal / tab / menu handlers. A left click well
                // outside the panel dismisses the form.
                if self.settings_open {
                    if button == MouseButton::Left && pressed {
                        let (mx, my) = (self.mouse_px.0 as f32, self.mouse_px.1 as f32);
                        if !gui::hit(self.settings_panel, mx, my) {
                            self.settings_open = false;
                            self.settings_drop = gui::SettingsDrop::None;
                            self.force_full_redraw = true;
                            self.mark_dirty(event_loop);
                        }
                    }
                    // Do NOT clear held_button here: the settings snapshot reads
                    // `held_button == Some(0)` as `mouse_down` for immediate-mode
                    // press-latch / slider-drag. Clearing it before the render frame
                    // fires makes every widget see mouse_down=false, killing all
                    // click and drag interactions. `held_button` is correctly set to
                    // None on the release event at the top of this handler (line
                    // `self.held_button = if pressed { Some(base) } else { None }`).
                    return;
                }

                // The help panel (§3.7) is a full-screen scrim + floating panel that
                // owns the pointer exactly like the settings form: its ✕ close button
                // and scrollbar are immediate-mode widgets resolved during paint from
                // the click edge captured above, and a click on the scrim (outside the
                // panel) dismisses it (handled inside `build_help`). Consume the event
                // so it never falls through to start a text selection / tab-click /
                // gutter-drag beneath the panel. Do NOT clear `held_button` — the
                // scrollbar drag reads it as `mouse_down` during paint (same rule as
                // the settings block above).
                if self.help_open {
                    return;
                }

                // The command palette owns the pointer: a left press on a listed
                // row activates it; a press anywhere else (the scrim) closes it.
                if self.palette.is_some() {
                    if button == MouseButton::Left && pressed {
                        let (mx, my) = (self.mouse_px.0 as f32, self.mouse_px.1 as f32);
                        let hit = self
                            .palette_rows
                            .iter()
                            .find(|(_, r)| gui::hit(*r, mx, my))
                            .map(|(idx, _)| *idx);
                        match hit {
                            Some(idx) => self.palette_activate_index(idx, event_loop),
                            None => self.close_palette(event_loop),
                        }
                    }
                    self.held_button = None;
                    return;
                }

                // The find bar owns the pointer too: a click outside the bottom bar
                // is consumed (no terminal selection beneath the overlay). Clicking
                // the bar itself is a no-op (text editing is keyboard-driven).
                if self.search.is_some() {
                    self.held_button = None;
                    return;
                }

                if !pressed {
                    self.dragging_tab = None; // end any tab drag-reorder on release
                    // End any gutter drag; re-evaluate the cursor for the spot we
                    // released over (still a gutter -> resize arrow, else default).
                    if self.dragging_gutter.take().is_some() {
                        let g = self.gutter_at(self.mouse_px.0, self.mouse_px.1);
                        self.apply_gutter_cursor(g.as_ref());
                        self.hovered_gutter = g;
                        self.mark_dirty(event_loop);
                    }
                    // Release any pressed toolbar item so its inset clears.
                    if self.held_strip_item.take().is_some() {
                        self.mark_dirty(event_loop);
                    }
                }

                // A left press over a pane resize gutter begins a drag and MUST NOT
                // start a text selection or focus-swap. Highest priority among
                // press handlers (the gutter sits in the inter-pane gap, not in any
                // pane's cell area, so this never steals a content click).
                if button == MouseButton::Left && pressed
                    && let Some(handle) = self.gutter_at(self.mouse_px.0, self.mouse_px.1) {
                        self.apply_gutter_cursor(Some(&handle));
                        self.hovered_gutter = Some(handle.clone());
                        self.dragging_gutter = Some(handle);
                        self.held_button = None;
                        self.mark_dirty(event_loop);
                        return;
                    }

                // A click anywhere while the dropdown is open: either invoke the
                // selected item (left-click inside panel) or dismiss the menu.
                // A right-click always closes the menu (second right-click = close).
                if pressed && self.menu_open
                    && (button == MouseButton::Left || button == MouseButton::Right)
                {
                    let (mx, my) = self.mouse_px;
                    if button == MouseButton::Left {
                        if let Some(action) = self.menu_hit_test(mx, my) {
                            self.invoke_menu_action(action, event_loop);
                        } else {
                            self.close_menu(event_loop);
                        }
                    } else {
                        // Right-click while menu is open: close without invoking.
                        self.close_menu(event_loop);
                    }
                    self.held_button = None;
                    return;
                }

                // A left press while the pane ⋮ menu is open: invoke or dismiss.
                if button == MouseButton::Left && pressed && self.pane_menu_open.is_some() {
                    let (mx, my) = self.mouse_px;
                    if let Some(idx) = self.pane_menu_hit_test(mx, my) {
                        self.invoke_pane_menu_action(idx, event_loop);
                    } else {
                        // Click outside the menu dismisses it; may still be a header
                        // or content click so don't return early.
                        self.pane_menu_open = None;
                        self.mark_dirty(event_loop);
                    }
                    self.held_button = None;
                    return;
                }

                // A left press on a pane title-bar header: focus the pane or toggle
                // the ⋮ menu. This must come before the gutter check (headers overlap
                // the top of each pane tile, not the inter-pane gap).
                if button == MouseButton::Left && pressed {
                    let (mx, my) = self.mouse_px;
                    if self.pane_header_click(mx, my, event_loop) {
                        self.held_button = None;
                        return;
                    }
                }

                // A left click in the tab strip switches tabs; never sent onward.
                if button == MouseButton::Left && pressed && self.strip_click(event_loop) {
                    self.held_button = None;
                    return;
                }

                // In a split, a press over a non-focused pane focuses it first, so
                // selection / mouse-reporting below target the pane the user
                // clicked. Re-derive the (now pane-local) cell after the swap.
                if pressed && self.is_split() {
                    let (mx, my) = self.mouse_px;
                    if self.focus_pane_at(mx, my, event_loop) {
                        self.mouse_cell = self.px_to_cell(mx, my);
                    }
                }

                let mode = self.term_mode();
                // Ctrl+Left opens an OSC8 hyperlink under the pointer, overriding
                // application mouse handling (the common terminal convention).
                if button == MouseButton::Left && pressed && self.mods.control_key() {
                    let (c, r) = self.mouse_cell;
                    if let Some(uri) = self.cell_hyperlink(c, r) {
                        Self::open_url(&uri);
                        return;
                    }
                }
                // Right-click: open the context menu, gated on mouse-reporting mode.
                //   - not in MOUSE_MODE: plain right-press opens the menu.
                //   - in MOUSE_MODE: Shift+right-press opens it (terminal bypass);
                //     a bare right-press is forwarded to the application.
                if button == MouseButton::Right && pressed {
                    let in_mouse_mode = mode.intersects(TermMode::MOUSE_MODE);
                    if !in_mouse_mode || self.mods.shift_key() {
                        self.open_context_menu(event_loop);
                        self.held_button = None;
                        return;
                    }
                    // else: fall through to report_mouse below
                }

                if mode.intersects(TermMode::MOUSE_MODE) {
                    // The application owns the mouse; never start a glassy
                    // selection or paste underneath it.
                    self.report_mouse(base, pressed, false, mode);
                    return;
                }

                match (button, pressed) {
                    // Left press: start (or extend the granularity of) a glassy
                    // text selection. Double/triple clicks within the same cell
                    // and a short window escalate to Semantic (word) then Lines.
                    (MouseButton::Left, true) => {
                        const MULTI_CLICK: Duration = Duration::from_millis(300);
                        let now = Instant::now();
                        let count = match self.last_click {
                            Some((cell, n, t))
                                if cell == self.mouse_cell
                                    && now.duration_since(t) < MULTI_CLICK =>
                            {
                                (n % 3) + 1
                            }
                            _ => 1,
                        };
                        self.last_click = Some((self.mouse_cell, count, now));
                        let ty = match count {
                            2 => SelectionType::Semantic,
                            3 => SelectionType::Lines,
                            _ => SelectionType::Simple,
                        };
                        self.start_selection(ty);
                        self.mark_dirty(event_loop);
                    }
                    // Left release: finish the drag; the selection persists for copy.
                    // Copy-on-select: a completed selection is mirrored to the
                    // clipboard immediately (the de-facto X11/terminal convention),
                    // so a middle-click / Ctrl+Shift+V paste works without an
                    // explicit copy. A no-op when nothing was actually selected.
                    (MouseButton::Left, false) => {
                        let was_selecting = self.selecting;
                        self.selecting = false;
                        if was_selecting {
                            self.copy_selection();
                        }
                    }
                    // Middle click pastes the clipboard (primary on X11 would be
                    // ideal, but arboard exposes only the standard clipboard).
                    (MouseButton::Middle, true) => {
                        self.paste_clipboard();
                        self.mark_dirty(event_loop);
                    }
                    _ => {}
                }
            }
            WindowEvent::MouseWheel { delta, phase, .. } => {
                use winit::event::TouchPhase;
                // Help panel owns the wheel while open: scroll its keybinding list.
                // The next `build_help` clamps `scroll` against the content height,
                // so we just accumulate here (negative = scroll up toward the top).
                if self.help_open {
                    let line_px = self
                        .renderer
                        .as_ref()
                        .map(|r| r.cell_metrics().height)
                        .unwrap_or(20.0)
                        .max(1.0);
                    let dy = match delta {
                        MouseScrollDelta::LineDelta(_, y) => y * line_px * 3.0,
                        MouseScrollDelta::PixelDelta(p) => p.y as f32,
                    };
                    // Wheel-up (positive y) moves content up = decrease scroll.
                    self.help_state.scroll = (self.help_state.scroll - dy).max(0.0);
                    self.mark_dirty(event_loop);
                    return;
                }
                // A touchpad gesture brackets its deltas with Started/Ended; reset
                // the accumulators and the one-switch-per-swipe latch at those
                // boundaries so each gesture is independent.
                if matches!(
                    phase,
                    TouchPhase::Started | TouchPhase::Ended | TouchPhase::Cancelled
                ) {
                    self.tab_scroll_accum = 0.0;
                    self.content_scroll_accum = 0.0;
                    self.swipe_consumed = false;
                }

                // Over the tab strip: a swipe/scroll switches tabs as a discrete
                // GESTURE — one tab per swipe, clamped at the ends (no wrap-around
                // carousel). Horizontal motion is preferred (natural swipe-to-switch).
                let in_strip = {
                    let bar_h = self
                        .renderer
                        .as_ref()
                        .map(|r| tab_bar_h(r.cell_metrics().height) as f64)
                        .unwrap_or(0.0);
                    self.mouse_px.1 < bar_h
                };
                if in_strip {
                    const STEP: f32 = 24.0; // px of swipe travel to trigger one switch
                    match delta {
                        // A discrete wheel notch always steps one tab (clamped).
                        MouseScrollDelta::LineDelta(x, y) => {
                            let primary = if x.abs() > y.abs() { x } else { y };
                            if primary > 0.0 {
                                self.step_tab(1, event_loop);
                            } else if primary < 0.0 {
                                self.step_tab(-1, event_loop);
                            }
                        }
                        // Touchpad: accumulate, fire ONCE per swipe at the threshold,
                        // then latch until the gesture ends — no twitchy carousel.
                        MouseScrollDelta::PixelDelta(p) => {
                            let primary = (if p.x.abs() > p.y.abs() { p.x } else { p.y }) as f32;
                            self.tab_scroll_accum += primary;
                            if !self.swipe_consumed && self.tab_scroll_accum.abs() >= STEP {
                                let dir = if self.tab_scroll_accum > 0.0 { 1 } else { -1 };
                                self.step_tab(dir, event_loop);
                                self.swipe_consumed = true;
                            }
                        }
                    }
                    return;
                }
                self.tab_scroll_accum = 0.0;

                // In a split, the wheel targets the pane under the pointer: focus it
                // so the scroll / mouse-report below acts on that pane's PTY.
                if self.is_split() {
                    let (mx, my) = self.mouse_px;
                    self.focus_pane_at(mx, my, event_loop);
                }

                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => {
                        self.content_scroll_accum = 0.0;
                        if y == 0.0 {
                            0
                        } else {
                            (y.abs().ceil() as i32) * y.signum() as i32
                        }
                    }
                    // Touchpads emit many sub-line pixel deltas; accumulate and step
                    // by the cell height so slow scrolls register instead of being
                    // truncated to zero each event (the "tiny scrolls do nothing" bug).
                    MouseScrollDelta::PixelDelta(p) => {
                        self.content_scroll_accum += p.y as f32;
                        let step = self
                            .renderer
                            .as_ref()
                            .map(|r| r.cell_metrics().height)
                            .unwrap_or(20.0)
                            .max(1.0);
                        let n = (self.content_scroll_accum / step) as i32;
                        self.content_scroll_accum -= n as f32 * step;
                        n
                    }
                };
                if lines == 0 {
                    return;
                }
                let mode = self.term_mode();
                let up = lines > 0;
                let count = lines.unsigned_abs() as usize;

                match wheel_action(mode) {
                    WheelAction::Report => {
                        // Wheel as button 64 (up) / 65 (down), one report per line.
                        let button = if up { 64 } else { 65 };
                        for _ in 0..count {
                            self.report_mouse(button, true, false, mode);
                        }
                    }
                    WheelAction::Arrows => {
                        // Alt-screen apps (pagers, bat, vim without `mouse=`) expect
                        // the wheel to emit arrow keys — xterm's alternateScroll is
                        // on by default and the alt screen has no scrollback of its
                        // own. ~3 lines per notch.
                        if let Some(pty) = &self.pty {
                            // DECCKM: when the app enabled application cursor-key
                            // mode, arrow keys go out as SS3 (ESC O X), so the wheel
                            // emulation must match or the pager sees the wrong code.
                            let seq: &[u8] = if mode.contains(TermMode::APP_CURSOR) {
                                if up { b"\x1bOA" } else { b"\x1bOB" }
                            } else if up {
                                b"\x1b[A"
                            } else {
                                b"\x1b[B"
                            };
                            let n = count * 3;
                            let mut out = Vec::with_capacity(seq.len() * n);
                            for _ in 0..n {
                                out.extend_from_slice(seq);
                            }
                            pty.write(out);
                        }
                    }
                    WheelAction::Scrollback => {
                        let delta = if up { WHEEL_LINES } else { -WHEEL_LINES } * count as i32;
                        if let Some(pty) = &self.pty {
                            pty.term.lock().scroll_display(Scroll::Delta(delta));
                        }
                        self.mark_dirty(event_loop);
                    }
                }
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
        let blink_active = self.focused && self.cursor_blinking();
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
}

