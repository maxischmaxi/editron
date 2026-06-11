//! Lazy-Texture-Cache: Panels fordern Bilder per Pfad an (`texture_requests`),
//! der Mainloop lädt sie zwischen den Frames (GPU-Upload braucht
//! `&mut RaylibHandle`, der während des Zeichnens geborgt ist).

use raylib::core::texture::{RaylibTexture2D, Texture2D};
use raylib::{RaylibHandle, RaylibThread};
use std::collections::HashMap;

#[derive(Default)]
pub struct TextureCache {
    /// None = Laden fehlgeschlagen (kein erneuter Versuch).
    map: HashMap<String, Option<Texture2D>>,
}

impl TextureCache {
    pub fn get(&self, path: &str) -> Option<&Texture2D> {
        self.map.get(path).and_then(|t| t.as_ref())
    }

    /// Dynamische Texture unter einem virtuellen Schlüssel ablegen
    /// (z. B. "player://program" für Live-Video-Frames).
    pub fn put(&mut self, key: &str, tex: Texture2D) {
        self.map.insert(key.to_string(), Some(tex));
    }

    pub fn remove(&mut self, key: &str) {
        self.map.remove(key);
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut Texture2D> {
        self.map.get_mut(key).and_then(|t| t.as_mut())
    }

    pub fn process_requests(
        &mut self,
        rl: &mut RaylibHandle,
        thread: &RaylibThread,
        requests: Vec<String>,
    ) {
        for path in requests {
            if self.map.contains_key(&path) {
                continue;
            }
            let tex = rl.load_texture(thread, &path).ok();
            if let Some(t) = &tex {
                t.set_texture_filter(
                    thread,
                    raylib::consts::TextureFilter::TEXTURE_FILTER_BILINEAR,
                );
            }
            self.map.insert(path, tex);
        }
    }
}
