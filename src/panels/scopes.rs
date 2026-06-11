//! Scopes-Panel: Luma-Waveform / RGB-Histogramm des aktuellen Programm-Bilds.
//! Bis die Decode-Engine echte Frames liefert, wird das Thumbnail des Clips
//! unter dem Playhead analysiert.

use crate::panels::Panel;
use crate::services::Services;
use crate::state::AppState;
use crate::theme;
use crate::ui::geom::Rect;
use crate::ui::widgets::select::select;
use crate::ui::{FontKind, Ui};
use raylib::prelude::RaylibDraw;

const SAMPLE_W: usize = 160;
const SAMPLE_H: usize = 90;

#[derive(Default)]
pub struct ScopesPanel {
    mode: usize, // 0 = Waveform, 1 = Histogramm
    /// Downgesampelte RGB-Daten des zuletzt analysierten Bilds.
    sample: Option<(String, Vec<[u8; 3]>)>,
}

fn load_sample(path: &str) -> Option<Vec<[u8; 3]>> {
    let mut image = raylib::core::texture::Image::load_image(path).ok()?;
    image.resize(SAMPLE_W as i32, SAMPLE_H as i32);
    let colors = image.get_image_data();
    Some(colors.iter().map(|c| [c.r, c.g, c.b]).collect())
}

impl Panel for ScopesPanel {
    fn update(&mut self, ui: &mut Ui, app: &mut AppState, _services: &Services, rect: Rect) {
        ui.fill(rect, theme::SURFACE_1);
        let mut area = rect;

        // Kopfzeile: Modus-Select links, Label rechts
        let bar = area.cut_top(36.0);
        ui.hline(bar.x, bar.bottom() - 1.0, bar.w, theme::LINE);
        let bar_inner = bar.inset_xy(8.0, 0.0);
        let select_rect = Rect::new(bar_inner.x, bar.y + 6.0, 110.0, 24.0);
        if let Some(m) = select(
            ui,
            "scopes.mode",
            select_rect,
            &["Waveform", "Histogramm"],
            self.mode,
        ) {
            self.mode = m;
        }
        ui.text_right(
            if self.mode == 0 { "Luma" } else { "RGB" },
            bar_inner,
            theme::TEXT_3,
            FontKind::Sans12,
        );

        // Bildquelle: Clip unter dem Playhead (Thumbnail)
        let t = app.timeline.playhead_sec;
        let solo_any = app.timeline.tracks.iter().any(|tr| tr.solo);
        let thumb = app
            .timeline
            .tracks
            .iter()
            .filter(|tr| {
                tr.kind == crate::core::timeline::TrackKind::Video
                    && !tr.muted
                    && (!solo_any || tr.solo)
            })
            .find_map(|tr| {
                app.timeline
                    .clips
                    .iter()
                    .find(|c| c.track_id == tr.id && c.enabled && t >= c.start && t < c.end())
                    .and_then(|c| app.media.asset(&c.asset_id))
                    .and_then(|a| a.thumbnail_path.clone())
            });

        ui.fill(area, theme::BLACK);
        let Some(thumb) = thumb else {
            let center = area.center_box(260.0, 52.0);
            let mut c = center;
            let ic = c.cut_top(20.0);
            ui.icon("monitor-off", ic, 20.0, theme::TEXT_3);
            c.cut_top(8.0);
            ui.text_centered(
                "Kein Bildsignal — Clip in den Programmmonitor laden.",
                c,
                theme::TEXT_3,
                FontKind::Sans12,
            );
            self.sample = None;
            return;
        };

        // Sample bei Bildwechsel neu laden (CPU-seitig, gecacht)
        if self.sample.as_ref().map(|(p, _)| p.as_str()) != Some(thumb.as_str()) {
            self.sample = load_sample(&thumb).map(|data| (thumb.clone(), data));
        }
        let Some((_, data)) = &self.sample else { return };

        // Graticule: Linien bei 0/25/50/75/100 %
        let w = area.w;
        let h = area.h;
        for i in 0..=4 {
            let y = area.y + (h * i as f32 / 4.0).round();
            ui.fill(
                Rect::new(area.x, y.min(area.bottom() - 1.0), w, 1.0),
                raylib::color::Color::new(255, 255, 255, 20),
            );
        }

        if self.mode == 0 {
            // Waveform: Luma je Spalte (additiv hell)
            let col_w = w / SAMPLE_W as f32;
            let color = raylib::color::Color::new(70, 240, 130, 26);
            for x in 0..SAMPLE_W {
                for y in 0..SAMPLE_H {
                    let [r, g, b] = data[y * SAMPLE_W + x];
                    let luma = 0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32;
                    let py = area.y + h - 1.0 - (luma / 255.0) * (h - 2.0);
                    ui.fill(
                        Rect::new(area.x + x as f32 * col_w, py, col_w.max(1.0), 1.0),
                        color,
                    );
                }
            }
        } else {
            // Histogramm: RGB-Verteilungen als Linienzüge
            let mut bins = [[0f32; 256]; 3];
            for px in data {
                bins[0][px[0] as usize] += 1.0;
                bins[1][px[1] as usize] += 1.0;
                bins[2][px[2] as usize] += 1.0;
            }
            let mut max = 1.0f32;
            for channel in &bins {
                for v in channel {
                    max = max.max(*v);
                }
            }
            let colors = [
                raylib::color::Color::new(255, 90, 90, 217),
                raylib::color::Color::new(90, 240, 130, 217),
                raylib::color::Color::new(110, 150, 255, 217),
            ];
            for (c, channel) in bins.iter().enumerate() {
                let mut prev = raylib::math::Vector2::new(area.x, area.y + h);
                for b in 0..256 {
                    let x = area.x + (b as f32 / 255.0) * (w - 1.0);
                    let y = area.y + h - 1.0 - (channel[b] / max) * (h - 4.0);
                    ui.d.draw_line_v(prev, raylib::math::Vector2::new(x, y), colors[c]);
                    prev = raylib::math::Vector2::new(x, y);
                }
            }
        }
    }
}
