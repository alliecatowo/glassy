//! The revamped sectioned settings window (settings-themes stream).
//!
//! Replaces the old flat shrink-to-fit form with a real preferences window: a
//! left SIDEBAR (General / Appearance / Themes / Keys / Panes / Advanced) and a
//! scrollable right pane per section, every config key represented. It reuses the
//! existing widget vocabulary (stepper / slider / segmented / dropdown / toggle /
//! text_input) and the same [`SettingsView`] / [`SettingsEvents`] / [`SettingsFields`]
//! flow — only extended additively.
//!
//! Rows are laid out into a content buffer then drawn with range-culling against
//! the scrollable viewport (no GPU scissor — matching `gui::help`), so a section
//! taller than the window scrolls instead of shrinking.

use super::*;

/// The kind of control a settings row carries. The row builder pushes these into
/// a flat list; the draw pass walks the list, culls off-screen rows, and emits the
/// right widget for each.
/// Full names for the window-effect dropdown (index order mirrors `WindowEffect::index`).
const EFFECT_NAMES: &[&str] = &[
    "Off",
    "Frosted",
    "Acrylic",
    "CRT",
    "Scanlines",
    "Grain",
    "Vignette",
    "Bloom",
    "Custom",
];

/// The Custom-effect slider rows: `(widget id, label, channel index into
/// `SettingsView::custom_effect`)`. Shown only when the effect is Custom.
const CUSTOM_FX_SLIDERS: &[(&str, &str, usize)] = &[
    ("settings/fx_curvature", "Curvature", 0),
    ("settings/fx_scanline", "Scanlines", 1),
    ("settings/fx_glow", "Glow", 2),
    ("settings/fx_vignette", "Vignette", 3),
    ("settings/fx_grain", "Grain", 4),
    ("settings/fx_tint", "Glass tint", 5),
];

enum RowKind<'a> {
    /// A dim section heading + a hairline under it.
    Heading(&'a str),
    /// `label` + a `−value+` stepper. Carries the widget id path.
    Stepper {
        id: &'static str,
        label: &'a str,
        text: String,
    },
    /// `label` + a slider (range `[min, max]`) with a right-aligned value
    /// readout. Every existing caller before the settings-sections stream used
    /// an implicit 0..1 range; `min`/`max` make that explicit so a wider-range
    /// control (e.g. `quake_height`'s 0.1..1.0) can reuse the same row kind.
    Slider {
        id: &'static str,
        label: &'a str,
        value: f32,
        min: f32,
        max: f32,
    },
    /// `label` + a segmented control. Returns the new index via the matching event.
    Segmented {
        id: &'static str,
        label: &'a str,
        options: &'a [&'a str],
        sel: usize,
    },
    /// `label` + an on/off toggle.
    Toggle {
        id: &'static str,
        label: &'a str,
        value: bool,
    },
    /// `label` + a dropdown header (the popup is drawn last, floating).
    Dropdown {
        id: &'static str,
        label: &'a str,
        text: &'a str,
        swatch: Option<[f32; 4]>,
        which: SettingsDrop,
    },
    /// `label` + a read-only field. `copyable` gates the copy (`⧉`) / open
    /// (`↗`) trailing icons AND their event routing (`SettingsEvents::copy_path`
    /// / `open_path`, which act on the CONFIG file path) — `false` for other
    /// read-only values (e.g. the Terminal section's resolved `shell`/`cwd`)
    /// so those icons never fire the wrong action.
    PathField {
        id: &'static str,
        label: &'a str,
        text: &'a str,
        copyable: bool,
    },
    /// A free-form informational line (dim), e.g. hints.
    Info(&'a str),
    /// A clickable runtime-profile row (Profiles section). Picking it switches
    /// the live profile via [`SettingsEvents::profile_pick`]. `active` marks the
    /// currently-active profile (accent checkmark + selected-row fill).
    Profile {
        index: usize,
        name: &'a str,
        active: bool,
    },
    /// The "(default)" row (Profiles section): the base config with no profile
    /// activated — the only way back once a profile has been switched to.
    /// Picking it fires [`SettingsEvents::profile_pick_default`].
    ProfileDefault { active: bool },
    /// "Duplicate current settings as a new profile" row (Profiles section): a
    /// name [`TextEdit`] + an inline accent Save button. Enter in the field or
    /// the button both fire [`SettingsEvents::profile_create`]; the pending name
    /// lives in `SettingsFields::profile_name`.
    ProfileCreate {
        text_id: &'static str,
        button_id: &'static str,
        placeholder: &'a str,
    },
    /// `label` + an editable text field bound to one of the [`SettingsFields`]
    /// models (word separators / font features).
    TextEdit {
        id: &'static str,
        label: &'a str,
        which: EditField,
        placeholder: &'a str,
    },
}

/// Which [`SettingsFields`] editable model a [`RowKind::TextEdit`] row drives.
#[derive(Clone, Copy)]
enum EditField {
    WordSep,
    FontFeatures,
    HintsChars,
    FontBold,
    FontItalic,
    FontBoldItalic,
    FontSymbolMap,
    FontVariations,
    StatusBarSegments,
    StatusBarTimeFormat,
    WallpaperTheme,
}

impl<'r> Ui<'r> {
    /// Build the sectioned settings window. Returns the [`SettingsEvents`] for the
    /// frame. `surface` is the framebuffer size in px.
    pub fn build_settings_sectioned(
        &mut self,
        surface: (f32, f32),
        v: &SettingsView,
        fields: &mut SettingsFields,
    ) -> SettingsEvents {
        let mut ev = SettingsEvents::default();
        let m = self.m;

        // Full-screen scrim.
        self.quad(
            Rect::new(0.0, 0.0, surface.0, surface.1),
            [0.0, 0.0, 0.0, 0.5],
        );

        // Centered window: ~64 cols wide, up to 84% of the surface tall.
        let pw = (m.cell_w * 64.0)
            .min(surface.0 - 2.0 * m.pad)
            .max(m.cell_w * 36.0);
        let ph = (surface.1 * 0.84)
            .min(m.row_h * 22.0)
            .max(m.row_h * 8.0)
            .round();
        let px = ((surface.0 - pw) * 0.5).round();
        let py = ((surface.1 - ph) * 0.5).round().max(m.pad);
        let panel = Rect::new(px, py, pw, ph);
        ev.panel = panel;
        let inner = self.panel(panel, m.card_radius);

        // Header: title + ✕.
        let title_y = (inner.y + (m.row_h - m.cell_h) * 0.5).round();
        self.label(inner.x.round(), title_y, "glassy — settings", fg());
        let close_r = Rect::new(inner.x + inner.w - m.row_h, inner.y, m.row_h, m.row_h);
        if self.icon_button(id("settings/close"), close_r, '✕').clicked {
            ev.close = true;
        }
        let header_h = m.row_h;
        let sep_y = (inner.y + header_h).round();
        self.separator(inner.x, sep_y, inner.w);

        // -- Sidebar (left) ---------------------------------------------------
        let body_top = sep_y + 1.0 + m.gap;
        let body_bot = inner.y + inner.h - m.row_h - m.gap; // leave room for footer
        let body_h = (body_bot - body_top).max(m.row_h);
        let sidebar_w = (m.cell_w * 13.0).round();
        let active_section = v.section.min(SettingsSection::ALL.len() - 1);
        for (i, sec) in SettingsSection::ALL.iter().enumerate() {
            let ry = body_top + i as f32 * (m.row_h + 2.0);
            if ry + m.row_h > body_bot {
                break;
            }
            let rr = Rect::new(inner.x, ry, sidebar_w - m.gap, m.row_h);
            let wid = id_combine(id("settings/section"), i as u64);
            let it = self.interact(wid, rr, true);
            if i == active_section {
                self.rrect(rr, m.radius, sel_bg());
            } else if it.hovered {
                self.rrect(rr, m.radius, state_fill(track_off(), 1.0, false));
            }
            let ty = (rr.center_y() - m.cell_h * 0.5).round();
            let col = if i == active_section { fg() } else { fg_dim() };
            self.label((rr.x + m.pad).round(), ty, sec.label(), col);
            if it.clicked {
                ev.section_pick = Some(i);
            }
        }

        // -- Right pane (scrollable content for the active section) -----------
        let pane_x = inner.x + sidebar_w;
        let scrollbar_w = (m.gap.max(6.0)).round();
        let pane_w = (inner.x + inner.w - pane_x - scrollbar_w - 4.0).max(m.cell_w * 12.0);
        let pane = Rect::new(pane_x, body_top, pane_w, body_h);

        // Build the rows for the active section.
        let section = SettingsSection::from_index(active_section);
        let rows = build_section_rows(section, v);

        // Row geometry: heading rows are a touch shorter than control rows.
        let ctrl_h = (m.row_h - m.gap).max(m.cell_h);
        let heading_h = (m.cell_h + m.gap).round();
        let step = m.row_h + m.gap;
        let row_height = |r: &RowKind| match r {
            RowKind::Heading(_) => heading_h + m.gap,
            RowKind::Info(_) => heading_h,
            _ => step,
        };
        let content_h: f32 = rows.iter().map(row_height).sum();

        // Clamp scroll.
        let max_scroll = (content_h - body_h).max(0.0);
        let scroll = v.section_scroll.clamp(0.0, max_scroll);

        let label_w = (pane_w * 0.42).min(m.cell_w * 18.0).max(m.cell_w * 9.0);
        let ctrl_x = pane.x + label_w;
        let ctrl_w = (pane.w - label_w).max(m.cell_w * 6.0);

        // Defer the floating dropdown popup until after the body draw so it floats.
        let mut pending_popup: Option<(Rect, SettingsDrop)> = None;

        let mut ry = pane.y - scroll;
        for row in &rows {
            let rh = row_height(row);
            // Cull rows fully outside the viewport.
            if ry + rh <= pane.y || ry >= pane.y + pane.h {
                ry += rh;
                continue;
            }
            match row {
                RowKind::Heading(text) => {
                    let ty = (ry + (heading_h - m.cell_h) * 0.5).round();
                    if ty >= pane.y && ty + m.cell_h <= pane.y + pane.h {
                        self.label(pane.x.round(), ty, text, fg_dim());
                        let line_y = (ry + heading_h).round();
                        if line_y < pane.y + pane.h {
                            self.separator(pane.x, line_y, pane.w);
                        }
                    }
                }
                RowKind::Info(text) => {
                    let ty = (ry + (heading_h - m.cell_h) * 0.5).round();
                    if ty >= pane.y && ty + m.cell_h <= pane.y + pane.h {
                        self.label_clip(pane.x.round(), ty, text, pane.w, fg_dim());
                    }
                }
                RowKind::Profile {
                    index,
                    name,
                    active,
                } => {
                    let inside = ry >= pane.y && ry + ctrl_h <= pane.y + pane.h;
                    if inside {
                        let rr = Rect::new(pane.x, ry, pane.w, ctrl_h);
                        let wid = id_combine(id("settings/profile"), *index as u64);
                        let it = self.interact(wid, rr, true);
                        self.paint_profile_row(rr, &it, name, *active, fg());
                        if it.clicked && !*active {
                            ev.profile_pick = Some(*index);
                        }
                    }
                }
                RowKind::ProfileDefault { active } => {
                    let inside = ry >= pane.y && ry + ctrl_h <= pane.y + pane.h;
                    if inside {
                        let rr = Rect::new(pane.x, ry, pane.w, ctrl_h);
                        let wid = id("settings/profile_default");
                        let it = self.interact(wid, rr, true);
                        self.paint_profile_row(rr, &it, "(default)", *active, fg_dim());
                        if it.clicked && !*active {
                            ev.profile_pick_default = true;
                        }
                    }
                }
                RowKind::ProfileCreate {
                    text_id,
                    button_id,
                    placeholder,
                } => {
                    let inside = ry >= pane.y && ry + ctrl_h <= pane.y + pane.h;
                    if inside {
                        let ly = (ry + (ctrl_h - m.cell_h) * 0.5).round();
                        self.label_clip(
                            pane.x.round(),
                            ly,
                            "New profile",
                            label_w - m.gap,
                            fg_dim(),
                        );
                        let btn_w = (m.cell_w * 10.0).round();
                        let text_w = (ctrl_w - btn_w - m.gap).max(m.cell_w * 6.0);
                        let fr = Rect::new(ctrl_x, ry, text_w, ctrl_h);
                        self.text_input(
                            id(text_id),
                            fr,
                            fields.profile_name,
                            fields.profile_name_ms,
                            placeholder,
                            fields.blink_on,
                            fields.double_click,
                        );
                        let btn_r = Rect::new(ctrl_x + text_w + m.gap, ry, btn_w, ctrl_h);
                        if self.accent_button(id(button_id), btn_r, "Save as").clicked {
                            ev.profile_create = true;
                        }
                    }
                }
                RowKind::TextEdit {
                    id: wid,
                    label,
                    which,
                    placeholder,
                } => {
                    let inside = ry >= pane.y && ry + ctrl_h <= pane.y + pane.h;
                    if inside {
                        let ly = (ry + (ctrl_h - m.cell_h) * 0.5).round();
                        self.label_clip(pane.x.round(), ly, label, label_w - m.gap, fg_dim());
                        let fr = Rect::new(ctrl_x, ry, ctrl_w, ctrl_h);
                        let (edit, ems): (&mut TextEdit, &mut TextInputMouse) = match which {
                            EditField::WordSep => (fields.word_sep, fields.word_sep_ms),
                            EditField::FontFeatures => (fields.font_feat, fields.font_feat_ms),
                            EditField::HintsChars => (fields.hints_chars, fields.hints_chars_ms),
                            EditField::FontBold => (fields.font_bold, fields.font_bold_ms),
                            EditField::FontItalic => (fields.font_italic, fields.font_italic_ms),
                            EditField::FontBoldItalic => {
                                (fields.font_bold_italic, fields.font_bold_italic_ms)
                            }
                            EditField::FontSymbolMap => {
                                (fields.font_symbol_map, fields.font_symbol_map_ms)
                            }
                            EditField::FontVariations => {
                                (fields.font_variations, fields.font_variations_ms)
                            }
                            EditField::StatusBarSegments => {
                                (fields.status_bar_segments, fields.status_bar_segments_ms)
                            }
                            EditField::StatusBarTimeFormat => (
                                fields.status_bar_time_format,
                                fields.status_bar_time_format_ms,
                            ),
                            EditField::WallpaperTheme => {
                                (fields.wallpaper_theme, fields.wallpaper_theme_ms)
                            }
                        };
                        self.text_input(
                            id(wid),
                            fr,
                            edit,
                            ems,
                            placeholder,
                            fields.blink_on,
                            fields.double_click,
                        );
                    }
                }
                _ => {
                    // Control rows are only interactive when fully inside the pane,
                    // so a half-clipped control never grabs a click.
                    let inside = ry >= pane.y && ry + ctrl_h <= pane.y + pane.h;
                    if inside {
                        self.draw_control_row(
                            row,
                            pane.x,
                            ry,
                            label_w,
                            ctrl_x,
                            ctrl_w,
                            ctrl_h,
                            v,
                            &mut ev,
                            &mut pending_popup,
                        );
                    }
                }
            }
            ry += rh;
        }

        // Scrollbar (when content overflows).
        if max_scroll > 0.0 {
            let track = Rect::new(inner.x + inner.w - scrollbar_w, pane.y, scrollbar_w, pane.h);
            let new = self.scrollbar(id("settings/scrollbar"), track, content_h, pane.h, scroll);
            if (new - scroll).abs() > f32::EPSILON {
                ev.section_scroll = Some(new);
            }
        }

        // Footer: Save (accent) + Close + transient saved.
        let fy = inner.y + inner.h - m.row_h;
        self.separator(inner.x, (fy - m.gap).round(), inner.w);
        let bw = (m.cell_w * 9.0).round();
        let close_btn = Rect::new(inner.x + inner.w - bw, fy, bw, m.row_h);
        let save_btn = Rect::new(close_btn.x - bw - m.gap, fy, bw, m.row_h);
        if self
            .accent_button(id("settings/save"), save_btn, "Save")
            .clicked
        {
            ev.save = true;
        }
        if self
            .button(id("settings/close_btn"), close_btn, "Close")
            .clicked
        {
            ev.close = true;
        }
        if v.saved {
            let ly = (fy + (m.row_h - m.cell_h) * 0.5).round();
            self.label(inner.x.round(), ly, "✓ saved", fill_on());
        }

        // -- Custom-theme hex editor (only on the Themes section) ------------
        // Drawn after the scrolled body so the editor panel floats clean over it.
        if section == SettingsSection::Themes && v.custom_editing < v.custom_labels.len() {
            self.draw_custom_editor(panel, v, fields, &mut ev);
        }

        // -- Floating dropdown popup (drawn LAST) ---------------------------
        if let Some((anchor, which)) = pending_popup {
            let (names, sel, swatches): (&[&str], usize, Option<&[[f32; 4]]>) = match which {
                SettingsDrop::Theme => (v.theme_names, v.theme_idx, Some(v.theme_swatches)),
                SettingsDrop::Font => (v.font_names, v.font_idx, None),
                SettingsDrop::ThemeLight => (
                    v.theme_names,
                    theme_index(v.theme_names, v.theme_light),
                    Some(v.theme_swatches),
                ),
                SettingsDrop::ThemeDark => (
                    v.theme_names,
                    theme_index(v.theme_names, v.theme_dark),
                    Some(v.theme_swatches),
                ),
                SettingsDrop::Effect => (EFFECT_NAMES, v.window_effect_idx.min(8), None),
                SettingsDrop::None => (&[], 0, None),
            };
            let pick = self.dropdown_popup(
                id("settings/section/popup"),
                anchor,
                names,
                sel,
                swatches,
                surface.1,
            );
            if let Some(p) = pick {
                match which {
                    SettingsDrop::Theme => ev.theme_pick = Some(p),
                    SettingsDrop::Font => ev.font_pick = Some(p),
                    SettingsDrop::ThemeLight => ev.theme_light_pick = Some(p),
                    SettingsDrop::ThemeDark => ev.theme_dark_pick = Some(p),
                    SettingsDrop::Effect => ev.window_effect = Some(p),
                    SettingsDrop::None => {}
                }
            }
        }

        ev
    }

    /// Emit the widget for a single control row at vertical position `ry`.
    #[allow(clippy::too_many_arguments)]
    fn draw_control_row(
        &mut self,
        row: &RowKind,
        px: f32,
        ry: f32,
        label_w: f32,
        ctrl_x: f32,
        ctrl_w: f32,
        ctrl_h: f32,
        v: &SettingsView,
        ev: &mut SettingsEvents,
        pending_popup: &mut Option<(Rect, SettingsDrop)>,
    ) {
        let m = self.m;
        let ly = (ry + (ctrl_h - m.cell_h) * 0.5).round();
        let label_clip = label_w - m.gap;
        let rect = |w: f32| Rect::new(ctrl_x, ry, w, ctrl_h);
        let toggle_w = (m.cell_h * 2.0).max(m.ctrl_w * 0.25).round();
        let toggle_h = m.cell_h;
        let toggle_rect = Rect::new(
            ctrl_x,
            (ry + (ctrl_h - toggle_h) * 0.5).round(),
            toggle_w,
            toggle_h,
        );
        match row {
            RowKind::Stepper {
                id: wid,
                label,
                text,
            } => {
                self.label_clip(px.round(), ly, label, label_clip, fg_dim());
                let delta = self.stepper(id(wid), rect(m.ctrl_w), text);
                apply_stepper_event(wid, delta, ev);
            }
            RowKind::Slider {
                id: wid,
                label,
                value,
                min,
                max,
            } => {
                self.label_clip(px.round(), ly, label, label_clip, fg_dim());
                let sl = rect((ctrl_w - m.cell_w * 6.0).max(m.cell_w * 4.0));
                if *wid == "settings/opacity" {
                    // Opacity's slider lives in PERCEPTUAL space (equal drags ==
                    // equal visual change) while the config stores the plain
                    // linear value, so map both ways around the widget.
                    let slider_pos = crate::renderer::opacity_to_slider(*value);
                    let new_pos = self.slider(id(wid), sl, slider_pos, 0.0, 1.0, 0.04);
                    let nv = crate::renderer::slider_to_opacity(new_pos);
                    if (nv - value).abs() > f32::EPSILON {
                        ev.opacity = Some(nv);
                    }
                    self.label_right(ctrl_x + ctrl_w, ly, &format!("{nv:.2}"), fg());
                } else {
                    // Everything else (Custom-effect channels, quake_height,
                    // power_mode_intensity) is a plain linear `[min, max]` slider.
                    let nv = self.slider(id(wid), sl, *value, *min, *max, 0.02);
                    if (nv - value).abs() > f32::EPSILON {
                        apply_slider_event(wid, nv, ev);
                    }
                    self.label_right(ctrl_x + ctrl_w, ly, &format!("{nv:.2}"), fg());
                }
            }
            RowKind::Segmented {
                id: wid,
                label,
                options,
                sel,
            } => {
                self.label_clip(px.round(), ly, label, label_clip, fg_dim());
                let nv = self.segmented(
                    id(wid),
                    rect(ctrl_w),
                    options,
                    (*sel).min(options.len().saturating_sub(1)),
                );
                if nv != *sel {
                    apply_segmented_event(wid, nv, ev);
                }
            }
            RowKind::Toggle {
                id: wid,
                label,
                value,
            } => {
                self.label_clip(px.round(), ly, label, label_clip, fg_dim());
                if self.toggle(id(wid), toggle_rect, *value) != *value {
                    ev.toggled.push(wid);
                }
            }
            RowKind::Dropdown {
                id: wid,
                label,
                text,
                swatch,
                which,
            } => {
                self.label_clip(px.round(), ly, label, label_clip, fg_dim());
                let dr = rect(ctrl_w);
                let open = v.open == *which;
                if self.dropdown(id(wid), dr, text, open, *swatch) == DropdownEvt::Toggle {
                    apply_dropdown_toggle(*which, ev);
                }
                if open {
                    *pending_popup = Some((dr, *which));
                }
            }
            RowKind::PathField {
                id: wid,
                label,
                text,
                copyable,
            } => {
                self.label_clip(px.round(), ly, label, label_clip, fg_dim());
                if *copyable {
                    match self.text_field_readonly(id(wid), rect(ctrl_w), text, true, true) {
                        FieldEvt::Copy => ev.copy_path = true,
                        FieldEvt::Open => ev.open_path = true,
                        FieldEvt::None => {}
                    }
                } else {
                    self.text_field_readonly(id(wid), rect(ctrl_w), text, false, false);
                }
            }
            RowKind::Heading(_)
            | RowKind::Info(_)
            | RowKind::Profile { .. }
            | RowKind::ProfileDefault { .. }
            | RowKind::ProfileCreate { .. }
            | RowKind::TextEdit { .. } => {}
        }
    }

    /// Paint one profile-list row's surface + label (shared by the named
    /// `RowKind::Profile` rows and the `RowKind::ProfileDefault` row): a
    /// selected-row fill + accent checkmark + "Active" hint when `active`,
    /// mirroring the dropdown popup's current-selection treatment
    /// ([`Ui::dropdown_popup`]) — the same accent-checkmark language used
    /// elsewhere in this file for "this is the current one", scaled down from
    /// the active-tab-chip's accent crown to a single-row list affordance.
    fn paint_profile_row(
        &mut self,
        rr: Rect,
        it: &Interaction,
        label_text: &str,
        active: bool,
        label_color: [f32; 4],
    ) {
        let m = self.m;
        if active {
            self.rrect(rr, m.radius, sel_bg());
        } else if it.hovered {
            self.rrect(rr, m.radius, state_fill(track_off(), 1.0, false));
        }
        let ty = (rr.center_y() - m.cell_h * 0.5).round();
        let mut tx = (rr.x + m.pad).round();
        if active {
            self.label(tx, ty, "✓", fill_on());
            tx += m.cell_w * 1.4;
        }
        self.label(tx, ty, label_text, label_color);
        let (hint, hint_color) = if active {
            ("Active", fill_on())
        } else {
            ("Switch →", fg_dim())
        };
        self.label_right(rr.x + rr.w - m.pad, ty, hint, hint_color);
    }

    /// Draw the floating custom-theme color editor: a swatch grid (click a swatch
    /// to edit), a hex input for the selected entry, and Apply / Save buttons.
    fn draw_custom_editor(
        &mut self,
        panel: Rect,
        v: &SettingsView,
        fields: &mut SettingsFields,
        ev: &mut SettingsEvents,
    ) {
        let m = self.m;
        // Editor card pinned to the bottom-right of the panel, above the footer.
        let card_w = (m.cell_w * 30.0).min(panel.w - 2.0 * m.pad);
        let card_h = (m.row_h * 6.0).round();
        let cx = (panel.x + panel.w - card_w - m.pad).round();
        let cy = (panel.y + panel.h - card_h - m.row_h - m.pad).round();
        let card = Rect::new(cx, cy, card_w, card_h);
        self.rrect(card, m.card_radius, glass_float());
        self.edge_light(card);

        let pad = m.pad;
        let title = format!("Edit: {}", v.custom_labels[v.custom_editing]);
        self.label((card.x + pad).round(), (card.y + pad).round(), &title, fg());

        // Swatch grid: 4 columns of the 20 entries, click to select.
        let cols = 5usize;
        let sw = (m.cell_h * 1.1).round();
        let gx = (card.x + pad).round();
        let gy = (card.y + pad + m.row_h).round();
        for (i, color) in v.custom_swatches.iter().enumerate() {
            let row = i / cols;
            let coln = i % cols;
            let r = Rect::new(
                gx + coln as f32 * (sw + 4.0),
                gy + row as f32 * (sw + 4.0),
                sw,
                sw,
            );
            let wid = id_combine(id("settings/custom/swatch"), i as u64);
            let it = self.interact(wid, r, true);
            self.rrect(r, 3.0, *color);
            if i == v.custom_editing {
                self.focus_ring(r, 3.0);
            } else if it.hovered {
                self.rrect(r.inset(1.0), 2.0, with_alpha(fg(), 0.15));
            }
            if it.clicked {
                ev.custom_color_pick = Some(i);
            }
        }

        // Hex input + Apply/Save on the right column of the card.
        let field_x = (gx + cols as f32 * (sw + 4.0) + m.gap).round();
        let field_w = (card.x + card.w - pad - field_x).max(m.cell_w * 8.0);
        let field_rect = Rect::new(field_x, gy, field_w, m.row_h - m.gap);
        self.text_input(
            id("settings/custom/hex"),
            field_rect,
            fields.theme_hex,
            fields.theme_hex_ms,
            "#rrggbb",
            fields.blink_on,
            fields.double_click,
        );
        let bw = (field_w - m.gap) * 0.5;
        let by = gy + m.row_h;
        let apply_btn = Rect::new(field_x, by, bw, m.row_h - m.gap);
        let save_btn = Rect::new(field_x + bw + m.gap, by, bw, m.row_h - m.gap);
        if self
            .button(id("settings/custom/apply"), apply_btn, "Apply")
            .clicked
        {
            ev.custom_apply = true;
        }
        if self
            .accent_button(id("settings/custom/save"), save_btn, "Save")
            .clicked
        {
            ev.custom_save = true;
        }
    }
}

/// Find the index of `name` within the THEME_NAMES-shaped `names` slice (0 if absent).
fn theme_index(names: &[&str], name: &str) -> usize {
    names.iter().position(|&n| n == name).unwrap_or(0)
}

/// Build the row list for a section. Borrows the [`SettingsView`] so labels and
/// values come straight from the live config snapshot.
fn build_section_rows<'a>(section: SettingsSection, v: &'a SettingsView<'a>) -> Vec<RowKind<'a>> {
    let mut rows: Vec<RowKind<'a>> = Vec::new();
    match section {
        SettingsSection::General => {
            rows.push(RowKind::Heading("Font"));
            rows.push(RowKind::Stepper {
                id: "settings/font_size",
                label: "Font size",
                text: format!("{:.0} px", v.font_px),
            });
            rows.push(RowKind::Dropdown {
                id: "settings/font_family",
                label: "Font",
                text: v.font_family,
                swatch: None,
                which: SettingsDrop::Font,
            });
            rows.push(RowKind::Heading("Window"));
            rows.push(RowKind::Slider {
                id: "settings/opacity",
                label: "Opacity",
                value: v.opacity,
                min: 0.0,
                max: 1.0,
            });
            rows.push(RowKind::Stepper {
                id: "settings/padding",
                label: "Padding",
                text: format!("{} px", v.padding),
            });
            rows.push(RowKind::Heading("Bell"));
            rows.push(RowKind::Segmented {
                id: "settings/bell",
                label: "Bell",
                options: &["Off", "Visual", "Audible"],
                sel: v.bell.min(2),
            });
        }
        SettingsSection::Appearance => {
            rows.push(RowKind::Heading("Cursor"));
            rows.push(RowKind::Segmented {
                id: "settings/cursor_style",
                label: "Cursor shape",
                options: &["Block", "Beam", "Underline"],
                sel: v.cursor_style_idx.min(2),
            });
            rows.push(RowKind::Toggle {
                id: "settings/cursor_blink",
                label: "Cursor blink",
                value: v.cursor_blink,
            });
            rows.push(RowKind::Toggle {
                id: "settings/cursor_trail",
                label: "Cursor trail",
                value: v.cursor_trail,
            });
            rows.push(RowKind::Heading("Text"));
            rows.push(RowKind::Toggle {
                id: "settings/ligatures",
                label: "Ligatures",
                value: v.ligatures,
            });
            rows.push(RowKind::Heading("Overlays"));
            rows.push(RowKind::Toggle {
                id: "settings/minimap",
                label: "Minimap",
                value: v.minimap,
            });
        }
        SettingsSection::Effects => {
            rows.push(RowKind::Heading("Window effect"));
            // Unified window post-process effect (supersedes the legacy CRT
            // toggle). Eight modes — a dropdown, since a segmented row crams 8
            // labels. Index order mirrors `WindowEffect::index`. Widget id is
            // UNCHANGED from the Appearance section this moved out of, so event
            // routing (`apply_dropdown_toggle`/`apply_segmented_event` in this
            // file, `ev.window_effect`/`window_effect_toggle` in `chrome.rs`) and
            // persistence (`SAVED_KEYS["window_effect"]`) are untouched.
            rows.push(RowKind::Dropdown {
                id: "settings/window_effect",
                label: "Effect",
                text: EFFECT_NAMES
                    .get(v.window_effect_idx)
                    .copied()
                    .unwrap_or("Off"),
                swatch: None,
                which: SettingsDrop::Effect,
            });
            // Custom effect: per-channel intensity sliders so any compatible
            // combination stacks. Only shown when Custom (index 8) is selected.
            // Widget ids UNCHANGED (see above).
            if v.window_effect_idx == 8 {
                for (id, label, ch) in CUSTOM_FX_SLIDERS {
                    rows.push(RowKind::Slider {
                        id,
                        label,
                        value: v.custom_effect.get(*ch).copied().unwrap_or(0.0),
                        min: 0.0,
                        max: 1.0,
                    });
                }
            }
            rows.push(RowKind::Heading("Power Mode"));
            rows.push(RowKind::Toggle {
                id: "settings/power_mode",
                label: "Power mode",
                value: v.power_mode,
            });
            rows.push(RowKind::Slider {
                id: "settings/power_mode_intensity",
                label: "Intensity",
                value: v.power_mode_intensity,
                min: 0.0,
                max: 1.0,
            });
            rows.push(RowKind::Heading("Focus"));
            rows.push(RowKind::Toggle {
                id: "settings/dim_unfocused",
                label: "Dim unfocused panes",
                value: v.dim_unfocused,
            });
            rows.push(RowKind::Heading("Clipboard"));
            rows.push(RowKind::Toggle {
                id: "settings/copy_html",
                label: "Copy as HTML",
                value: v.copy_html,
            });
        }
        SettingsSection::Terminal => {
            rows.push(RowKind::Heading("Hints mode"));
            rows.push(RowKind::TextEdit {
                id: "settings/hints_chars",
                label: "Hint chars",
                which: EditField::HintsChars,
                placeholder: "asdfghjkl… (default)",
            });
            rows.push(RowKind::Heading("Font overrides"));
            rows.push(RowKind::Info(
                "Applies on restart — the running font stack isn't reloaded live.",
            ));
            rows.push(RowKind::TextEdit {
                id: "settings/font_bold",
                label: "Bold font",
                which: EditField::FontBold,
                placeholder: "(synthesized)",
            });
            rows.push(RowKind::TextEdit {
                id: "settings/font_italic",
                label: "Italic font",
                which: EditField::FontItalic,
                placeholder: "(synthesized)",
            });
            rows.push(RowKind::TextEdit {
                id: "settings/font_bold_italic",
                label: "Bold-italic font",
                which: EditField::FontBoldItalic,
                placeholder: "(synthesized)",
            });
            rows.push(RowKind::TextEdit {
                id: "settings/font_symbol_map",
                label: "Symbol map",
                which: EditField::FontSymbolMap,
                placeholder: "U+E000-U+F8FF:Symbols Nerd Font Mono",
            });
            rows.push(RowKind::TextEdit {
                id: "settings/font_variations",
                label: "Font variations",
                which: EditField::FontVariations,
                placeholder: "wght=450 wdth=75",
            });
            rows.push(RowKind::Heading("Startup"));
            rows.push(RowKind::Info(
                "Set in the config file — changing the shell live is unsafe.",
            ));
            rows.push(RowKind::PathField {
                id: "settings/shell",
                label: "Shell",
                text: v.shell_display,
                copyable: false,
            });
            rows.push(RowKind::PathField {
                id: "settings/cwd",
                label: "Startup cwd",
                text: v.cwd_display,
                copyable: false,
            });
        }
        SettingsSection::Themes => {
            rows.push(RowKind::Heading("Wallpaper"));
            rows.push(RowKind::TextEdit {
                id: "settings/wallpaper_theme",
                label: "Wallpaper image",
                which: EditField::WallpaperTheme,
                placeholder: "(disabled) /path/to/image.png",
            });
            rows.push(RowKind::Heading("Active theme"));
            let swatch = v.theme_swatches.get(v.theme_idx).copied();
            rows.push(RowKind::Dropdown {
                id: "settings/theme",
                label: "Theme",
                text: v.theme_names.get(v.theme_idx).copied().unwrap_or(""),
                swatch,
                which: SettingsDrop::Theme,
            });
            rows.push(RowKind::Heading("Follow system"));
            rows.push(RowKind::Toggle {
                id: "settings/follow_system",
                label: "Follow system",
                value: v.follow_system,
            });
            rows.push(RowKind::Dropdown {
                id: "settings/theme_light",
                label: "Light theme",
                text: v.theme_light,
                swatch: v
                    .theme_swatches
                    .get(theme_index(v.theme_names, v.theme_light))
                    .copied(),
                which: SettingsDrop::ThemeLight,
            });
            rows.push(RowKind::Dropdown {
                id: "settings/theme_dark",
                label: "Dark theme",
                text: v.theme_dark,
                swatch: v
                    .theme_swatches
                    .get(theme_index(v.theme_names, v.theme_dark))
                    .copied(),
                which: SettingsDrop::ThemeDark,
            });
            rows.push(RowKind::Heading("Custom theme"));
            rows.push(RowKind::Info(
                "Click a swatch below to edit fg/bg/cursor/ansi.",
            ));
            // Reserve space so the floating editor card never overlaps real rows.
            for _ in 0..5 {
                rows.push(RowKind::Info(""));
            }
        }
        SettingsSection::Keys => {
            rows.push(RowKind::Heading("Keybindings"));
            rows.push(RowKind::Info("Edit [keybindings] in the config file:"));
            rows.push(RowKind::PathField {
                id: "settings/config",
                label: "Config",
                text: v.config_path,
                copyable: true,
            });
            rows.push(RowKind::Info("Press F1 for the full keybinding reference."));
        }
        SettingsSection::Panes => {
            rows.push(RowKind::Heading("Tabs"));
            rows.push(RowKind::Segmented {
                id: "settings/tab_bar",
                label: "Tab bar",
                options: &["Auto", "Always", "Never"],
                sel: v.tab_bar_mode.min(2),
            });
            rows.push(RowKind::Heading("Panes"));
            rows.push(RowKind::Toggle {
                id: "settings/pane_headers",
                label: "Pane headers",
                value: v.pane_headers,
            });
            rows.push(RowKind::Heading("Status"));
            rows.push(RowKind::Toggle {
                id: "settings/status_bar",
                label: "Status bar",
                value: v.status_bar,
            });
            rows.push(RowKind::Heading("Window title"));
            rows.push(RowKind::Toggle {
                id: "settings/title_show_cwd",
                label: "Show cwd",
                value: v.title_show_cwd,
            });
            rows.push(RowKind::Toggle {
                id: "settings/title_show_count",
                label: "Show tab count",
                value: v.title_show_count,
            });
        }
        SettingsSection::Quake => {
            rows.push(RowKind::Heading("Quake / dropdown mode"));
            rows.push(RowKind::Toggle {
                id: "settings/quake",
                label: "Enable quake mode",
                value: v.quake,
            });
            rows.push(RowKind::Info(
                "Restart required — quake mode is only armed at startup.",
            ));
            rows.push(RowKind::Slider {
                id: "settings/quake_height",
                label: "Height",
                value: v.quake_height,
                min: crate::config::parse::QUAKE_HEIGHT_MIN,
                max: crate::config::parse::QUAKE_HEIGHT_MAX,
            });
            rows.push(RowKind::Stepper {
                id: "settings/quake_animation_ms",
                label: "Slide duration",
                text: format!("{} ms", v.quake_animation_ms),
            });
        }
        SettingsSection::Notifications => {
            rows.push(RowKind::Heading("Command finished"));
            rows.push(RowKind::Toggle {
                id: "settings/notify_command_finish",
                label: "Notify when a command finishes",
                value: v.notify_command_finish,
            });
            rows.push(RowKind::Stepper {
                id: "settings/notify_command_threshold_ms",
                label: "Minimum duration",
                text: format!("{} ms", v.notify_command_threshold_ms),
            });
            rows.push(RowKind::Heading("Command output"));
            rows.push(RowKind::Toggle {
                id: "settings/command_fold",
                label: "Allow folding output",
                value: v.command_fold,
            });
            // Moved from the Panes section — widget id UNCHANGED so event
            // routing (`CONFIG_TOGGLES` in `chrome.rs`) and persistence
            // (`SAVED_KEYS["command_badges"]`) are untouched.
            rows.push(RowKind::Toggle {
                id: "settings/command_badges",
                label: "Command badges",
                value: v.command_badges,
            });
        }
        SettingsSection::Advanced => {
            rows.push(RowKind::Heading("Scrollback"));
            rows.push(RowKind::Stepper {
                id: "settings/scrollback",
                label: "Scrollback",
                text: format!("{}", v.scrollback),
            });
            rows.push(RowKind::Heading("Selection"));
            rows.push(RowKind::Toggle {
                id: "settings/copy_on_select",
                label: "Copy on select",
                value: v.copy_on_select,
            });
            rows.push(RowKind::TextEdit {
                id: "settings/word_separator",
                label: "Word seps",
                which: EditField::WordSep,
                placeholder: "(default)",
            });
            rows.push(RowKind::Heading("Shaping"));
            rows.push(RowKind::TextEdit {
                id: "settings/font_features",
                label: "Font features",
                which: EditField::FontFeatures,
                placeholder: "ss01 calt=0 …",
            });
            rows.push(RowKind::Heading("Padding overrides"));
            rows.push(RowKind::Info(
                "Per-side overrides win over the uniform padding above.",
            ));
            rows.push(RowKind::Stepper {
                id: "settings/padding_top",
                label: "Top",
                text: format!("{} px", v.padding_top),
            });
            rows.push(RowKind::Stepper {
                id: "settings/padding_bottom",
                label: "Bottom",
                text: format!("{} px", v.padding_bottom),
            });
            rows.push(RowKind::Stepper {
                id: "settings/padding_left",
                label: "Left",
                text: format!("{} px", v.padding_left),
            });
            rows.push(RowKind::Stepper {
                id: "settings/padding_right",
                label: "Right",
                text: format!("{} px", v.padding_right),
            });
            rows.push(RowKind::Heading("Status bar"));
            rows.push(RowKind::TextEdit {
                id: "settings/status_bar_segments",
                label: "Segments",
                which: EditField::StatusBarSegments,
                placeholder: "(default) cwd git_branch mode …",
            });
            rows.push(RowKind::TextEdit {
                id: "settings/status_bar_time_format",
                label: "Time format",
                which: EditField::StatusBarTimeFormat,
                placeholder: "%H:%M",
            });
            rows.push(RowKind::Heading("Session"));
            rows.push(RowKind::Toggle {
                id: "settings/restore_session",
                label: "Restore session",
                value: v.restore_session,
            });
            rows.push(RowKind::Heading("Config file"));
            rows.push(RowKind::PathField {
                id: "settings/config",
                label: "Config",
                text: v.config_path,
                copyable: true,
            });
        }
        // Runtime `[profile.*]` switching + "duplicate current as a new
        // profile". Split out of Advanced (profiles-ui stream) into its own
        // section — see `SettingsSection::Profiles`'s doc comment.
        SettingsSection::Profiles => {
            rows.push(RowKind::Heading("Profiles"));
            rows.push(RowKind::ProfileDefault {
                active: v.active_profile.is_none(),
            });
            for (i, name) in v.profile_names.iter().enumerate() {
                rows.push(RowKind::Profile {
                    index: i,
                    name,
                    active: v.active_profile == Some(*name),
                });
            }
            rows.push(RowKind::Heading("New profile"));
            rows.push(RowKind::ProfileCreate {
                text_id: "settings/profile_new_name",
                button_id: "settings/profile_new_save",
                placeholder: "name",
            });
            rows.push(RowKind::Info(
                "Duplicates the CURRENT live settings into a new [profile.NAME] section.",
            ));
        }
    }
    rows
}

/// Map a Custom-effect channel slider's new value to its event (carrying the
/// channel index into `custom_effect`).
fn apply_slider_event(wid: &str, nv: f32, ev: &mut SettingsEvents) {
    for entry in CUSTOM_FX_SLIDERS {
        if entry.0 == wid {
            ev.custom_effect = Some((entry.2, nv));
            return;
        }
    }
    match wid {
        "settings/power_mode_intensity" => ev.power_mode_intensity = Some(nv),
        "settings/quake_height" => ev.quake_height = Some(nv),
        _ => {}
    }
}

/// Map a stepper widget id to its `*_delta` event field.
fn apply_stepper_event(wid: &str, delta: i32, ev: &mut SettingsEvents) {
    if delta == 0 {
        return;
    }
    match wid {
        "settings/font_size" => ev.font_delta = delta,
        "settings/scrollback" => ev.scrollback_delta = delta,
        "settings/padding" => ev.padding_delta = delta,
        "settings/padding_top" => ev.padding_top_delta = delta,
        "settings/padding_bottom" => ev.padding_bottom_delta = delta,
        "settings/padding_left" => ev.padding_left_delta = delta,
        "settings/padding_right" => ev.padding_right_delta = delta,
        "settings/quake_animation_ms" => ev.quake_animation_delta = delta,
        "settings/notify_command_threshold_ms" => ev.notify_threshold_delta = delta,
        _ => {}
    }
}

/// Map a segmented widget id to its `Option<usize>` event field.
fn apply_segmented_event(wid: &str, nv: usize, ev: &mut SettingsEvents) {
    match wid {
        "settings/bell" => ev.bell = Some(nv),
        "settings/cursor_style" => ev.cursor_style = Some(nv),
        "settings/tab_bar" => ev.tab_bar_mode = Some(nv),
        "settings/window_effect" => ev.window_effect = Some(nv),
        _ => {}
    }
}

/// Map a dropdown toggle to its `*_toggle` event field.
fn apply_dropdown_toggle(which: SettingsDrop, ev: &mut SettingsEvents) {
    match which {
        SettingsDrop::Theme => ev.theme_toggle = true,
        SettingsDrop::Font => ev.font_toggle = true,
        SettingsDrop::ThemeLight => ev.theme_light_toggle = true,
        SettingsDrop::ThemeDark => ev.theme_dark_toggle = true,
        SettingsDrop::Effect => ev.window_effect_toggle = true,
        SettingsDrop::None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_sections_have_distinct_labels() {
        let labels: Vec<&str> = SettingsSection::ALL.iter().map(|s| s.label()).collect();
        assert_eq!(labels.len(), 11);
        for (i, a) in labels.iter().enumerate() {
            for b in labels.iter().skip(i + 1) {
                assert_ne!(a, b, "section labels must be unique");
            }
        }
    }

    #[test]
    fn section_from_index_clamps() {
        assert_eq!(SettingsSection::from_index(0), SettingsSection::General);
        assert_eq!(SettingsSection::from_index(9), SettingsSection::Advanced);
        // Out of range clamps to the first section.
        assert_eq!(SettingsSection::from_index(99), SettingsSection::General);
    }

    #[test]
    fn theme_index_finds_or_defaults() {
        let names = ["tokyo-night", "dracula", "nord"];
        assert_eq!(theme_index(&names, "dracula"), 1);
        assert_eq!(theme_index(&names, "nord"), 2);
        // Absent name → 0.
        assert_eq!(theme_index(&names, "missing"), 0);
    }

    #[test]
    fn every_section_builds_nonempty_rows() {
        // Build a minimal view; each section must yield at least one row so the
        // pane is never blank.
        let names: [&str; 1] = ["tokyo-night"];
        let sw: [[f32; 4]; 1] = [[0.0; 4]];
        let labels: [&str; 1] = ["fg"];
        let v = SettingsView {
            font_px: 14.0,
            opacity: 1.0,
            bell: 0,
            theme_idx: 0,
            theme_names: &names,
            theme_swatches: &sw,
            font_family: "default",
            font_names: &names,
            font_idx: 0,
            scrollback: 10000,
            config_path: "~/.config/glassy/glassy.conf",
            open: SettingsDrop::None,
            saved: false,
            status_bar: false,
            pane_headers: false,
            follow_system: false,
            ligatures: false,
            restore_session: false,
            padding: 0,
            word_separator: "",
            font_features: "",
            cursor_style_idx: 0,
            cursor_blink: false,
            tab_bar_mode: 0,
            window_effect_idx: 0,
            custom_effect: [0.0; 6],
            section: 0,
            section_scroll: 0.0,
            copy_on_select: false,
            minimap: false,
            command_badges: true,
            cursor_trail: false,
            title_show_cwd: true,
            title_show_count: false,
            theme_light: "tokyo-night",
            theme_dark: "tokyo-night",
            custom_labels: &labels,
            custom_swatches: &sw,
            custom_editing: usize::MAX,
            profile_names: &[],
            active_profile: None,
            power_mode: false,
            power_mode_intensity: 0.6,
            dim_unfocused: true,
            copy_html: false,
            quake: false,
            quake_height: 0.5,
            quake_animation_ms: 180,
            notify_command_finish: true,
            notify_command_threshold_ms: 10_000,
            command_fold: true,
            hints_chars: "",
            font_bold: "",
            font_italic: "",
            font_bold_italic: "",
            font_symbol_map: "",
            font_variations: "",
            shell_display: "(default shell)",
            cwd_display: "",
            status_bar_segments: "",
            status_bar_time_format: "%H:%M",
            padding_top: 0,
            padding_bottom: 0,
            padding_left: 0,
            padding_right: 0,
            wallpaper_theme: "",
        };
        for sec in SettingsSection::ALL {
            let rows = build_section_rows(*sec, &v);
            assert!(!rows.is_empty(), "{:?} should build rows", sec);
        }
    }
}
