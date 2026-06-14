//! Font-Verwaltung: löst den Font-Stack über fontconfig auf
//! (Inter → Noto Sans, JetBrains Mono → …) und lädt jede benötigte
//! (Schnitt, Größe)-Kombination als eigenen Atlas. Atlanten werden mit 2-facher
//! Pixelgröße gerastert und bilinear herunterskaliert (sauberes Antialiasing
//! mit Subpixel-Positionierung).
//!
//! HiDPI: Der Atlas wird in PHYSIKALISCHER Auflösung gerastert — Pixelgröße =
//! `logische_größe × ui_scale × OVERSAMPLE`. `measure()` liefert weiterhin
//! logische Maße (Layout bleibt skalierungsunabhängig), gezeichnet wird mit
//! `render_size = logische_größe × ui_scale` (physikalisch). Beim Scale-Wechsel
//! (Monitorwechsel/Override) baut der Mainloop die Atlanten neu auf.

use crate::core::text_raster::fc_match;
use raylib::consts::TextureFilter;
use raylib::core::text::Font;
use raylib::core::RaylibHandle;
use raylib::ffi;
use raylib::math::Vector2;
use raylib::RaylibThread;

/// Codepoints, die in die Atlanten gerastert werden: ASCII, Latin-1
/// (Umlaute!), typografische Zeichen (– — „ “ … ›), Pfeile.
fn codepoints() -> Vec<char> {
    let mut cps: Vec<char> = (0x20u32..=0x7e).filter_map(char::from_u32).collect();
    cps.extend((0xa0u32..=0xff).filter_map(char::from_u32));
    cps.extend(
        [
            0x2013, 0x2014, 0x2018, 0x2019, 0x201a, 0x201c, 0x201d, 0x201e, 0x2022, 0x2026,
            0x2039, 0x203a, 0x2192, 0x2190, 0x2191, 0x2193, 0x2715,
        ]
        .iter()
        .filter_map(|&c| char::from_u32(c)),
    );
    cps
}

/// Sucht entlang einer Familien-Präferenzliste den ersten echten Treffer
/// (fc-match liefert sonst stillschweigend einen Fallback). Die fc-match-
/// Anbindung teilt sich die UI mit dem Titel-Rasterizer
/// (`core::text_raster`).
fn resolve_font(families: &[&str], fc_weight: u32) -> Option<String> {
    for fam in families {
        if let Some((matched, file, _)) = fc_match(&format!("{fam}:weight={fc_weight}")) {
            if matched.to_lowercase().contains(&fam.to_lowercase()) {
                return Some(file);
            }
        }
    }
    // Letzte Rettung: irgendein Sans der Plattform
    fc_match(&format!("sans-serif:weight={fc_weight}")).map(|(_, f, _)| f)
}

const SANS_STACK: &[&str] = &["Inter", "Segoe UI", "Noto Sans", "DejaVu Sans"];
const MONO_STACK: &[&str] = &[
    "JetBrains Mono",
    "Cascadia Mono",
    "Noto Sans Mono",
    "DejaVu Sans Mono",
];

// fontconfig-Gewichte: 80 = Regular, 100 = Medium, 180 = DemiBold, 200 = Bold
const FC_REGULAR: u32 = 80;
const FC_MEDIUM: u32 = 100;
const FC_SEMIBOLD: u32 = 180;
const FC_BOLD: u32 = 200;

/// Supersampling-Faktor der Atlanten.
const OVERSAMPLE: f32 = 2.0;

pub struct FontHandle {
    font: Font,
    /// Logische Pixelgröße (CSS px) — Bezug für `measure()` und das Layout.
    pub size: f32,
    /// Physikalische Pixelgröße für `draw_text_ex` (= `size × ui_scale`).
    pub render_size: f32,
    /// Zeilenhöhe (CSS line-height der Tailwind-Stufe, logisch).
    pub line_height: f32,
}

impl FontHandle {
    fn load(
        rl: &mut RaylibHandle,
        thread: &RaylibThread,
        path: &str,
        size: f32,
        line_height: f32,
        scale: f32,
        chars: &[char],
    ) -> FontHandle {
        let chars_string: String = chars.iter().collect();
        // Atlas in physikalischer Auflösung rastern: logische Größe × UI-Scale ×
        // Supersampling. So bleiben Glyphen auf HiDPI-Displays gestochen scharf.
        let base = ((size * scale * OVERSAMPLE).round() as i32).max(1);
        let font = rl
            .load_font_ex(thread, path, base, Some(&chars_string))
            .unwrap_or_else(|e| panic!("Font {path} ({size}px) konnte nicht geladen werden: {e}"));
        unsafe {
            ffi::SetTextureFilter(
                font.texture,
                TextureFilter::TEXTURE_FILTER_BILINEAR as i32,
            );
        }
        FontHandle {
            font,
            size,
            render_size: size * scale,
            line_height,
        }
    }

    pub fn raw(&self) -> &Font {
        &self.font
    }

    pub fn measure(&self, text: &str) -> Vector2 {
        use raylib::core::text::RaylibFont;
        self.font.measure_text(text, self.size, 0.0)
    }

    pub fn width(&self, text: &str) -> f32 {
        self.measure(text).x
    }

    /// Kürzt `text` mit "…" auf `max_w` (CSS text-overflow: ellipsis).
    pub fn ellipsize(&self, text: &str, max_w: f32) -> String {
        if self.width(text) <= max_w {
            return text.to_string();
        }
        let ell = "…";
        let ell_w = self.width(ell);
        let mut out = String::new();
        let mut w = 0.0;
        for c in text.chars() {
            let cw = self.width(&c.to_string());
            if w + cw + ell_w > max_w {
                break;
            }
            w += cw;
            out.push(c);
        }
        out.push_str(ell);
        out
    }
}

/// Alle in der App verwendeten (Schnitt, Größe)-Kombinationen.
pub struct Fonts {
    pub sans_12: FontHandle,        // text-xs
    pub sans_12_medium: FontHandle, // text-xs font-medium (aktiver Workspace-Tab)
    pub sans_12_bold: FontHandle,   // Logo-„E“
    pub sans_14: FontHandle,        // text-sm
    pub sans_14_semibold: FontHandle, // „Editron“-Titel
    pub sans_16: FontHandle,        // text-base (Dialoge)
    pub sans_16_semibold: FontHandle,
    pub mono_12: FontHandle, // font-mono text-xs (Timecodes)
    pub mono_11: FontHandle, // Timeline-Lineal
}

impl Fonts {
    /// Lädt alle Atlanten in physikalischer Auflösung für den gegebenen
    /// UI-Scale. Bei einem Scale-Wechsel (Monitorwechsel/Override) ruft der
    /// Mainloop dies erneut auf und ersetzt den `Fonts`-Wert.
    pub fn load(rl: &mut RaylibHandle, thread: &RaylibThread, scale: f32) -> Fonts {
        let chars = codepoints();
        let sans_reg = resolve_font(SANS_STACK, FC_REGULAR).expect("kein Sans-Font gefunden");
        let sans_med = resolve_font(SANS_STACK, FC_MEDIUM).unwrap_or_else(|| sans_reg.clone());
        let sans_semi = resolve_font(SANS_STACK, FC_SEMIBOLD).unwrap_or_else(|| sans_med.clone());
        let sans_bold = resolve_font(SANS_STACK, FC_BOLD).unwrap_or_else(|| sans_semi.clone());
        let mono_reg = resolve_font(MONO_STACK, FC_REGULAR).unwrap_or_else(|| sans_reg.clone());

        Fonts {
            sans_12: FontHandle::load(rl, thread, &sans_reg, 12.0, 16.0, scale, &chars),
            sans_12_medium: FontHandle::load(rl, thread, &sans_med, 12.0, 16.0, scale, &chars),
            sans_12_bold: FontHandle::load(rl, thread, &sans_bold, 12.0, 16.0, scale, &chars),
            sans_14: FontHandle::load(rl, thread, &sans_reg, 14.0, 20.0, scale, &chars),
            sans_14_semibold: FontHandle::load(rl, thread, &sans_semi, 14.0, 20.0, scale, &chars),
            sans_16: FontHandle::load(rl, thread, &sans_reg, 16.0, 24.0, scale, &chars),
            sans_16_semibold: FontHandle::load(rl, thread, &sans_semi, 16.0, 24.0, scale, &chars),
            mono_12: FontHandle::load(rl, thread, &mono_reg, 12.0, 16.0, scale, &chars),
            mono_11: FontHandle::load(rl, thread, &mono_reg, 11.0, 14.0, scale, &chars),
        }
    }
}
