//! GPU-Seite der 3D-LUTs: ein pfad-indizierter Cache hochgeladener
//! RGBA32F-Texturen (gepackt wie [`crate::core::lut::Lut::pack_rgba_f32`]).
//! Die Texturen leben über Frames hinweg im Mainloop (analog zum
//! `GradeShader`) und werden zwischen den Frames befüllt; der Programmmonitor
//! bindet sie beim Zeichnen an den Grade-Shader (`bind_lut_textures`).
//!
//! Die CPU-Seite (geparste [`Lut`]-Daten) liegt getrennt im
//! [`crate::core::lut::LutCache`] (Scopes/Export). Beide referenzieren die
//! Datei über denselben Pfad-Schlüssel.

use crate::core::lut::{Lut, LutDim};
use raylib::core::texture::Texture2D;
use raylib::ffi;
use raylib::{RaylibHandle, RaylibThread};
use std::collections::HashMap;

/// Eine hochgeladene LUT-Textur plus die Skalar-Parameter für den Shader.
pub struct LutTexture {
    pub tex: Texture2D,
    /// 1 = 1D, 3 = 3D (Shader-`mode`).
    pub mode: f32,
    pub size: f32,
    pub dmin: [f32; 3],
    pub dmax: [f32; 3],
}

/// Pfad → hochgeladene Textur. `None` = Upload gescheitert/offline (kein
/// erneuter Versuch, bis der Eintrag invalidiert wird).
#[derive(Default)]
pub struct LutGpuCache {
    map: HashMap<String, Option<LutTexture>>,
}

impl LutGpuCache {
    /// Wurde `path` schon (erfolgreich oder gescheitert) verarbeitet?
    pub fn contains(&self, path: &str) -> bool {
        self.map.contains_key(path)
    }

    /// Hochgeladene Textur zu einem Pfad (None = nicht vorhanden/gescheitert).
    pub fn get(&self, path: &str) -> Option<&LutTexture> {
        self.map.get(path).and_then(|o| o.as_ref())
    }

    /// Pfad als gescheitert markieren (z. B. Offline; verhindert Neuversuch).
    pub fn mark_failed(&mut self, path: &str) {
        self.map.entry(path.to_string()).or_insert(None);
    }

    /// Eintrag verwerfen (nach Relink/Datei-Wechsel) ⇒ erneuter Upload.
    pub fn invalidate(&mut self, path: &str) {
        self.map.remove(path);
    }

    /// LUT als RGBA32F-2D-Textur hochladen (idempotent je Pfad). Muss zwischen
    /// den Frames laufen (braucht `&mut RaylibHandle`).
    pub fn upload(&mut self, _rl: &mut RaylibHandle, thread: &RaylibThread, path: &str, lut: &Lut) {
        if self.map.contains_key(path) {
            return;
        }
        let entry = build_lut_texture(thread, lut);
        self.map.insert(path.to_string(), entry);
    }
}

/// Eine geparste LUT in eine GPU-Float-Textur überführen. `LoadTextureFromImage`
/// KOPIERT die Pixel in den GPU-Speicher, daher genügt es, dass `data` während
/// des Aufrufs lebt. Punktfilter — die Trilinearität rechnet der Shader manuell
/// per `texelFetch` (formelgleich zur CPU).
fn build_lut_texture(thread: &RaylibThread, lut: &Lut) -> Option<LutTexture> {
    use raylib::core::texture::RaylibTexture2D;
    let (data, w, h) = lut.pack_rgba_f32();
    let img = ffi::Image {
        data: data.as_ptr() as *mut std::os::raw::c_void,
        width: w,
        height: h,
        mipmaps: 1,
        format: ffi::PixelFormat::PIXELFORMAT_UNCOMPRESSED_R32G32B32A32 as i32,
    };
    let raw = unsafe { ffi::LoadTextureFromImage(img) };
    if raw.id == 0 {
        eprintln!("[lut] RGBA32F-Textur-Upload fehlgeschlagen (Float-Texturen nicht unterstützt?)");
        return None;
    }
    let tex = unsafe { Texture2D::from_raw(raw) };
    tex.set_texture_filter(thread, raylib::consts::TextureFilter::TEXTURE_FILTER_POINT);
    let mode = match lut.dim {
        LutDim::OneD => 1.0,
        LutDim::ThreeD => 3.0,
    };
    Some(LutTexture {
        tex,
        mode,
        size: lut.size as f32,
        dmin: lut.domain_min,
        dmax: lut.domain_max,
    })
}
