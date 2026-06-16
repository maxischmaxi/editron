//! Modale Bestätigungsdialoge der Medienverwaltung: „Ordner löschen?“
//! (Inhalt-Behandlung) und „Verwendete Medien entfernen?“.

use crate::core::proxy::{ProxyCodec, ProxyScale};
use crate::state::AppState;
use crate::stores::DialogId;
use crate::theme;
use crate::ui::geom::Rect;
use crate::ui::widgets::select::select;
use crate::ui::widgets::{drop_shadow, TextButton, TextButtonStyle};
use crate::ui::{FontKind, Interaction, Ui};
use raylib::consts::{KeyboardKey, MouseCursor};

#[derive(Default)]
pub struct MediaDialogs;

impl MediaDialogs {
    pub fn render(&mut self, ui: &mut Ui, state: &mut AppState) {
        match state.app.open_dialog {
            Some(DialogId::DeleteBin) => render_delete_bin(ui, state),
            Some(DialogId::ConfirmRemoveMedia) => render_confirm_remove(ui, state),
            Some(DialogId::ProxySettings) => render_proxy_settings(ui, state),
            Some(DialogId::ConfirmQuitRender) => render_confirm_quit(ui, state),
            Some(DialogId::ConfirmDeleteSequence) => render_confirm_delete_sequence(ui, state),
            Some(DialogId::ConfirmRemoveTrack) => render_confirm_remove_track(ui, state),
            _ => {}
        }
    }
}

/// „Beenden, während Render-Jobs laufen?" — Warnung beim App-Schließen.
fn render_confirm_quit(ui: &mut Ui, state: &mut AppState) {
    let active = state.render_queue.active_count();
    if active == 0 {
        // Inzwischen alle fertig — direkt beenden.
        state.app.quit_requested = true;
        return;
    }
    if esc_pressed(ui) {
        state.app.open_dialog = None;
        return;
    }
    let (mut body, footer) = modal_frame(ui, "triangle-alert", "Render-Jobs laufen noch", 540.0, 200.0);
    let intro = body.cut_top(18.0);
    ui.text_left(
        &format!("{active} Render-Job(s) sind noch aktiv."),
        intro,
        theme::TEXT_1,
        FontKind::Sans12,
    );
    body.cut_top(10.0);
    let note = body.cut_top(36.0);
    ui.text_left(
        "Beim Beenden werden alle laufenden und wartenden Exporte abgebrochen.",
        note,
        theme::TEXT_3,
        FontKind::Sans12,
    );

    ui.hline(footer.x, footer.y, footer.w, theme::LINE);
    let f = footer.inset_xy(16.0, 0.0);
    let quit_rect = Rect::new(f.right() - 210.0, f.y + 12.0, 210.0, 28.0);
    if danger_button(ui, "quit.confirm", quit_rect, "Trotzdem beenden").clicked {
        state.app.quit_requested = true;
        return;
    }
    let keep = TextButton::new("Weiter rendern").style(TextButtonStyle::Outline);
    let kw = keep.measure(ui);
    if keep
        .show(ui, "quit.cancel", Rect::new(quit_rect.x - 8.0 - kw, f.y + 12.0, kw, 28.0))
        .clicked
    {
        state.app.open_dialog = None;
    }
}

/// Roter Sekundär-/Bestätigungs-Button für destruktive Aktionen.
fn danger_button(ui: &mut Ui, id_src: impl std::hash::Hash, rect: Rect, label: &str) -> Interaction {
    let id = ui.id(id_src);
    let it = ui.interact(id, rect);
    let bg = if it.hovered || it.held {
        theme::with_alpha(theme::DANGER, 230)
    } else {
        theme::DANGER
    };
    ui.fill_rounded(rect, theme::RADIUS_SM, bg);
    ui.text_centered(label, rect, theme::WHITE, FontKind::Sans12Medium);
    if it.hovered {
        ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
    }
    it
}

/// Gemeinsames Modal-Gerüst: abdunkeln, Box, Kopfzeile. Liefert (body, footer).
fn modal_frame(ui: &mut Ui, icon: &str, title: &str, w: f32, h: f32) -> (Rect, Rect) {
    ui.fill(ui.screen, theme::with_alpha(theme::BLACK, 130));
    let w = w.min(ui.screen.w - 32.0);
    let h = h.min(ui.screen.h - 32.0);
    let rect = ui.screen.center_box(w, h);
    drop_shadow(ui, rect, theme::RADIUS_LG);
    ui.fill_rounded(rect, theme::RADIUS_LG, theme::SURFACE_1);
    ui.stroke_rounded(rect, theme::RADIUS_LG, 1.0, theme::LINE_STRONG);

    let mut area = rect;
    let head = area.cut_top(48.0);
    ui.hline(head.x, head.bottom() - 1.0, head.w, theme::LINE);
    let mut hi = head.inset_xy(16.0, 0.0);
    let icon_cell = hi.cut_left(18.0);
    ui.icon(icon, icon_cell, 18.0, theme::TEXT_2);
    hi.cut_left(8.0);
    ui.text_left(title, hi, theme::TEXT_1, FontKind::Sans16Semibold);

    let footer = area.cut_bottom(52.0);
    let body = area.inset_xy(16.0, 12.0);
    (body, footer)
}

fn esc_pressed(ui: &Ui) -> bool {
    ui.input.keys.iter().any(|k| k.key == KeyboardKey::KEY_ESCAPE)
}

// --------------------------------------------------------------- Bin löschen

fn render_delete_bin(ui: &mut Ui, state: &mut AppState) {
    let Some(bin_id) = state.app.bin_delete_target.clone() else {
        state.app.open_dialog = None;
        return;
    };
    if !state.media.bin_exists(&bin_id) {
        state.app.bin_delete_target = None;
        state.app.open_dialog = None;
        return;
    }
    if esc_pressed(ui) {
        state.app.bin_delete_target = None;
        state.app.open_dialog = None;
        return;
    }

    let name = state.media.bin_name(&bin_id);
    let asset_count = state.media.count_assets_in_subtree(&bin_id);
    let sub_count = state.media.bin_subtree(&bin_id).len().saturating_sub(1);

    let (mut body, footer) = modal_frame(ui, "trash-2", "Ordner löschen?", 540.0, 232.0);

    let intro = body.cut_top(18.0);
    let label = ui.font(FontKind::Sans12).ellipsize(
        &format!("Der Ordner „{name}“ ist nicht leer."),
        intro.w,
    );
    ui.text_left(&label, intro, theme::TEXT_1, FontKind::Sans12);
    body.cut_top(8.0);

    let mut info = body.cut_top(18.0);
    let cell = info.cut_left(110.0);
    ui.text_left("Enthält", cell, theme::TEXT_3, FontKind::Sans12);
    let contents = format!(
        "{asset_count} Medi{} · {sub_count} Unterordner",
        if asset_count == 1 { "um" } else { "en" }
    );
    ui.text_left(&contents, info, theme::TEXT_1, FontKind::Mono12);
    body.cut_top(12.0);

    let note = body.cut_top(36.0);
    ui.text_left(
        "„Inhalt behalten“ hebt Medien und Unterordner eine Ebene nach oben.",
        note,
        theme::TEXT_3,
        FontKind::Sans12,
    );

    // ---- Buttons ----
    ui.hline(footer.x, footer.y, footer.w, theme::LINE);
    let f = footer.inset_xy(16.0, 0.0);

    // Rechts: destruktives „Ordner + Inhalt löschen“.
    let del_rect = Rect::new(f.right() - 200.0, f.y + 12.0, 200.0, 28.0);
    if danger_button(ui, "media.delbin.all", del_rect, "Ordner + Inhalt löschen").clicked {
        let removed = state.media.delete_bin(&bin_id, false);
        if !removed.is_empty() {
            state.timeline.remove_clips_for_assets(&removed);
            if let Some(src) = state.playback.source_asset_id.clone() {
                if removed.contains(&src) {
                    state.playback.source_asset_id = None;
                    state.playback.source = Default::default();
                }
            }
        }
        state.app.bin_delete_target = None;
        state.app.open_dialog = None;
        return;
    }

    // Mitte: „Inhalt behalten“.
    let keep = TextButton::new("Inhalt behalten").style(TextButtonStyle::Outline);
    let kw = keep.measure(ui).max(140.0);
    if keep
        .show(ui, "media.delbin.keep", Rect::new(del_rect.x - 8.0 - kw, f.y + 12.0, kw, 28.0))
        .clicked
    {
        state.media.delete_bin(&bin_id, true);
        state.app.bin_delete_target = None;
        state.app.open_dialog = None;
        return;
    }

    // Links: Abbrechen.
    let cancel = TextButton::new("Abbrechen").style(TextButtonStyle::Ghost);
    let cw = cancel.measure(ui).max(96.0);
    if cancel.show(ui, "media.delbin.cancel", Rect::new(f.x, f.y + 12.0, cw, 28.0)).clicked {
        state.app.bin_delete_target = None;
        state.app.open_dialog = None;
    }
}

// ----------------------------------------------------- Proxy-Einstellungen

fn render_proxy_settings(ui: &mut Ui, state: &mut AppState) {
    if esc_pressed(ui) {
        state.app.open_dialog = None;
        return;
    }

    let (mut body, footer) = modal_frame(ui, "gauge", "Proxy-Einstellungen", 560.0, 332.0);

    let intro = body.cut_top(36.0);
    ui.text_left(
        "Format und Auflösung für neu erstellte Proxys. Der Export verwendet\nstets die Originale.",
        intro,
        theme::TEXT_3,
        FontKind::Sans12,
    );
    body.cut_top(8.0);

    // ---- Codec ----
    let codecs = ProxyCodec::ALL;
    let codec_labels: Vec<&str> = codecs.iter().map(|c| c.label()).collect();
    let codec_idx = codecs
        .iter()
        .position(|c| *c == state.media.proxy_settings.codec)
        .unwrap_or(0);
    let mut row = body.cut_top(34.0);
    let cell = row.cut_left(140.0);
    ui.text_left("Codec", Rect::new(cell.x, cell.y + 6.0, cell.w, 24.0), theme::TEXT_2, FontKind::Sans12);
    let sel = Rect::new(row.x, row.y + 3.0, row.w.min(280.0), 26.0);
    if let Some(i) = select(ui, "proxy.codec", sel, &codec_labels, codec_idx) {
        state.media.proxy_settings.codec = codecs[i];
    }
    body.cut_top(6.0);

    // ---- Auflösung ----
    let scales = ProxyScale::ALL;
    let scale_labels: Vec<&str> = scales.iter().map(|s| s.label()).collect();
    let scale_idx = scales
        .iter()
        .position(|s| *s == state.media.proxy_settings.scale)
        .unwrap_or(0);
    let mut row = body.cut_top(34.0);
    let cell = row.cut_left(140.0);
    ui.text_left("Auflösung", Rect::new(cell.x, cell.y + 6.0, cell.w, 24.0), theme::TEXT_2, FontKind::Sans12);
    let sel = Rect::new(row.x, row.y + 3.0, row.w.min(280.0), 26.0);
    if let Some(i) = select(ui, "proxy.scale", sel, &scale_labels, scale_idx) {
        state.media.proxy_settings.scale = scales[i];
    }
    body.cut_top(6.0);

    // ---- Ablageordner (konfigurierbar) ----
    let mut row = body.cut_top(34.0);
    let cell = row.cut_left(140.0);
    ui.text_left("Ordner", Rect::new(cell.x, cell.y + 6.0, cell.w, 24.0), theme::TEXT_2, FontKind::Sans12);
    let folder_label = state
        .media
        .proxy_settings
        .folder
        .clone()
        .unwrap_or_else(|| "Standard (neben dem Projekt)".to_string());
    // Buttons rechts: Ändern… + (falls eigener Ordner) Standard.
    let change = TextButton::new("Ändern…").style(TextButtonStyle::Outline);
    let cw = change.measure(ui).max(80.0);
    if change
        .show(ui, "proxy.folder.change", Rect::new(row.right() - cw, row.y + 3.0, cw, 26.0))
        .clicked
    {
        ui.run_command("proxy.pickFolder");
    }
    let mut path_w = row.w - cw - 8.0;
    if state.media.proxy_settings.folder.is_some() {
        let reset = TextButton::new("Standard").style(TextButtonStyle::Ghost);
        let rw = reset.measure(ui).max(72.0);
        if reset
            .show(ui, "proxy.folder.reset", Rect::new(row.right() - cw - 8.0 - rw, row.y + 3.0, rw, 26.0))
            .clicked
        {
            ui.run_command("proxy.resetFolder");
        }
        path_w -= rw + 8.0;
    }
    let disp = ui.font(FontKind::Mono12).ellipsize(&folder_label, path_w.max(40.0));
    ui.text_left(&disp, Rect::new(row.x, row.y + 6.0, path_w.max(40.0), 24.0), theme::TEXT_1, FontKind::Mono12);
    body.cut_top(10.0);

    let note = body.cut_top(36.0);
    ui.text_left(
        "Halbe Auflösung ist der Standard; Viertel für sehr schwache Hardware.",
        note,
        theme::TEXT_3,
        FontKind::Sans12,
    );

    // ---- Buttons ----
    ui.hline(footer.x, footer.y, footer.w, theme::LINE);
    let f = footer.inset_xy(16.0, 0.0);
    let done = TextButton::new("Fertig").style(TextButtonStyle::Solid);
    let dw = done.measure(ui).max(96.0);
    if done
        .show(ui, "proxy.settings.done", Rect::new(f.right() - dw, f.y + 12.0, dw, 28.0))
        .clicked
    {
        state.app.open_dialog = None;
    }
}

// --------------------------------------------- Verschachtelte Sequenz löschen

fn render_confirm_delete_sequence(ui: &mut Ui, state: &mut AppState) {
    let Some(target) = state.app.sequence_delete_target.clone() else {
        state.app.open_dialog = None;
        return;
    };
    if state.timeline.get(&target).is_none() {
        state.app.sequence_delete_target = None;
        state.app.open_dialog = None;
        return;
    }
    if esc_pressed(ui) {
        state.app.sequence_delete_target = None;
        state.app.open_dialog = None;
        return;
    }

    let name = state.timeline.name_of(&target).unwrap_or("Sequenz").to_string();
    let uses = state.timeline.nest_usage_count(&target);

    let (mut body, footer) = modal_frame(ui, "triangle-alert", "Sequenz löschen?", 540.0, 212.0);
    let intro = body.cut_top(18.0);
    let label = ui.font(FontKind::Sans12).ellipsize(
        &format!("Die Sequenz „{name}“ wird als verschachtelte Sequenz verwendet."),
        intro.w,
    );
    ui.text_left(&label, intro, theme::TEXT_1, FontKind::Sans12);
    body.cut_top(10.0);
    let note = body.cut_top(36.0);
    ui.text_left(
        &format!(
            "{uses} Nest-Clip(s) in anderen Sequenzen werden mit gelöscht.",
        ),
        note,
        theme::TEXT_3,
        FontKind::Sans12,
    );

    ui.hline(footer.x, footer.y, footer.w, theme::LINE);
    let f = footer.inset_xy(16.0, 0.0);
    let del_rect = Rect::new(f.right() - 200.0, f.y + 12.0, 200.0, 28.0);
    if danger_button(ui, "seq.del.confirm", del_rect, "Sequenz + Nests löschen").clicked {
        state.timeline.remove(&target);
        state.app.sequence_delete_target = None;
        state.app.open_dialog = None;
        return;
    }
    let keep = TextButton::new("Abbrechen").style(TextButtonStyle::Outline);
    let kw = keep.measure(ui).max(96.0);
    if keep
        .show(ui, "seq.del.cancel", Rect::new(del_rect.x - 8.0 - kw, f.y + 12.0, kw, 28.0))
        .clicked
    {
        state.app.sequence_delete_target = None;
        state.app.open_dialog = None;
    }
}

// --------------------------------------------------------- Belegte Spur entfernen

fn render_confirm_remove_track(ui: &mut Ui, state: &mut AppState) {
    let Some(target) = state.app.remove_track_target.clone() else {
        state.app.open_dialog = None;
        return;
    };
    let Some(track) = state.timeline.tracks.iter().find(|t| t.id == target).cloned() else {
        state.app.remove_track_target = None;
        state.app.open_dialog = None;
        return;
    };
    if esc_pressed(ui) {
        state.app.remove_track_target = None;
        state.app.open_dialog = None;
        return;
    }

    let name = crate::core::timeline::track_name(&track, &state.timeline.tracks);
    let clips = state.timeline.track_clip_count(&target);

    let (mut body, footer) = modal_frame(ui, "triangle-alert", "Spur entfernen?", 540.0, 212.0);
    let intro = body.cut_top(18.0);
    ui.text_left(
        &format!("Die Spur „{name}“ enthält {clips} Clip(s)."),
        intro,
        theme::TEXT_1,
        FontKind::Sans12,
    );
    body.cut_top(10.0);
    let note = body.cut_top(36.0);
    ui.text_left(
        "Beim Entfernen der Spur werden diese Clips mit gelöscht.",
        note,
        theme::TEXT_3,
        FontKind::Sans12,
    );

    ui.hline(footer.x, footer.y, footer.w, theme::LINE);
    let f = footer.inset_xy(16.0, 0.0);
    let del_rect = Rect::new(f.right() - 200.0, f.y + 12.0, 200.0, 28.0);
    if danger_button(ui, "track.del.confirm", del_rect, "Spur + Clips entfernen").clicked {
        state.timeline.remove_track(&target);
        state.app.remove_track_target = None;
        state.app.open_dialog = None;
        return;
    }
    let keep = TextButton::new("Abbrechen").style(TextButtonStyle::Outline);
    let kw = keep.measure(ui).max(96.0);
    if keep
        .show(ui, "track.del.cancel", Rect::new(del_rect.x - 8.0 - kw, f.y + 12.0, kw, 28.0))
        .clicked
    {
        state.app.remove_track_target = None;
        state.app.open_dialog = None;
    }
}

// ------------------------------------------------- Verwendete Medien entfernen

fn render_confirm_remove(ui: &mut Ui, state: &mut AppState) {
    let ids = state.media.selected_asset_ids.clone();
    if ids.is_empty() {
        state.app.open_dialog = None;
        return;
    }
    if esc_pressed(ui) {
        state.app.open_dialog = None;
        return;
    }

    let used: usize = ids
        .iter()
        .filter(|id| state.timeline.asset_usage_count(id) > 0)
        .count();
    let total = ids.len();

    let (mut body, footer) = modal_frame(ui, "triangle-alert", "Verwendete Medien entfernen?", 540.0, 212.0);

    let intro = body.cut_top(18.0);
    let msg = if total == 1 {
        "Dieses Medium wird in der Sequenz verwendet.".to_string()
    } else {
        format!("{used} von {total} ausgewählten Medien werden in der Sequenz verwendet.")
    };
    ui.text_left(&msg, intro, theme::TEXT_1, FontKind::Sans12);
    body.cut_top(10.0);
    let note = body.cut_top(36.0);
    ui.text_left(
        "Beim Entfernen werden auch die zugehörigen Clips aus der Timeline gelöscht.",
        note,
        theme::TEXT_3,
        FontKind::Sans12,
    );

    // ---- Buttons ----
    ui.hline(footer.x, footer.y, footer.w, theme::LINE);
    let f = footer.inset_xy(16.0, 0.0);
    let remove_rect = Rect::new(f.right() - 170.0, f.y + 12.0, 170.0, 28.0);
    if danger_button(ui, "media.rm.confirm", remove_rect, "Entfernen").clicked {
        ui.run_command("media.removeSelectedConfirmed");
        // Der Command schließt den Dialog selbst.
        return;
    }
    let keep = TextButton::new("Behalten").style(TextButtonStyle::Outline);
    let kw = keep.measure(ui).max(96.0);
    if keep
        .show(ui, "media.rm.cancel", Rect::new(remove_rect.x - 8.0 - kw, f.y + 12.0, kw, 28.0))
        .clicked
    {
        state.app.open_dialog = None;
    }
}
