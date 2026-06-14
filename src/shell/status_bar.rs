//! Fußleiste: Status/Werkzeug/Workspace links; Medienanzahl, Keymap-Preset,
//! FFmpeg-Version rechts.

use crate::core::timecode::format_duration;
use crate::state::AppState;
use crate::stores::{tool_label, workspace_name};
use crate::theme;
use crate::ui::geom::Rect;
use crate::ui::{Dispatch, FontKind, Ui};
use raylib::consts::MouseCursor;

/// Text des Export-Chips: laufender Job (Prozent + ETA + wartende) oder, falls
/// keiner läuft, die Anzahl wartender Jobs. `None` = Chip ausblenden.
fn export_chip_text(app: &AppState) -> Option<String> {
    let q = &app.render_queue;
    if let Some(job) = q.running() {
        let mut t = format!("Export {:.0} %", job.progress_pct);
        if let Some(eta) = job.eta_sec.filter(|e| e.is_finite() && *e > 0.0) {
            t.push_str(&format!(" · noch {}", format_duration(eta)));
        }
        let waiting = q.waiting_count();
        if waiting > 0 {
            t.push_str(&format!(" · +{waiting}"));
        }
        Some(t)
    } else {
        let waiting = q.waiting_count();
        if waiting > 0 {
            let suffix = if q.paused { " (pausiert)" } else { "" };
            Some(format!("Warteschlange: {waiting} wartend{suffix}"))
        } else {
            None
        }
    }
}

fn divider(ui: &mut Ui, x: f32, rect: Rect) {
    // h-3 w-px bg-line-strong
    ui.fill(
        Rect::new(x, rect.y + (rect.h - 12.0) / 2.0, 1.0, 12.0),
        theme::LINE_STRONG,
    );
}

pub fn render(ui: &mut Ui, app: &AppState, rect: Rect) {
    ui.fill(rect, theme::SURFACE_0);
    ui.hline(rect.x, rect.y, rect.w, theme::LINE);

    let font = FontKind::Sans12;
    let inner = rect.inset_xy(12.0, 0.0);

    // ---- Rechts zuerst vermessen, damit links korrekt truncated ----
    let seq_label = app.timeline.settings.describe();
    let asset_count = app.media.assets.len();
    let assets_label = if asset_count == 1 {
        "1 Medium".to_string()
    } else {
        format!("{asset_count} Medien")
    };
    let preset_label = format!("Tastatur: {}", app.keymap.active_preset().name);
    let ffmpeg_label = match &app.app.ffmpeg {
        None => "FFmpeg …".to_string(),
        Some(info) if info.available => {
            format!("FFmpeg {}", info.version.as_deref().unwrap_or("—"))
        }
        Some(_) => "FFmpeg fehlt".to_string(),
    };
    let ffmpeg_missing = matches!(&app.app.ffmpeg, Some(info) if !info.available);
    let ffmpeg_kind = if ffmpeg_missing {
        FontKind::Sans12
    } else {
        FontKind::Mono12
    };

    let mut x = inner.right();
    let w = ui.font(ffmpeg_kind).width(&ffmpeg_label);
    x -= w;
    ui.text_left(
        &ffmpeg_label,
        Rect::new(x, inner.y, w, inner.h),
        if ffmpeg_missing { theme::DANGER } else { theme::TEXT_3 },
        ffmpeg_kind,
    );
    x -= 8.0 + 1.0;
    divider(ui, x, rect);
    x -= 8.0;
    let w = ui.font(font).width(&preset_label);
    x -= w;
    ui.text_left(
        &preset_label,
        Rect::new(x, inner.y, w, inner.h),
        theme::TEXT_3,
        font,
    );
    x -= 8.0 + 1.0;
    divider(ui, x, rect);
    x -= 8.0;
    let w = ui.font(font).width(&assets_label);
    x -= w;
    ui.text_left(
        &assets_label,
        Rect::new(x, inner.y, w, inner.h),
        theme::TEXT_3,
        font,
    );
    // Sequenz-Format (Auflösung · Framerate · DF) wie in Resolve/Premiere.
    x -= 8.0 + 1.0;
    divider(ui, x, rect);
    x -= 8.0;
    let w = ui.font(FontKind::Mono12).width(&seq_label);
    x -= w;
    ui.text_left(
        &seq_label,
        Rect::new(x, inner.y, w, inner.h),
        theme::TEXT_3,
        FontKind::Mono12,
    );

    let right_edge = x - 16.0;

    // ---- Links: Status | Werkzeug | Workspace ----
    let status = app.app.status_message.as_deref().unwrap_or("Bereit");
    let tool = format!("Werkzeug: {}", tool_label(app.app.active_tool));
    let ws = workspace_name(&app.app.active_workspace);

    let tool_w = ui.font(font).width(&tool);
    let ws_w = ui.font(font).width(ws);
    let fixed = 8.0 + 1.0 + 8.0 + tool_w + 8.0 + 1.0 + 8.0 + ws_w;
    let status_max = (right_edge - inner.x - fixed).max(40.0);
    let status_text = ui.font(font).ellipsize(status, status_max);
    let status_color = if app.app.status_message.is_some() {
        theme::TEXT_2
    } else {
        theme::TEXT_3
    };

    let mut x = inner.x;
    let w = ui.font(font).width(&status_text);
    ui.text_left(
        &status_text,
        Rect::new(x, inner.y, w, inner.h),
        status_color,
        font,
    );
    x += w + 8.0;

    // ---- Export-Fortschritts-Chip (klickbar → Warteschlange) ----
    if let Some(chip) = export_chip_text(app) {
        divider(ui, x, rect);
        x += 1.0 + 8.0;
        let cw = ui.font(font).width(&chip) + 22.0;
        let chip_rect = Rect::new(x, inner.y, cw, inner.h);
        let id = ui.id("statusbar.exportChip");
        let it = ui.interact(id, chip_rect);
        if it.hovered {
            ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
            ui.tooltip(id, chip_rect, "Render-Warteschlange öffnen");
        }
        if it.clicked {
            ui.dispatch.push(Dispatch {
                command: "queue.open".to_string(),
                arg: None,
            });
        }
        let color = if it.hovered { theme::TEXT_1 } else { theme::ACCENT };
        let icon_cell = Rect::new(x, inner.y + (inner.h - 14.0) / 2.0, 14.0, 14.0);
        ui.icon("loader-circle", icon_cell, 13.0, color);
        ui.text_left(&chip, Rect::new(x + 18.0, inner.y, cw - 18.0, inner.h), color, font);
        x += cw + 8.0;
    }

    divider(ui, x, rect);
    x += 1.0 + 8.0;
    ui.text_left(&tool, Rect::new(x, inner.y, tool_w, inner.h), theme::TEXT_3, font);
    x += tool_w + 8.0;
    divider(ui, x, rect);
    x += 1.0 + 8.0;
    ui.text_left(ws, Rect::new(x, inner.y, ws_w, inner.h), theme::TEXT_3, font);
}
