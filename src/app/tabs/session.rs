//! Session persistence: build / save / restore tab+split snapshots.

use super::super::*;

impl App {
    /// Build a [`crate::session::Session`] snapshot of the current tabs in display
    /// order: each tab's pane tree (a single-pane tab is a one-leaf tree), the
    /// per-pane cwds, and any custom title. Pane ids are remapped to stable
    /// session-relative indices (a tab's leaves numbered 0..N in DFS order) so the
    /// file round-trips independently of the live id counter.
    pub(crate) fn build_session(&self) -> crate::session::Session {
        use crate::session::{PaneState, Session, TabState};

        let mut tabs = Vec::new();
        for &tab_id in &self.tab_order {
            let is_active = tab_id == self.active_id;
            // Resolve this tab's pane group + focused pane + cwd sources.
            let (panes_group, custom_title, focused_cwd, pane_cwds_map): (
                Option<&PaneGroup>,
                Option<String>,
                Option<std::path::PathBuf>,
                &std::collections::HashMap<usize, std::path::PathBuf>,
            ) = if is_active {
                (
                    self.panes.as_ref(),
                    self.active_custom_title.clone(),
                    self.active_cwd.clone(),
                    &self.active_pane_cwds,
                )
            } else {
                match self.background.iter().find(|s| s.id == tab_id) {
                    Some(s) => (
                        s.panes.as_ref(),
                        s.custom_title.clone(),
                        s.last_cwd.clone(),
                        &s.pane_cwds,
                    ),
                    None => continue,
                }
            };

            // The DFS leaf order gives stable session-relative ids; build the
            // live-id -> session-id map and its inverse for the layout descriptor.
            let leaves: Vec<usize> = match panes_group {
                Some(g) => g.layout.leaves(),
                None => vec![tab_id], // single-pane tab: sole leaf is the tab id
            };
            let session_id =
                |live: usize| -> usize { leaves.iter().position(|&l| l == live).unwrap_or(0) };

            let layout = match panes_group {
                Some(g) => g.layout.to_desc(&session_id),
                None => crate::pane::LayoutDesc {
                    root: crate::pane::NodeDesc::Leaf(0),
                    focused: 0,
                },
            };

            // Per-pane cwd: the focused pane uses the tab's cwd; others use their
            // tracked pane cwd (active_pane_cwds / Session::pane_cwds).
            let focused_live = panes_group.map(|g| g.layout.focused()).unwrap_or(tab_id);
            let mut panes = Vec::new();
            for &live in &leaves {
                let cwd = if live == focused_live {
                    focused_cwd.clone()
                } else {
                    pane_cwds_map.get(&live).cloned()
                };
                panes.push(PaneState {
                    id: session_id(live),
                    cwd: cwd.map(|p| p.to_string_lossy().into_owned()),
                });
            }

            tabs.push(TabState {
                layout,
                panes,
                custom_title,
            });
        }

        Session {
            active: self.active_pos(),
            tabs,
        }
    }

    /// Persist the current session to the state file. Called on exit and on tab/
    /// split changes when `restore_session` is enabled (so a crash still leaves a
    /// recent snapshot). A no-op when persistence is off.
    pub(crate) fn save_session(&self) {
        if !self.config.restore_session {
            return;
        }
        self.build_session().save();
    }

    /// Restore tabs/splits/cwds from a saved [`crate::session::Session`], replacing
    /// the single initial tab spawned in `resumed`. Each pane gets a fresh shell in
    /// its persisted cwd; the layout tree and focus are rebuilt; custom titles are
    /// reapplied. Best-effort: a tab that fails to spawn any pane is skipped, and an
    /// empty result leaves the initial tab untouched. Called once at startup.
    pub(crate) fn restore_session(
        &mut self,
        saved: crate::session::Session,
        event_loop: &ActiveEventLoop,
    ) {
        if saved.tabs.is_empty() || self.renderer.is_none() {
            return;
        }
        let m = self.renderer.as_ref().unwrap().cell_metrics();
        let (cw, ch) = (m.width.round() as u16, m.height.round() as u16);

        // Tear down the placeholder initial tab (the lone pty spawned in resumed).
        if let Some(pty) = self.pty.take() {
            pty.shutdown();
        }
        self.background.clear();
        self.tab_order.clear();
        self.active_id = 0;
        self.active_title.clear();
        self.active_custom_title = None;
        self.active_pane_cwds.clear();
        self.active_cwd = None;
        self.panes = None;

        // Rebuild each tab into a parked Session (the active one is swapped in after).
        let active_idx = saved.active.min(saved.tabs.len().saturating_sub(1));
        // (orig_idx, tab_id, session): orig_idx is the index into saved.tabs so the
        // active tab can be selected even when earlier tabs fail to spawn (and are
        // skipped), which would otherwise shift `built` and misalign active_idx.
        let mut built: Vec<(usize, usize, Session)> = Vec::new();
        for (orig_idx, tab) in saved.tabs.iter().enumerate() {
            // Map each session-relative leaf id to a freshly-allocated live id, and
            // spawn a pane PTY per leaf in its saved cwd.
            let leaves = tab.layout.leaves();
            let mut live_of: std::collections::HashMap<usize, usize> =
                std::collections::HashMap::new();
            let mut ptys: std::collections::HashMap<usize, Pty> = std::collections::HashMap::new();
            let mut cwd_of: std::collections::HashMap<usize, std::path::PathBuf> =
                std::collections::HashMap::new();
            for &sess_id in &leaves {
                let live = self.next_id;
                self.next_id += 1;
                let cwd = tab
                    .panes
                    .iter()
                    .find(|p| p.id == sess_id)
                    .and_then(|p| p.cwd.clone())
                    .map(std::path::PathBuf::from);
                let pty = match Pty::spawn(
                    self.proxy.clone(),
                    live,
                    self.cols,
                    self.rows,
                    cw,
                    ch,
                    self.config.shell.clone(),
                    cwd.clone(),
                    self.config.scrollback,
                    &self.config.word_separator,
                    self.config.cursor_style.to_cursor_shape(),
                    self.config.cursor_blink,
                ) {
                    Ok(p) => p,
                    Err(e) => {
                        log::warn!("session restore: pane spawn failed: {e:#}");
                        continue;
                    }
                };
                live_of.insert(sess_id, live);
                if let Some(c) = cwd {
                    cwd_of.insert(live, c);
                }
                ptys.insert(live, pty);
            }
            if ptys.is_empty() {
                continue; // nothing spawned for this tab; skip it
            }

            // Only reconstruct the full split tree when EVERY leaf spawned; if any
            // pane failed (rare), collapse to a single-pane tab using one survivor so
            // the layout never references a pane with no PTY.
            let all_spawned = live_of.len() == leaves.len();
            let (layout, focused_live) = if all_spawned {
                let id_of = |sess: usize| -> usize { *live_of.get(&sess).unwrap_or(&0) };
                let mut l = crate::pane::Layout::from_desc(&tab.layout, &id_of);
                if !ptys.contains_key(&l.focused()) {
                    let any = *ptys.keys().next().unwrap();
                    l.focus(any);
                }
                let f = l.focused();
                (l, f)
            } else {
                let any = *ptys.keys().next().unwrap();
                (crate::pane::Layout::new(any), any)
            };
            // The tab's stable id is its focused pane's live id (mirrors the live
            // model where the focused pane id == tab id).
            let tab_id = focused_live;

            // The focused pane's pty is the Session::pty; the rest become `others`.
            // `focused_live` is always a live key here (the layout was forced onto a
            // present pane above, and the collapse branch takes a key straight from
            // `ptys`), so the remove can't miss — but don't panic if that invariant
            // ever breaks: skip this tab and shut down its spawned PTYs instead.
            debug_assert!(
                ptys.contains_key(&focused_live),
                "focused_live {focused_live} must be a live pane id"
            );
            let Some(focused_pty) = ptys.remove(&focused_live) else {
                log::warn!("session restore: focused pane {focused_live} had no PTY; skipping tab");
                for (_, p) in ptys.drain() {
                    p.shutdown();
                }
                continue;
            };
            let focused_cwd = cwd_of.get(&focused_live).cloned();
            // Drop any survivor PTYs not in the (possibly collapsed) layout.
            if !all_spawned {
                for (_, p) in ptys.drain() {
                    p.shutdown();
                }
            }

            let panes = if layout.len() == 1 {
                // Single-pane tab: any extra PTYs already drained above.
                for (_, p) in ptys.drain() {
                    p.shutdown();
                }
                None
            } else {
                let mut others_titles = HashMap::new();
                for &live in live_of.values() {
                    others_titles.entry(live).or_insert_with(String::new);
                }
                Some(PaneGroup {
                    layout,
                    others: ptys,
                    others_titles,
                })
            };

            // Per-pane cwds for non-focused panes (for re-persisting later).
            let mut pane_cwds = std::collections::HashMap::new();
            for (&live, c) in &cwd_of {
                if live != focused_live {
                    pane_cwds.insert(live, c.clone());
                }
            }

            let session = Session {
                id: tab_id,
                pty: focused_pty,
                panes,
                title: String::new(),
                activity: false,
                busy_until: None,
                last_cwd: focused_cwd,
                custom_title: tab.custom_title.clone(),
                pane_cwds,
            };
            built.push((orig_idx, tab_id, session));
        }

        if built.is_empty() {
            // Everything failed to spawn: fall back to a single fresh tab so the
            // app is still usable.
            self.spawn_fallback_tab();
            return;
        }

        // Install: tab_order from built order, active tab swapped into the live slots.
        for (_, id, _) in &built {
            self.tab_order.push(*id);
        }
        // Select by the ORIGINAL saved index, not a position in `built` (which may
        // be shorter if any tab failed to spawn). Fall back to the first survivor.
        let active_id = built
            .iter()
            .find(|(orig, ..)| *orig == active_idx)
            .map(|(_, id, _)| *id)
            .unwrap_or(built[0].1);
        for (_, id, session) in built {
            if id == active_id {
                self.active_id = session.id;
                self.active_title = session.title;
                self.active_custom_title = session.custom_title;
                self.active_cwd = session.last_cwd;
                self.active_pane_cwds = session.pane_cwds;
                self.pty = Some(session.pty);
                self.panes = session.panes;
            } else {
                self.background.push(session);
            }
        }
        if self.panes.is_some() {
            self.resize_panes();
        }
        self.update_window_title();
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// Spawn a single fresh tab as id 0 (used when session restore fails entirely).
    pub(super) fn spawn_fallback_tab(&mut self) {
        let Some(r) = self.renderer.as_ref() else {
            return;
        };
        let m = r.cell_metrics();
        if let Ok(pty) = Pty::spawn(
            self.proxy.clone(),
            self.next_id,
            self.cols,
            self.rows,
            m.width.round() as u16,
            m.height.round() as u16,
            self.config.shell.clone(),
            None,
            self.config.scrollback,
            &self.config.word_separator,
            self.config.cursor_style.to_cursor_shape(),
            self.config.cursor_blink,
        ) {
            self.active_id = self.next_id;
            self.tab_order.push(self.next_id);
            self.next_id += 1;
            self.pty = Some(pty);
        }
    }
}
