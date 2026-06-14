//! Info-Panel: Metadaten + Stream-Tabellen des ausgewählten Assets.

use crate::core::timecode::format_timecode;
use crate::panels::Panel;
use crate::services::Services;
use crate::state::AppState;
use crate::theme;
use crate::ui::geom::Rect;
use crate::ui::widgets::scroll::ScrollState;
use crate::ui::{FontKind, Ui};

#[derive(Default)]
pub struct InfoPanel {
    scroll: ScrollState,
}

fn format_megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1e6)
}

fn format_video_bitrate(bitrate: Option<u64>) -> String {
    match bitrate {
        Some(b) if b > 0 => format!("{:.1} Mbit/s", b as f64 / 1e6),
        _ => "—".into(),
    }
}

fn format_audio_bitrate(bitrate: Option<u64>) -> String {
    match bitrate {
        Some(b) if b > 0 => format!("{} kbit/s", (b as f64 / 1e3).round() as i64),
        _ => "—".into(),
    }
}

fn format_fps(fps: f64) -> String {
    if fps.fract() == 0.0 {
        format!("{fps:.0}")
    } else {
        format!("{fps:.2}")
    }
}

fn format_sample_rate(sample_rate: u32) -> String {
    let khz = sample_rate as f64 / 1000.0;
    if khz.fract() == 0.0 {
        format!("{khz:.0} kHz")
    } else {
        format!("{khz:.1} kHz")
    }
}

/// Proxy-Status eines Assets als Klartext fürs Info-Panel.
fn proxy_info_text(media: &crate::stores::MediaStore, asset: &crate::core::types::MediaAsset) -> String {
    use crate::stores::ProxyJobStatus;
    match media.proxy_status(&asset.id) {
        Some(ProxyJobStatus::Building(p)) => format!("wird erstellt — {} %", (p * 100.0).round() as u32),
        Some(ProxyJobStatus::Failed(e)) => format!("Fehler ({e})"),
        None => match &asset.proxy_path {
            None => "—".into(),
            Some(_) if asset.proxy_offline => "fehlt/veraltet".into(),
            Some(_) => "vorhanden".into(),
        },
    }
}

fn format_imported_at(secs: f64) -> String {
    // Lokale Zeit über `date` (vermeidet eine Chrono-Abhängigkeit).
    let out = std::process::Command::new("date")
        .arg("-d")
        .arg(format!("@{}", secs as i64))
        .arg("+%d.%m.%Y, %H:%M")
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => "—".into(),
    }
}

impl Panel for InfoPanel {
    fn update(&mut self, ui: &mut Ui, app: &mut AppState, _services: &Services, rect: Rect) {
        ui.fill(rect, theme::SURFACE_1);

        let asset = app
            .media
            .selected_asset_ids
            .first()
            .and_then(|id| app.media.asset(id))
            .or_else(|| {
                app.playback
                    .source_asset_id
                    .as_ref()
                    .and_then(|id| app.media.asset(id))
            })
            .cloned();

        let Some(asset) = asset else {
            let center = rect.center_box(160.0, 48.0);
            let mut c = center;
            let ic = c.cut_top(20.0);
            ui.icon("info", ic, 20.0, theme::TEXT_3);
            c.cut_top(8.0);
            ui.text_centered("Clip auswählen", c, theme::TEXT_3, FontKind::Sans12);
            return;
        };

        let info = &asset.info;
        let fps = info.video.first().map(|v| v.fps).unwrap_or(25.0);

        // Metadaten-Zeilen (vor der Höhenschätzung zusammenstellen).
        let usage = app.timeline.asset_usage_count(&asset.id);
        let usage_text = if usage == 0 {
            "nicht verwendet".to_string()
        } else {
            format!("{usage}×")
        };
        let bin_label = app.media.bin_path_label(app.media.effective_bin(&asset));
        let meta_rows: Vec<(&str, String)> = vec![
            ("Container", info.container.clone()),
            ("Dauer", format_timecode(info.duration_sec, fps)),
            ("Größe", format_megabytes(info.size_bytes)),
            (
                "Aufnahme",
                info.recorded_at
                    .map(crate::panels::media_browser::format_unix_date)
                    .unwrap_or_else(|| "—".into()),
            ),
            ("Importiert", format_imported_at(asset.imported_at)),
            ("Ordner", bin_label),
            ("Verwendung", usage_text),
            ("Proxy", proxy_info_text(&app.media, &asset)),
        ];

        // Inhaltshöhe abschätzen (Header + Grid + Tabellen)
        let video_rows = info.video.len();
        let audio_rows = info.audio.len();
        let mut content_h = 12.0 + 20.0 + 18.0 + 12.0 + meta_rows.len() as f32 * 20.0;
        if video_rows > 0 {
            content_h += 16.0 + 20.0 + 24.0 + video_rows as f32 * 24.0;
        }
        if audio_rows > 0 {
            content_h += 16.0 + 20.0 + 24.0 + audio_rows as f32 * 24.0;
        }
        content_h += 12.0;

        let view = self.scroll.begin(ui, rect, 0.0, content_h);
        let inner_x = view.viewport.x + 12.0;
        let inner_w = view.viewport.w - 24.0;
        let mut y = view.origin_y + 12.0;

        // Titel + Pfad
        let name = ui.font(FontKind::Sans14Semibold).ellipsize(&asset.name, inner_w);
        ui.text(
            &name,
            crate::ui::geom::v2(inner_x, y),
            theme::TEXT_1,
            FontKind::Sans14Semibold,
        );
        y += 20.0;
        let path = ui.font(FontKind::Sans12).ellipsize(&asset.path, inner_w);
        ui.text(&path, crate::ui::geom::v2(inner_x, y), theme::TEXT_3, FontKind::Sans12);
        y += 18.0 + 12.0;

        // Metadaten-Grid (2 Spalten)
        let label_w = inner_w / 2.0;
        for (label, value) in meta_rows {
            ui.text(
                label,
                crate::ui::geom::v2(inner_x, y),
                theme::TEXT_2,
                FontKind::Sans12,
            );
            let value = ui.font(FontKind::Mono12).ellipsize(&value, inner_w - label_w);
            ui.text(
                &value,
                crate::ui::geom::v2(inner_x + label_w, y),
                theme::TEXT_1,
                FontKind::Mono12,
            );
            y += 20.0;
        }

        // Stream-Tabellen
        let table = |ui: &mut Ui,
                         y: &mut f32,
                         title: &str,
                         headers: &[&str],
                         rows: Vec<Vec<String>>| {
            *y += 16.0;
            ui.text(
                title,
                crate::ui::geom::v2(inner_x, *y),
                theme::TEXT_2,
                FontKind::Sans12Medium,
            );
            *y += 20.0;
            let col_w = inner_w / headers.len() as f32;
            for (i, h) in headers.iter().enumerate() {
                ui.text(
                    h,
                    crate::ui::geom::v2(inner_x + i as f32 * col_w + 8.0, *y + 4.0),
                    theme::TEXT_3,
                    FontKind::Sans12,
                );
            }
            *y += 24.0;
            ui.hline(inner_x, *y - 1.0, inner_w, theme::LINE);
            for row in rows {
                for (i, cell) in row.iter().enumerate() {
                    let text = ui.font(FontKind::Mono12).ellipsize(cell, col_w - 12.0);
                    ui.text(
                        &text,
                        crate::ui::geom::v2(inner_x + i as f32 * col_w + 8.0, *y + 4.0),
                        theme::TEXT_1,
                        FontKind::Mono12,
                    );
                }
                *y += 24.0;
                ui.hline(inner_x, *y - 1.0, inner_w, theme::LINE);
            }
        };

        if !info.video.is_empty() {
            table(
                ui,
                &mut y,
                "Video-Streams",
                &["Codec", "Auflösung", "fps", "Pixelformat", "Bitrate"],
                info.video
                    .iter()
                    .map(|v| {
                        vec![
                            v.codec.clone(),
                            format!("{}×{}", v.width, v.height),
                            format_fps(v.fps),
                            v.pix_fmt.clone().unwrap_or_else(|| "—".into()),
                            format_video_bitrate(v.bitrate),
                        ]
                    })
                    .collect(),
            );
        }
        if !info.audio.is_empty() {
            table(
                ui,
                &mut y,
                "Audio-Streams",
                &["Codec", "Kanäle", "Samplerate", "Bitrate"],
                info.audio
                    .iter()
                    .map(|a| {
                        vec![
                            a.codec.clone(),
                            a.channels.to_string(),
                            format_sample_rate(a.sample_rate),
                            format_audio_bitrate(a.bitrate),
                        ]
                    })
                    .collect(),
            );
        }

        self.scroll.end(ui, rect, 0.0, content_h);
    }
}

/// Verlauf-Panel (Platzhalter).
#[derive(Default)]
pub struct HistoryPanel;

impl Panel for HistoryPanel {
    fn update(&mut self, ui: &mut Ui, _app: &mut AppState, _services: &Services, rect: Rect) {
        ui.fill(rect, theme::SURFACE_1);
        let mut area = rect;
        let head = area.cut_top(110.0);
        let center = head.center_box(260.0, 52.0);
        let mut c = center;
        let ic = c.cut_top(20.0);
        ui.icon("clock", ic, 20.0, theme::TEXT_3);
        c.cut_top(8.0);
        ui.text_centered(
            "Der Verlauf zeichnet künftig jeden Bearbeitungsschritt auf.",
            c,
            theme::TEXT_3,
            FontKind::Sans12,
        );

        let examples: [(&str, &str); 3] = [
            ("Import: clip_0042.mp4", "import"),
            ("Schnitt bei 00:00:12:08", "scissors"),
            ("Effekt: Gaußscher Weichzeichner", "sparkles"),
        ];
        let mut y = area.y;
        for (name, icon) in examples {
            let row = Rect::new(area.x + 8.0, y, area.w - 16.0, 28.0);
            let mut ri = row.inset_xy(8.0, 0.0);
            let ic = ri.cut_left(14.0);
            ui.icon(icon, ic, 14.0, theme::with_alpha(theme::TEXT_3, 153));
            ri.cut_left(8.0);
            let badge_w = ui.font(FontKind::Mono12).width("Vorschau") + 8.0;
            let badge = ri.cut_right(badge_w);
            let badge = Rect::new(badge.x, row.y + 6.0, badge_w, 17.0);
            ui.stroke_rounded(badge, theme::RADIUS_SM, 1.0, theme::LINE);
            ui.text_centered(
                "Vorschau",
                badge,
                theme::with_alpha(theme::TEXT_3, 153),
                FontKind::Mono12,
            );
            ri.cut_right(8.0);
            ui.text_left(name, ri, theme::with_alpha(theme::TEXT_3, 153), FontKind::Sans12);
            y += 28.0;
        }
    }
}
