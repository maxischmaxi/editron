//! Titel-Engine für den Programmmonitor: rastert sichtbare Titel-Clips am
//! Playhead CPU-seitig (gemeinsamer Rasterizer mit dem Export —
//! `core/text_raster.rs`) und lädt das Ergebnis zwischen den Frames als
//! Textur hoch (`title://clip/<id>`), analog zu den Decode-Texturen des
//! Players. Gerastert wird in Canvas-Auflösung des Monitors (scharf, kein
//! Hochskalieren), bei Zoom-Animationen überabgetastet wie im Export.

use crate::core::compose;
use crate::core::text_raster;
use crate::state::AppState;
use crate::ui::textures::TextureCache;
use raylib::core::texture::{Image, RaylibTexture2D};
use raylib::{RaylibHandle, RaylibThread};
use std::collections::HashMap;

/// Texture-Schlüssel eines Titel-Layers.
pub fn title_texture_key(clip_id: &str) -> String {
    format!("title://clip/{clip_id}")
}

struct CachedTitle {
    hash: u64,
    w: usize,
    h: usize,
}

#[derive(Default)]
pub struct TitleEngine {
    cache: HashMap<String, CachedTitle>,
}

impl TitleEngine {
    /// Pro Frame im Mainloop aufrufen (nach dem Player-Tick, vor dem Zeichnen).
    pub fn tick(
        &mut self,
        rl: &mut RaylibHandle,
        thread: &RaylibThread,
        state: &mut AppState,
        textures: &mut TextureCache,
    ) {
        let t = state.timeline.playhead_sec;
        // Sichtbare Text-Layer — Titel UND Untertitel-Segmente (inkl.
        // Übergangs-Verlängerung — dieselbe Layer-Auflösung wie Monitor
        // und Export).
        let visible: Vec<(String, u64, usize, usize)> =
            compose::visible_program_layers(&state.timeline, t)
                .into_iter()
                .filter_map(|layer| match layer {
                    compose::ProgramLayer::Clip { clip, .. } => {
                        let spec = compose::layer_title_spec(&state.timeline, clip)?;
                        // Canvas-Auflösung des Monitors (vom Panel gemeldet);
                        // Fallback: Sequenzauflösung, bis das Panel einmal lief.
                        let (cw, ch) = state.monitor.program_canvas;
                        let (cw, ch) = if cw >= 8 && ch >= 8 {
                            (cw, ch)
                        } else {
                            (state.timeline.settings.width, state.timeline.settings.height)
                        };
                        // Zoom-Animationen überabtasten (wie der Export).
                        // Medienzeit-Fenster des Clips (speed-bewusst).
                        let over = compose::max_scale_in_window(
                            &clip.fx,
                            clip.media_in(),
                            clip.media_out(),
                        )
                        .clamp(1.0, 2.0);
                        let w = ((cw as f64 * over).round() as usize).clamp(8, 4096);
                        let h = ((ch as f64 * over).round() as usize).clamp(8, 4096);
                        let mut hash = spec.content_hash();
                        hash = hash
                            .wrapping_mul(31)
                            .wrapping_add(w as u64)
                            .wrapping_mul(31)
                            .wrapping_add(h as u64);
                        Some((clip.id.clone(), hash, w, h))
                    }
                    compose::ProgramLayer::Solid { .. } => None,
                })
                .collect();

        // Nicht mehr sichtbare Titel entsorgen (Textur + Scope-Mini).
        let minis = &mut state.monitor.preview_frames;
        self.cache.retain(|clip_id, _| {
            let keep = visible.iter().any(|(id, ..)| id == clip_id);
            if !keep {
                textures.remove(&title_texture_key(clip_id));
                minis.remove(clip_id);
            }
            keep
        });

        for (clip_id, hash, w, h) in visible {
            let fresh = self
                .cache
                .get(&clip_id)
                .map(|c| c.hash != hash || c.w != w || c.h != h)
                .unwrap_or(true);
            if !fresh {
                continue;
            }
            let Some(spec) = state
                .timeline
                .clip(&clip_id)
                .and_then(|c| compose::layer_title_spec(&state.timeline, c))
            else {
                continue;
            };
            let raster = text_raster::render_title(&spec, w as u32, h as u32);
            let key = title_texture_key(&clip_id);
            let existing = textures.get(&key).map(|t| (t.width, t.height));
            if existing != Some((raster.w as i32, raster.h as i32)) {
                let image =
                    Image::gen_image_color(raster.w as i32, raster.h as i32, raylib::color::Color::BLANK);
                if let Ok(tex) = rl.load_texture_from_image(thread, &image) {
                    tex.set_texture_filter(
                        thread,
                        raylib::consts::TextureFilter::TEXTURE_FILTER_BILINEAR,
                    );
                    textures.put(&key, tex);
                }
            }
            if let Some(tex) = textures.get_mut(&key) {
                let _ = tex.update_texture(&raster.data);
            }
            // Scope-Mini (wie die Player-Decode-Frames, ≤ 192 px, nearest).
            let mw = raster.w.min(192).max(1);
            let mh = (raster.h * mw / raster.w.max(1)).max(1);
            let mut rgba = vec![0u8; mw * mh * 4];
            for y in 0..mh {
                let sy = y * raster.h / mh;
                for x in 0..mw {
                    let sx = x * raster.w / mw;
                    let src = (sy * raster.w + sx) * 4;
                    let dst = (y * mw + x) * 4;
                    rgba[dst..dst + 4].copy_from_slice(&raster.data[src..src + 4]);
                }
            }
            state
                .monitor
                .preview_frames
                .insert(clip_id.clone(), crate::stores::MiniFrame { w: mw, h: mh, rgba });
            self.cache.insert(clip_id, CachedTitle { hash, w, h });
        }
    }
}
