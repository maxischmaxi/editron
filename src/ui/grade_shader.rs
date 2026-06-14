//! GPU-Pendant zu `core/grade.rs`: Fragment-Shader für die Echtzeit-
//! Farbkorrektur im Programmmonitor. Die Pipeline MUSS formelgleich mit
//! `grade_pixel` bleiben (Weißabgleich/Belichtung linear → Tonwerte →
//! Kontrast → Lift/Gamma/Gain → Sättigung/Dynamik → Vignette), damit
//! Vorschau und CPU-Export identisch aussehen.

use crate::core::grade::GradeParams;
use raylib::core::shaders::Shader;
use raylib::prelude::RaylibShader;
use raylib::{RaylibHandle, RaylibThread};

/// Fragment-Shader (GLSL 330, Desktop): erwartet raylibs Default-Vertex-
/// Stage (fragTexCoord/fragColor/texture0/colDiffuse).
const FRAGMENT_SRC: &str = r#"
#version 330
in vec2 fragTexCoord;
in vec4 fragColor;
uniform sampler2D texture0;
uniform vec4 colDiffuse;

uniform vec3 wbGain;
uniform vec4 tonal;     // Schwarz, Schatten, Lichter, Weiß
uniform float slope;
uniform vec3 lift;
uniform vec3 invGamma;
uniform vec3 gain;
uniform vec2 satVib;    // Sättigung, Dynamik
uniform vec4 vignette;  // Stärke (signiert), Mittelpunkt, Weichkante, Rundheit
uniform float ditherAmt; // 1 = Output-Dithering an, 0 = aus (Paritätstest)

out vec4 finalColor;

float lumaOf(vec3 c) { return dot(c, vec3(0.2126, 0.7152, 0.0722)); }

// Interleaved Gradient Noise (blue-noise-artig, rein aus den Pixelkoordinaten —
// flimmerfrei) → TPDF-Dither, formelgleich zu core/pixbuf.rs. Bricht das
// 8-Bit-Banding eines Grades direkt beim Schreiben in den Framebuffer.
float ign(vec2 p) { return fract(52.9829189 * fract(0.06711056 * p.x + 0.00583715 * p.y)); }
vec3 tpdfDither(vec2 frag) {
    float d = ign(frag) - ign(frag + vec2(113.0, 271.0)); // ~(-1, 1)
    return vec3(d / 255.0);
}

void main() {
    vec4 tex = texture(texture0, fragTexCoord);
    vec3 c = tex.rgb;
    // 1) Weißabgleich + Belichtung in linearem Licht (γ 2,2).
    vec3 lin = pow(max(c, 0.0), vec3(2.2)) * wbGain;
    c = pow(max(lin, 0.0), vec3(1.0 / 2.2));
    // 2) Tonwerte: luma-gewichtete Offsets.
    float l = lumaOf(c);
    float tn = tonal.x * (1.0 - smoothstep(0.0, 0.25, l))
             + tonal.y * (1.0 - smoothstep(0.0, 0.65, l))
             + tonal.z * smoothstep(0.35, 1.0, l)
             + tonal.w * smoothstep(0.75, 1.0, l);
    // 3) Kontrast um 0,5; 4) Lift/Gamma/Gain.
    vec3 g = clamp((c + tn - 0.5) * slope + 0.5, 0.0, 1.0);
    g = clamp(g * gain + lift * (1.0 - g), 0.0, 1.0);
    c = pow(g, invGamma);
    // 5) Sättigung/Dynamik (luma-erhaltend).
    l = lumaOf(c);
    float satNow = clamp(max(c.r, max(c.g, c.b)) - min(c.r, min(c.g, c.b)), 0.0, 1.0);
    float sat = max(satVib.x * (1.0 + satVib.y * (1.0 - smoothstep(0.0, 0.5, satNow))), 0.0);
    c = vec3(l) + (c - vec3(l)) * sat;
    // 6) Vignette über die Clip-UVs.
    if (vignette.x != 0.0) {
        vec2 p = (fragTexCoord - 0.5) * 2.0;
        float circ = length(p) * 0.70710678;
        float rectd = max(abs(p.x), abs(p.y));
        float shape = (vignette.w + 1.0) * 0.5;
        float d = rectd + (circ - rectd) * shape;
        float f = smoothstep(vignette.y, min(vignette.y + max(vignette.z, 0.01), 1.5), d);
        if (vignette.x > 0.0) { c *= 1.0 - vignette.x * f; }
        else { c += (1.0 - c) * (-vignette.x) * f; }
    }
    // Output-Dithering (getapert an den Extremen wie der CPU-Pfad): bricht das
    // Banding gegradeter Verläufe auf dem 8-Bit-Display.
    vec3 head = clamp(min(c, 1.0 - c) * 255.0, 0.0, 1.0);
    c += tpdfDither(gl_FragCoord.xy) * head * ditherAmt;
    finalColor = vec4(clamp(c, 0.0, 1.0), tex.a) * colDiffuse * fragColor;
}
"#;

/// Geladener Grade-Shader + aufgelöste Uniform-Locations. Uniforms werden
/// nur bei Wertänderung neu gesetzt (ein Set pro Layer und Frame ist ok,
/// aber identische Parameter — z. B. Standbild — sparen die GL-Calls).
pub struct GradeShader {
    pub shader: Shader,
    loc_wb_gain: i32,
    loc_tonal: i32,
    loc_slope: i32,
    loc_lift: i32,
    loc_inv_gamma: i32,
    loc_gain: i32,
    loc_sat_vib: i32,
    loc_vignette: i32,
    loc_dither: i32,
    last: Option<GradeParams>,
}

impl GradeShader {
    /// Lädt den Shader; None, wenn die Kompilierung fehlschlägt (die
    /// Vorschau fällt dann auf ungegradete Darstellung zurück).
    pub fn load(rl: &mut RaylibHandle, thread: &RaylibThread) -> Option<GradeShader> {
        let shader = rl.load_shader_from_memory(thread, None, Some(FRAGMENT_SRC));
        if !shader.is_shader_valid() {
            eprintln!("[grade] Farbkorrektur-Shader nicht ladbar — Vorschau ohne Grading");
            return None;
        }
        let loc = |name: &str| shader.get_shader_location(name);
        let mut gs = GradeShader {
            loc_wb_gain: loc("wbGain"),
            loc_tonal: loc("tonal"),
            loc_slope: loc("slope"),
            loc_lift: loc("lift"),
            loc_inv_gamma: loc("invGamma"),
            loc_gain: loc("gain"),
            loc_sat_vib: loc("satVib"),
            loc_vignette: loc("vignette"),
            loc_dither: loc("ditherAmt"),
            shader,
            last: None,
        };
        // Vorschau-Dithering standardmäßig an (EDITRON_GRADE_DITHER=0 schaltet
        // es für Vergleichs-/Debug-Zwecke aus).
        let on = std::env::var("EDITRON_GRADE_DITHER").map(|v| v != "0").unwrap_or(true);
        gs.set_dither(on);
        Some(gs)
    }

    /// Output-Dithering an-/ausschalten (aus: für den GPU↔CPU-Paritätstest,
    /// damit gegen den ungeditherten f32-CPU-Pfad verglichen werden kann).
    pub fn set_dither(&mut self, on: bool) {
        self.shader
            .set_shader_value(self.loc_dither, if on { 1.0f32 } else { 0.0f32 });
    }

    /// Uniforms für die Parameter setzen (vor `begin_shader_mode`).
    pub fn apply(&mut self, p: &GradeParams) {
        if self.last.as_ref() == Some(p) {
            return;
        }
        self.shader.set_shader_value(self.loc_wb_gain, p.wb_gain);
        self.shader.set_shader_value(
            self.loc_tonal,
            [p.blacks, p.shadows, p.highlights, p.whites],
        );
        self.shader.set_shader_value(self.loc_slope, p.slope);
        self.shader.set_shader_value(self.loc_lift, p.lift);
        self.shader.set_shader_value(self.loc_inv_gamma, p.inv_gamma);
        self.shader.set_shader_value(self.loc_gain, p.gain);
        self.shader
            .set_shader_value(self.loc_sat_vib, [p.saturation, p.vibrance]);
        self.shader.set_shader_value(self.loc_vignette, p.vignette);
        self.last = Some(*p);
    }
}
