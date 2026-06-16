//! 3D- (und 1D-) LUTs im `.cube`-Format (Adobe/Resolve/IRIDAS) für Industrie-
//! Looks und Kamera-Konvertierungen. Ein Clip kann zwei LUT-Slots tragen
//! ([`crate::core::grade::ColorGrade::input_lut`]/`look_lut`):
//!
//! * **Input-LUT** — technische Normalisierung (Log→Rec709, Kamera→Display),
//!   wird GANZ AM ANFANG der Grading-Pipeline angewandt (vor Weißabgleich),
//!   speist also das gesamte Grading mit normalisiertem Material.
//! * **Look-LUT** — kreativer Schluss-Stempel, wird GANZ AM ENDE angewandt
//!   (nach Lift/Gamma/Gain, Kurven, Sättigung und Vignette).
//!
//! Die Pixel-Mathematik existiert ZWEIMAL formelgleich: trilineare Abtastung
//! im CPU-Pfad hier ([`Lut::sample`]) und im GPU-Fragment-Shader
//! (`ui/grade_shader.rs`, manuelle `texelFetch`-Trilinearität auf der
//! gepackten Float-Textur aus [`Lut::pack_rgba_f32`]). Beide lesen DIESELBEN
//! f32-Stützwerte (GPU als RGBA32F-Textur) ⇒ Vorschau und Export sind
//! pixelgleich, wie bei den Tonwertkurven.
//!
//! Der LUT-Pfad (plus Stärke) wird im Projekt referenziert; die Datei selbst
//! liegt extern. Fehlt sie beim Laden, greift das Offline-Muster der Medien
//! ([`LutCache`] liefert einen Fehler, das Farbe-Panel zeigt einen Hinweis +
//! „Datei suchen…“).

use std::collections::HashMap;
use std::sync::Arc;

/// Obergrenzen, damit eine kaputte/absurde Datei keine GB allokiert. Die
/// 3D-Grenze beschränkt zugleich die GPU-Texturbreite (N·N Spalten) auf
/// 96·96 = 9216 < üblicher `GL_MAX_TEXTURE_SIZE` (16384). Gängige Größen
/// (17/25/33/64/65) liegen weit darunter.
const MAX_3D_SIZE: usize = 96;
/// 1D-LUTs werden als Textur der Breite N (Höhe 1) hochgeladen.
const MAX_1D_SIZE: usize = 8192;

/// Dimensionalität einer LUT.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LutDim {
    /// Per-Kanal-Kennlinie (`LUT_1D_SIZE`): N Stützwerte, je Kanal unabhängig.
    OneD,
    /// Volumetrische LUT (`LUT_3D_SIZE`): N³ Stützwerte, trilinear abgetastet.
    ThreeD,
}

/// Eine geparste `.cube`-LUT. `data` hält die Stützwerte als RGB-Tripel:
/// * 1D: `size` Einträge, Index = Stützstelle.
/// * 3D: `size³` Einträge in `.cube`-Reihenfolge (Rot ändert sich am
///   schnellsten): `data[r + size*(g + size*b)]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Lut {
    pub dim: LutDim,
    pub size: usize,
    /// Eingangs-Definitionsbereich je Kanal (Standard 0…1).
    pub domain_min: [f32; 3],
    pub domain_max: [f32; 3],
    pub data: Vec<[f32; 3]>,
    pub title: String,
}

impl Lut {
    /// Index eines 3D-Gitterpunkts (Rot am schnellsten).
    #[inline]
    fn idx3(&self, r: usize, g: usize, b: usize) -> usize {
        r + self.size * (g + self.size * b)
    }

    /// Normiert einen Kanalwert in den Stützstellen-Float-Index `0…size-1`
    /// (Definitionsbereich + Klemmung an den Rändern, wie GL_CLAMP_TO_EDGE).
    #[inline]
    fn axis_pos(&self, ch: usize, v: f32) -> f32 {
        let span = self.domain_max[ch] - self.domain_min[ch];
        let t = if span.abs() > 1e-12 {
            (v - self.domain_min[ch]) / span
        } else {
            0.0
        };
        t.clamp(0.0, 1.0) * (self.size as f32 - 1.0)
    }

    /// LUT an einem RGB-Wert abtasten (3D = trilinear, 1D = linear je Kanal).
    /// MUSS formelgleich mit dem GPU-Shader (`applyLut`) bleiben. Das Ergebnis
    /// wird NICHT geklemmt (HDR-LUTs dürfen > 1 liefern; die finale Klemmung
    /// passiert am Pipeline-Ende, exakt wie auf der GPU).
    pub fn sample(&self, rgb: [f32; 3]) -> [f32; 3] {
        match self.dim {
            LutDim::OneD => {
                let mut out = [0.0f32; 3];
                for ch in 0..3 {
                    let f = self.axis_pos(ch, rgb[ch]);
                    let i0 = f.floor() as usize;
                    let i1 = (i0 + 1).min(self.size - 1);
                    let fr = f - i0 as f32;
                    out[ch] = self.data[i0][ch] * (1.0 - fr) + self.data[i1][ch] * fr;
                }
                out
            }
            LutDim::ThreeD => {
                let fr = self.axis_pos(0, rgb[0]);
                let fg = self.axis_pos(1, rgb[1]);
                let fb = self.axis_pos(2, rgb[2]);
                let (r0, g0, b0) = (fr.floor() as usize, fg.floor() as usize, fb.floor() as usize);
                let n1 = self.size - 1;
                let (r1, g1, b1) = ((r0 + 1).min(n1), (g0 + 1).min(n1), (b0 + 1).min(n1));
                let (dr, dg, db) = (fr - r0 as f32, fg - g0 as f32, fb - b0 as f32);
                let mut out = [0.0f32; 3];
                for ch in 0..3 {
                    let c000 = self.data[self.idx3(r0, g0, b0)][ch];
                    let c100 = self.data[self.idx3(r1, g0, b0)][ch];
                    let c010 = self.data[self.idx3(r0, g1, b0)][ch];
                    let c110 = self.data[self.idx3(r1, g1, b0)][ch];
                    let c001 = self.data[self.idx3(r0, g0, b1)][ch];
                    let c101 = self.data[self.idx3(r1, g0, b1)][ch];
                    let c011 = self.data[self.idx3(r0, g1, b1)][ch];
                    let c111 = self.data[self.idx3(r1, g1, b1)][ch];
                    // Trilinear: zuerst in r, dann g, dann b interpolieren.
                    let c00 = c000 * (1.0 - dr) + c100 * dr;
                    let c10 = c010 * (1.0 - dr) + c110 * dr;
                    let c01 = c001 * (1.0 - dr) + c101 * dr;
                    let c11 = c011 * (1.0 - dr) + c111 * dr;
                    let c0 = c00 * (1.0 - dg) + c10 * dg;
                    let c1 = c01 * (1.0 - dg) + c11 * dg;
                    out[ch] = c0 * (1.0 - db) + c1 * db;
                }
                out
            }
        }
    }

    /// LUT mit Stärke `0…1` zumischen: `mix(rgb, sample(rgb), strength)`.
    #[inline]
    pub fn apply(&self, rgb: [f32; 3], strength: f32) -> [f32; 3] {
        if strength <= 0.0 {
            return rgb;
        }
        let s = self.sample(rgb);
        [
            rgb[0] + (s[0] - rgb[0]) * strength,
            rgb[1] + (s[1] - rgb[1]) * strength,
            rgb[2] + (s[2] - rgb[2]) * strength,
        ]
    }

    /// In eine GPU-taugliche RGBA32F-Textur packen (Alpha = 1). Liefert die
    /// Float-Daten plus `(width, height)`:
    /// * 1D: `width = size`, `height = 1`.
    /// * 3D: `width = size·size`, `height = size`; Texel `(b·size + r, g)`
    ///   hält das Gitter `data[idx3(r, g, b)]` (Blau-Scheiben nebeneinander).
    /// Der Shader liest diese Texel per `texelFetch` und interpoliert manuell
    /// mit DERSELBEN Trilinear-Formel wie [`Lut::sample`].
    pub fn pack_rgba_f32(&self) -> (Vec<f32>, i32, i32) {
        let n = self.size;
        let (w, h) = match self.dim {
            LutDim::OneD => (n, 1),
            LutDim::ThreeD => (n * n, n),
        };
        let mut buf = vec![0.0f32; w * h * 4];
        let mut put = |x: usize, y: usize, c: [f32; 3]| {
            let o = (y * w + x) * 4;
            buf[o] = c[0];
            buf[o + 1] = c[1];
            buf[o + 2] = c[2];
            buf[o + 3] = 1.0;
        };
        match self.dim {
            LutDim::OneD => {
                for i in 0..n {
                    put(i, 0, self.data[i]);
                }
            }
            LutDim::ThreeD => {
                for b in 0..n {
                    for g in 0..n {
                        for r in 0..n {
                            put(b * n + r, g, self.data[self.idx3(r, g, b)]);
                        }
                    }
                }
            }
        }
        (buf, w as i32, h as i32)
    }
}

/// `.cube`-Text parsen (1D oder 3D). Erkennt `TITLE`, `LUT_1D_SIZE`,
/// `LUT_3D_SIZE`, `DOMAIN_MIN`/`DOMAIN_MAX` (sowie die Alt-Schlüssel
/// `LUT_*D_INPUT_RANGE`), Kommentare (`#`) und leere Zeilen. Datenzeilen sind
/// drei Floats. Liefert eine deutsche Fehlermeldung bei ungültiger Struktur.
pub fn parse_cube(text: &str) -> Result<Lut, String> {
    let mut dim: Option<LutDim> = None;
    let mut size: usize = 0;
    let mut domain_min = [0.0f32; 3];
    let mut domain_max = [1.0f32; 3];
    let mut title = String::new();
    let mut data: Vec<[f32; 3]> = Vec::new();

    for (lineno, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let key = parts.next().unwrap_or("");
        let upper = key.to_ascii_uppercase();
        match upper.as_str() {
            "TITLE" => {
                // Rest der Zeile, Anführungszeichen entfernen.
                title = line[key.len()..].trim().trim_matches('"').to_string();
            }
            "LUT_1D_SIZE" => {
                let n: usize = parts
                    .next()
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| format!("Zeile {}: LUT_1D_SIZE ohne Zahl", lineno + 1))?;
                if !(2..=MAX_1D_SIZE).contains(&n) {
                    return Err(format!("LUT_1D_SIZE {n} außerhalb 2…{MAX_1D_SIZE}"));
                }
                dim = Some(LutDim::OneD);
                size = n;
                data.reserve(n);
            }
            "LUT_3D_SIZE" => {
                let n: usize = parts
                    .next()
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| format!("Zeile {}: LUT_3D_SIZE ohne Zahl", lineno + 1))?;
                if !(2..=MAX_3D_SIZE).contains(&n) {
                    return Err(format!("LUT_3D_SIZE {n} außerhalb 2…{MAX_3D_SIZE}"));
                }
                dim = Some(LutDim::ThreeD);
                size = n;
                data.reserve(n * n * n);
            }
            "DOMAIN_MIN" => {
                domain_min = parse_triplet(&mut parts)
                    .ok_or_else(|| format!("Zeile {}: DOMAIN_MIN ungültig", lineno + 1))?;
            }
            "DOMAIN_MAX" => {
                domain_max = parse_triplet(&mut parts)
                    .ok_or_else(|| format!("Zeile {}: DOMAIN_MAX ungültig", lineno + 1))?;
            }
            "LUT_1D_INPUT_RANGE" | "LUT_3D_INPUT_RANGE" => {
                // Alt-Schlüssel: zwei Skalare (min max) für alle Kanäle.
                let lo: f32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
                let hi: f32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1.0);
                domain_min = [lo; 3];
                domain_max = [hi; 3];
            }
            _ => {
                // Datenzeile: drei Floats (key ist bereits der erste Wert).
                let r: f32 = key
                    .parse()
                    .map_err(|_| format!("Zeile {}: unbekannter Schlüssel/ungültiger Wert „{key}“", lineno + 1))?;
                let g: f32 = parts
                    .next()
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| format!("Zeile {}: Datenzeile braucht 3 Werte", lineno + 1))?;
                let b: f32 = parts
                    .next()
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| format!("Zeile {}: Datenzeile braucht 3 Werte", lineno + 1))?;
                data.push([r, g, b]);
            }
        }
    }

    let dim = dim.ok_or_else(|| "Keine LUT_1D_SIZE/LUT_3D_SIZE-Angabe".to_string())?;
    let expected = match dim {
        LutDim::OneD => size,
        LutDim::ThreeD => size * size * size,
    };
    if data.len() != expected {
        return Err(format!(
            "Erwartet {expected} Stützwerte, gefunden {}",
            data.len()
        ));
    }
    Ok(Lut {
        dim,
        size,
        domain_min,
        domain_max,
        data,
        title,
    })
}

fn parse_triplet<'a>(parts: &mut impl Iterator<Item = &'a str>) -> Option<[f32; 3]> {
    let a = parts.next()?.parse().ok()?;
    let b = parts.next()?.parse().ok()?;
    let c = parts.next()?.parse().ok()?;
    Some([a, b, c])
}

/// `.cube`-Datei laden und parsen.
pub fn load_cube_file(path: &str) -> Result<Lut, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{e}"))?;
    parse_cube(&text)
}

// ------------------------------------------------------- Anwendung pro Slot

/// Eine aufgelöste, anzuwendende LUT mit Stärke (`0…1`). Borgt die geteilten
/// LUT-Daten (aus [`LutCache`] bzw. der Export-Bibliothek).
#[derive(Clone, Copy)]
pub struct LutApply<'a> {
    pub lut: &'a Lut,
    pub strength: f32,
}

/// Input-/Look-LUT eines Layers als Borrow-Paar — wird durch [`grade_buffer`]
/// und die Per-Pixel-Funktion gereicht. Leer ⇒ kein LUT-Einfluss.
///
/// [`grade_buffer`]: crate::core::grade::grade_buffer
#[derive(Clone, Copy, Default)]
pub struct LutStack<'a> {
    pub input: Option<LutApply<'a>>,
    pub look: Option<LutApply<'a>>,
}

impl LutStack<'_> {
    /// Leerer Stapel (kein LUT) — als `&LutStack::EMPTY` an Grade-Aufrufer.
    pub const EMPTY: LutStack<'static> = LutStack {
        input: None,
        look: None,
    };

    /// Wirkt mindestens ein Slot (vorhanden und Stärke > 0)?
    #[inline]
    pub fn is_active(&self) -> bool {
        self.input.is_some_and(|a| a.strength > 0.0) || self.look.is_some_and(|a| a.strength > 0.0)
    }
}

/// Besitzende Variante von [`LutStack`]: hält `Arc<Lut>` + Stärke je Slot.
/// Lebt an einem Layer (Export/Monitor/Scopes); [`Self::borrow`] erzeugt den
/// kurzlebigen Borrow-Stapel für [`grade_buffer`]/[`grade_lut_pixel`].
///
/// [`grade_buffer`]: crate::core::grade::grade_buffer
/// [`grade_lut_pixel`]: crate::core::grade::grade_lut_pixel
#[derive(Clone, Default)]
pub struct OwnedLutStack {
    pub input: Option<(Arc<Lut>, f32)>,
    pub look: Option<(Arc<Lut>, f32)>,
}

impl OwnedLutStack {
    /// Borrow-Stapel für die Per-Pixel-Pipeline.
    pub fn borrow(&self) -> LutStack<'_> {
        LutStack {
            input: self.input.as_ref().map(|(l, s)| LutApply {
                lut: l.as_ref(),
                strength: *s,
            }),
            look: self.look.as_ref().map(|(l, s)| LutApply {
                lut: l.as_ref(),
                strength: *s,
            }),
        }
    }

    /// Wirkt mindestens ein Slot?
    pub fn is_active(&self) -> bool {
        self.input.as_ref().is_some_and(|(_, s)| *s > 0.0)
            || self.look.as_ref().is_some_and(|(_, s)| *s > 0.0)
    }
}

// ---------------------------------------------------------------- CPU-Cache

/// Pfad-indizierter Cache geparster LUTs für den Hauptthread (Player-Vorschau,
/// Scopes, Farbe-Panel). Ein Eintrag ist `Ok(Lut)` oder `Err(Meldung)` —
/// fehlende/ungültige Dateien werden als Fehler gemerkt (Offline-Muster) und
/// nicht bei jedem Frame erneut versucht. Geteilt via [`Arc`], damit das
/// Abtasten ohne Klonen läuft.
#[derive(Default)]
pub struct LutCache {
    map: HashMap<String, Result<Arc<Lut>, String>>,
}

impl LutCache {
    /// Eintrag für `path` (lädt+parst beim ersten Zugriff). Erneutes Laden
    /// einer geänderten Datei erfolgt nicht automatisch — siehe [`Self::reload`].
    pub fn get_or_load(&mut self, path: &str) -> &Result<Arc<Lut>, String> {
        if !self.map.contains_key(path) {
            let entry = load_cube_file(path).map(Arc::new);
            self.map.insert(path.to_string(), entry);
        }
        self.map.get(path).expect("eben eingefügt")
    }

    /// Einen Slot (Pfad + Stärke 0…1) zu `(Arc<Lut>, Stärke)` auflösen; lädt
    /// bei Bedarf. None ⇒ leerer Pfad, Stärke 0 oder Datei offline/ungültig.
    pub fn resolve(&mut self, path: &str, strength01: f32) -> Option<(Arc<Lut>, f32)> {
        if path.is_empty() || strength01 <= 0.0 {
            return None;
        }
        self.get_or_load(path)
            .as_ref()
            .ok()
            .map(|l| (l.clone(), strength01))
    }

    /// Cache-Eintrag verwerfen (nach Relink/Datei-Wechsel), erzwingt Neuladen.
    pub fn invalidate(&mut self, path: &str) {
        self.map.remove(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: [f32; 3], b: [f32; 3], eps: f32) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() <= eps)
    }

    /// Identitäts-3D-LUT der Größe `n` erzeugen (Gitterpunkt = seine Position).
    fn identity_3d_cube(n: usize) -> String {
        let mut s = format!("LUT_3D_SIZE {n}\n");
        for b in 0..n {
            for g in 0..n {
                for r in 0..n {
                    let f = |i: usize| i as f32 / (n as f32 - 1.0);
                    s.push_str(&format!("{} {} {}\n", f(r), f(g), f(b)));
                }
            }
        }
        s
    }

    #[test]
    fn parses_3d_cube_and_reports_dim_size() {
        let lut = parse_cube(&identity_3d_cube(2)).expect("parse");
        assert_eq!(lut.dim, LutDim::ThreeD);
        assert_eq!(lut.size, 2);
        assert_eq!(lut.data.len(), 8);
        assert_eq!(lut.domain_min, [0.0; 3]);
        assert_eq!(lut.domain_max, [1.0; 3]);
    }

    #[test]
    fn identity_3d_lut_is_passthrough() {
        for n in [2usize, 17, 33] {
            let lut = parse_cube(&identity_3d_cube(n)).expect("parse");
            for &c in &[
                [0.0, 0.0, 0.0],
                [1.0, 1.0, 1.0],
                [0.25, 0.5, 0.75],
                [0.13, 0.87, 0.42],
                [0.999, 0.001, 0.5],
            ] {
                let out = lut.sample(c);
                assert!(approx(out, c, 2e-3), "n={n} {c:?} -> {out:?}");
            }
        }
    }

    #[test]
    fn identity_1d_lut_is_passthrough() {
        let n = 64usize;
        let mut s = format!("LUT_1D_SIZE {n}\n");
        for i in 0..n {
            let v = i as f32 / (n as f32 - 1.0);
            s.push_str(&format!("{v} {v} {v}\n"));
        }
        let lut = parse_cube(&s).expect("parse");
        assert_eq!(lut.dim, LutDim::OneD);
        for &c in &[[0.0; 3], [0.3, 0.6, 0.9], [1.0; 3]] {
            assert!(approx(lut.sample(c), c, 1e-3), "{c:?}");
        }
    }

    #[test]
    fn known_3d_lut_hits_reference_invert() {
        // Invertierende LUT: out = 1 - in an jedem Gitterpunkt. Trilinear
        // reproduziert die affine Funktion exakt (auch zwischen den Stützen).
        let n = 2usize;
        let mut s = format!("LUT_3D_SIZE {n}\n");
        for b in 0..n {
            for g in 0..n {
                for r in 0..n {
                    let f = |i: usize| i as f32 / (n as f32 - 1.0);
                    s.push_str(&format!("{} {} {}\n", 1.0 - f(r), 1.0 - f(g), 1.0 - f(b)));
                }
            }
        }
        let lut = parse_cube(&s).expect("parse");
        for &c in &[[0.2, 0.4, 0.6], [0.0, 0.5, 1.0], [0.73, 0.11, 0.95]] {
            let out = lut.sample(c);
            let expect = [1.0 - c[0], 1.0 - c[1], 1.0 - c[2]];
            assert!(approx(out, expect, 1e-5), "{c:?} -> {out:?} vs {expect:?}");
        }
    }

    #[test]
    fn known_3d_lut_scales_channels() {
        // out = (0.5·r, 1·g, 0·b) — affin, also trilinear exakt.
        let n = 2usize;
        let mut s = format!("LUT_3D_SIZE {n}\n");
        for b in 0..n {
            for g in 0..n {
                for r in 0..n {
                    let f = |i: usize| i as f32 / (n as f32 - 1.0);
                    s.push_str(&format!("{} {} {}\n", 0.5 * f(r), f(g), 0.0 * f(b)));
                }
            }
        }
        let lut = parse_cube(&s).expect("parse");
        let out = lut.sample([0.8, 0.6, 0.9]);
        assert!(approx(out, [0.4, 0.6, 0.0], 1e-5), "{out:?}");
    }

    #[test]
    fn strength_blends_towards_identity() {
        let lut = parse_cube(&{
            // Konstant-Schwarz-LUT.
            let n = 2usize;
            let mut s = format!("LUT_3D_SIZE {n}\n");
            for _ in 0..8 {
                s.push_str("0 0 0\n");
            }
            s
        })
        .expect("parse");
        let c = [0.6, 0.6, 0.6];
        assert!(approx(lut.apply(c, 0.0), c, 1e-6)); // Stärke 0 = unverändert
        assert!(approx(lut.apply(c, 1.0), [0.0; 3], 1e-6)); // Stärke 1 = voll
        assert!(approx(lut.apply(c, 0.5), [0.3; 3], 1e-6)); // halb
    }

    #[test]
    fn domain_remaps_input_range() {
        // Identität auf dem Bereich 0…2: Eingang 1,0 liegt mittig ⇒ 0,5-Gitter.
        let n = 3usize;
        let mut s = format!("LUT_3D_SIZE {n}\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 2 2 2\n");
        for b in 0..n {
            for g in 0..n {
                for r in 0..n {
                    let f = |i: usize| i as f32 / (n as f32 - 1.0); // 0, 0.5, 1
                    s.push_str(&format!("{} {} {}\n", f(r), f(g), f(b)));
                }
            }
        }
        let lut = parse_cube(&s).expect("parse");
        // Eingang 1,0 / Domain-Max 2 ⇒ t = 0,5 ⇒ mittlerer Gitterpunkt (0,5).
        let out = lut.sample([1.0, 1.0, 1.0]);
        assert!(approx(out, [0.5; 3], 1e-4), "{out:?}");
    }

    #[test]
    fn packed_texture_dimensions_and_layout() {
        let lut = parse_cube(&identity_3d_cube(4)).expect("parse");
        let (buf, w, h) = lut.pack_rgba_f32();
        assert_eq!((w, h), (16, 4)); // N·N x N
        assert_eq!(buf.len() as i32, w * h * 4);
        // Texel (b·N+r, g) hält data[idx3(r,g,b)].
        let n = 4usize;
        let (r, g, b) = (1usize, 2usize, 3usize);
        let x = b * n + r;
        let o = (g * (n * n) + x) * 4;
        let expect = lut.data[lut.idx3(r, g, b)];
        assert!(approx([buf[o], buf[o + 1], buf[o + 2]], expect, 0.0));
    }

    #[test]
    fn rejects_bad_files() {
        assert!(parse_cube("0 0 0\n0 0 0\n").is_err()); // keine Size-Angabe
        assert!(parse_cube("LUT_3D_SIZE 2\n0 0 0\n").is_err()); // zu wenige Daten
        assert!(parse_cube("LUT_3D_SIZE 1\n").is_err()); // Size < 2
        assert!(parse_cube("LUT_3D_SIZE 999\n").is_err()); // Size zu groß
    }

    #[test]
    fn skips_comments_and_title() {
        let src = "# Kommentar\nTITLE \"Mein Look\"\nLUT_3D_SIZE 2\n\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n";
        let lut = parse_cube(src).expect("parse");
        assert_eq!(lut.title, "Mein Look");
        assert_eq!(lut.size, 2);
    }

    #[test]
    fn cache_reports_offline_for_missing_file() {
        let mut cache = LutCache::default();
        let res = cache.get_or_load("/nonexistent/does-not-exist.cube");
        assert!(res.is_err(), "fehlende Datei ⇒ Offline-Fehler");
        // Auflösen einer fehlenden Datei ⇒ None (Offline ⇒ kein LUT-Einfluss).
        assert!(cache.resolve("/nonexistent/does-not-exist.cube", 1.0).is_none());
    }
}
