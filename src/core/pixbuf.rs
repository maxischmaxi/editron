//! High-Bit-Depth-Frame-Repräsentation für den Export-Compositor: f32-RGBA,
//! display-referred (gamma-codiert) in 0..1 — dieselbe Semantik wie `u8 / 255`.
//!
//! ## Architekturentscheidung (Banding-Vermeidung ohne Look-Änderung)
//!
//! - **Compositing/Alpha-Blending bleibt im GAMMA-Raum** (display-referred);
//!   nur die Präzision steigt von 8 Bit auf f32. Das erhält die Optik der
//!   bestehenden Übergänge/Titel-Kanten und die Parität zur GPU-Vorschau
//!   exakt. Linear-Light-Blending würde den Look aller Bestands-Projekte
//!   ändern und gehört zum (bewusst ausgeklammerten) HDR-Grading — siehe
//!   `docs/ARCHITECTURE.md`.
//! - Die Farbkorrektur (`core/grade.rs`) konvertiert intern weiterhin für
//!   Weißabgleich/Belichtung nach linear und zurück (unverändert).
//! - 10-/12-Bit-Quellen werden als `rgba64le` (16 Bit/Kanal) dekodiert und
//!   verlustarm nach f32 gehoben; 8-Bit-Quellen bleiben `rgba` (u8) —
//!   der 8-Bit-Schnellpfad zahlt keine Bandbreite drauf.
//! - **Quantisierung am Pipeline-Ende:** für >8-Bit-Ziele f32→u16
//!   (`rgba64le`, der Encoder rechnet 16→10 Bit mit eigenem Dithering), für
//!   8-Bit-Ziele f32→u8 mit **TPDF-Dithering** (bricht Restbanding auf
//!   flachen Verläufen, ohne sichtbares Korn — deterministisch pro Pixel,
//!   also flimmerfrei über die Zeit).

/// Ein f32-RGBA-Frame: interleaved (R,G,B,A) je Pixel, Werte 0..1
/// (display-referred, gamma). `len() == w*h*4`. Aliast bewusst `Vec<f32>`,
/// damit der Compositor wie beim `Vec<u8>`-Pfad mit flachen Slices arbeitet.
pub type FloatBuf = Vec<f32>;

/// 8-Bit-RGBA → f32 (0..1). Exakt invers zu [`f32_to_rgba8`] (ohne Dither).
#[inline]
pub fn rgba8_to_f32(src: &[u8]) -> FloatBuf {
    src.iter().map(|&b| b as f32 * (1.0 / 255.0)).collect()
}

/// 8-Bit-RGBA → f32 (0..1) IN PLACE (kein Alloc pro Frame — Export-Hotloop).
#[inline]
pub fn rgba8_into_f32(src: &[u8], dst: &mut [f32]) {
    debug_assert_eq!(src.len(), dst.len());
    for (d, &b) in dst.iter_mut().zip(src) {
        *d = b as f32 * (1.0 / 255.0);
    }
}

/// 16-Bit-RGBA little-endian → f32 (0..1) IN PLACE (`dst.len() == src.len()/2`).
#[inline]
pub fn rgba64le_into_f32(src: &[u8], dst: &mut [f32]) {
    debug_assert_eq!(src.len(), dst.len() * 2);
    for (i, d) in dst.iter_mut().enumerate() {
        let c = &src[i * 2..i * 2 + 2];
        *d = u16::from_le_bytes([c[0], c[1]]) as f32 * (1.0 / 65535.0);
    }
}

/// f32 (0..1) → 8-Bit-RGBA, gerundet, ohne Dithering (Referenz/Tests).
#[inline]
pub fn f32_to_rgba8(src: &[f32]) -> Vec<u8> {
    src.iter()
        .map(|&v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
        .collect()
}

/// f32 (0..1) → 16-Bit-RGBA little-endian (`rgba64le`), gerundet.
/// 16 Bit reichen für jede Display-Quantisierung; der Encoder dithert beim
/// Schritt 16→10/12 Bit selbst, daher hier nur Runden.
#[inline]
pub fn f32_to_rgba64le(src: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len() * 2);
    for &v in src {
        let q = (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16;
        out.extend_from_slice(&q.to_le_bytes());
    }
    out
}

/// Deterministischer, flimmerfreier Dither-Wert in ~(−1, 1) LSB (TPDF =
/// Dreiecks-Wahrscheinlichkeitsdichte aus zwei dekorrelierten Rauschwerten).
/// Basis ist „Interleaved Gradient Noise“ (Jorge Jimenez) — blue-noise-artig,
/// rein aus den Pixelkoordinaten (kein Frame-Index ⇒ kein zeitliches Flimmern).
#[inline]
fn ign(x: u32, y: u32) -> f32 {
    let v = 0.06711056 * x as f32 + 0.00583715 * y as f32;
    (52.9829189 * (v - v.floor())).fract()
}

#[inline]
fn tpdf_dither(x: u32, y: u32) -> f32 {
    // Zwei dekorrelierte uniforme [0,1) → Dreieck in (−1, 1).
    ign(x, y) - ign(x.wrapping_add(113), y.wrapping_add(271))
}

/// f32 (0..1) → 8-Bit-RGBA mit TPDF-Dithering auf RGB (Alpha exakt gerundet).
/// Bricht das Banding flacher Verläufe beim 8-Bit-Quantisieren. `w`/`h`
/// liefern die Pixelkoordinaten für das deterministische Rauschen.
pub fn f32_to_rgba8_dithered(src: &[f32], w: usize, h: usize) -> Vec<u8> {
    debug_assert_eq!(src.len(), w * h * 4);
    let mut out = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            let d = tpdf_dither(x as u32, y as u32);
            for ch in 0..3 {
                let scaled = src[i + ch].clamp(0.0, 1.0) * 255.0;
                // Dither in der äußersten 1-LSB-Zone tapern, damit reines
                // Schwarz (0) und Weiß (255) exakt bleiben — dort gibt es
                // ohnehin kein Banding zu brechen.
                let head = scaled.min(255.0 - scaled).min(1.0);
                out[i + ch] = (scaled + 0.5 + d * head).clamp(0.0, 255.0) as u8;
            }
            out[i + 3] = (src[i + 3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        }
    }
    out
}

/// Bittiefe pro Kanal eines ffmpeg-`pix_fmt`-Namens (z. B. `yuv420p10le`,
/// `p010le`, `gbrp12le`, `rgb48`). 8, wenn keine höhere Tiefe erkennbar ist.
/// Heuristik über die im Namen kodierte Zahl bzw. bekannte 16-Bit-Formate.
pub fn pix_fmt_bit_depth(pix_fmt: &str) -> u32 {
    let f = pix_fmt.trim().to_ascii_lowercase();
    // Explizite Bit-Suffixe (yuv420p10le, gbrp12le, yuv444p16le …).
    for n in [16u32, 14, 12, 10, 9] {
        if f.contains(&format!("p{n}")) || f.contains(&format!("{n}le")) || f.contains(&format!("{n}be")) {
            return n;
        }
    }
    // p010/p012/p016 (semi-planar 4:2:0 high-bit), x2rgb10/rgb30 …
    if f.starts_with("p0") {
        if let Some(rest) = f.strip_prefix("p0") {
            if let Ok(n) = rest.trim_end_matches(|c: char| c.is_alphabetic()).parse::<u32>() {
                if n >= 8 {
                    return n;
                }
            }
        }
    }
    // 48-bit RGB / 64-bit RGBA = 16 Bit/Kanal.
    if f.starts_with("rgb48") || f.starts_with("bgr48") || f.starts_with("rgba64") || f.starts_with("bgra64") {
        return 16;
    }
    if f.contains("x2") && f.contains("10") {
        return 10;
    }
    8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgba8_roundtrips_exactly() {
        let src: Vec<u8> = (0..=255u8).flat_map(|b| [b, b, b, 255]).collect();
        let f = rgba8_to_f32(&src);
        let back = f32_to_rgba8(&f);
        assert_eq!(src, back, "u8 → f32 → u8 muss identisch sein");
    }

    #[test]
    fn rgba64le_roundtrips_near_exactly() {
        // Ein paar 16-Bit-Werte über den vollen Bereich.
        let vals: [u16; 4] = [0, 12345, 40000, 65535];
        let mut src = Vec::new();
        for v in vals {
            for _ in 0..4 {
                src.extend_from_slice(&v.to_le_bytes());
            }
        }
        let mut f = vec![0f32; src.len() / 2];
        rgba64le_into_f32(&src, &mut f);
        let back = f32_to_rgba64le(&f);
        assert_eq!(src, back, "u16 → f32 → u16 muss identisch sein");
    }

    #[test]
    fn dither_breaks_banding_on_flat_gradient() {
        // Sehr flacher Verlauf 0,500 → 0,502 über 256 px: ohne Dither rundet
        // alles auf denselben u8-Code (Banding); mit Dither variiert er.
        let (w, h) = (256usize, 1usize);
        let mut buf = vec![0f32; w * h * 4];
        for x in 0..w {
            let v = 0.500 + (x as f32 / w as f32) * 0.002;
            let i = x * 4;
            buf[i] = v;
            buf[i + 1] = v;
            buf[i + 2] = v;
            buf[i + 3] = 1.0;
        }
        let plain = f32_to_rgba8(&buf);
        let distinct_plain: std::collections::HashSet<u8> =
            plain.iter().step_by(4).copied().collect();
        assert_eq!(distinct_plain.len(), 1, "ohne Dither ⇒ Banding (ein Code)");

        let dithered = f32_to_rgba8_dithered(&buf, w, h);
        let distinct_dith: std::collections::HashSet<u8> =
            dithered.iter().step_by(4).copied().collect();
        assert!(
            distinct_dith.len() >= 2,
            "mit Dither ⇒ mehrere Codes (Banding gebrochen), waren {}",
            distinct_dith.len()
        );
    }

    #[test]
    fn dither_is_deterministic_and_unbiased() {
        // Deterministisch (kein Frame-Index): gleicher Input ⇒ gleicher Output.
        let (w, h) = (64usize, 64usize);
        let buf = vec![0.4980f32; w * h * 4]; // exakt zwischen 126 und 127
        let a = f32_to_rgba8_dithered(&buf, w, h);
        let b = f32_to_rgba8_dithered(&buf, w, h);
        assert_eq!(a, b, "Dither muss reproduzierbar sein");
        // Mittelwert über die Fläche bleibt nahe am wahren Wert (kein Bias).
        let mean: f64 = a.iter().step_by(4).map(|&v| v as f64).sum::<f64>() / (w * h) as f64;
        assert!((mean - 127.0).abs() < 0.6, "Dither verzerrt den Mittelwert nicht: {mean}");
    }

    #[test]
    fn high_value_does_not_overflow() {
        let buf = vec![1.0f32; 4];
        let out = f32_to_rgba8_dithered(&buf, 1, 1);
        assert_eq!(out, vec![255, 255, 255, 255], "Weiß bleibt 255 trotz Dither");
        let lo = f32_to_rgba8_dithered(&vec![0.0f32; 4], 1, 1);
        assert_eq!(lo, vec![0, 0, 0, 0]);
    }

    #[test]
    fn bit_depth_detection() {
        assert_eq!(pix_fmt_bit_depth("yuv420p"), 8);
        assert_eq!(pix_fmt_bit_depth("yuvj420p"), 8);
        assert_eq!(pix_fmt_bit_depth("yuv420p10le"), 10);
        assert_eq!(pix_fmt_bit_depth("yuv422p10le"), 10);
        assert_eq!(pix_fmt_bit_depth("yuv444p12le"), 12);
        assert_eq!(pix_fmt_bit_depth("p010le"), 10);
        assert_eq!(pix_fmt_bit_depth("p016le"), 16);
        assert_eq!(pix_fmt_bit_depth("gbrp10le"), 10);
        assert_eq!(pix_fmt_bit_depth("rgb48le"), 16);
        assert_eq!(pix_fmt_bit_depth("rgba64le"), 16);
        assert_eq!(pix_fmt_bit_depth("rgb24"), 8);
        assert_eq!(pix_fmt_bit_depth("nv12"), 8);
    }
}
