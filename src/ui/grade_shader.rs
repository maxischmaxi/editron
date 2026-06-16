//! GPU-Pendant zu `core/grade.rs`: Fragment-Shader für die Echtzeit-
//! Farbkorrektur im Programmmonitor. Die Pipeline MUSS formelgleich mit
//! `grade_pixel` bleiben (Weißabgleich/Belichtung linear → Tonwerte →
//! Kontrast → Lift/Gamma/Gain → Tonwertkurven → Sättigung/Dynamik →
//! Vignette), damit Vorschau und CPU-Export identisch aussehen.

use crate::core::grade::{GradeParams, LUT_N};
use raylib::core::shaders::Shader;
use raylib::ffi;
use raylib::prelude::RaylibShader;
use raylib::{RaylibHandle, RaylibThread};

/// Skalar-Uniforms eines LUT-Slots (Mode/Größe/Domain/Stärke). Die Stützwerte
/// liegen separat in einer GPU-Textur, die beim Zeichnen gebunden wird.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct LutUniform {
    /// 0 = aus, 1 = 1D, 3 = 3D.
    pub mode: f32,
    pub size: f32,
    pub dmin: [f32; 3],
    pub dmax: [f32; 3],
    pub strength: f32,
}

impl LutUniform {
    pub const OFF: LutUniform = LutUniform {
        mode: 0.0,
        size: 0.0,
        dmin: [0.0; 3],
        dmax: [1.0; 3],
        strength: 0.0,
    };

    pub fn is_active(&self) -> bool {
        self.mode > 0.5 && self.strength > 0.0
    }
}

// Die GLSL-LUT-Arrays sind fest auf 256 dimensioniert (ein Eintrag je
// 8-Bit-Code). Hält den Shader mit der CPU-LUT-Auflösung in Sync.
const _: () = assert!(LUT_N == 256, "FRAGMENT_SRC lutR/lutG/lutB[256] anpassen");

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
uniform float curvesOn;  // >0,5 = Tonwertkurven anwenden
uniform float lutR[256]; // kombinierte Kurven-LUT je Kanal (Master ∘ Kanal),
uniform float lutG[256]; // formelgleich zu core/grade.rs sample_lut
uniform float lutB[256];
uniform float ditherAmt; // 1 = Output-Dithering an, 0 = aus (Paritätstest)

// 3D-LUTs (.cube) — Input-Slot am Pipeline-ANFANG, Look-Slot am -ENDE.
// Die Stützwerte liegen als RGBA32F-Textur (gepackt wie Lut::pack_rgba_f32),
// die hier manuell trilinear (3D) bzw. linear je Kanal (1D) abgetastet wird —
// formelgleich zu core/lut.rs Lut::sample.
uniform sampler2D inputLut;
uniform float inputLutMode; // 0 = aus, 1 = 1D, 3 = 3D
uniform float inputLutSize; // N (Stützstellen je Achse)
uniform vec3 inputLutMin;
uniform vec3 inputLutMax;
uniform float inputLutStrength;
uniform sampler2D lookLut;
uniform float lookLutMode;
uniform float lookLutSize;
uniform vec3 lookLutMin;
uniform vec3 lookLutMax;
uniform float lookLutStrength;

out vec4 finalColor;

float lumaOf(vec3 c) { return dot(c, vec3(0.2126, 0.7152, 0.0722)); }

// Ein 3D-Gitterpunkt: Texel (b·N + r, g), Blau-Scheiben nebeneinander.
vec3 texLut3(sampler2D tex, float n, float r, float g, float b) {
    return texelFetch(tex, ivec2(int(b * n + r), int(g)), 0).rgb;
}

// Trilineare 3D-Abtastung (formelgleich Lut::sample, ThreeD-Zweig).
vec3 lut3d(sampler2D tex, float n, vec3 c, vec3 dmin, vec3 dmax) {
    vec3 t = clamp((c - dmin) / max(dmax - dmin, vec3(1e-12)), 0.0, 1.0);
    float n1 = n - 1.0;
    vec3 f = t * n1;
    vec3 i0 = floor(f);
    vec3 i1 = min(i0 + 1.0, vec3(n1));
    vec3 d = f - i0;
    vec3 c000 = texLut3(tex, n, i0.r, i0.g, i0.b);
    vec3 c100 = texLut3(tex, n, i1.r, i0.g, i0.b);
    vec3 c010 = texLut3(tex, n, i0.r, i1.g, i0.b);
    vec3 c110 = texLut3(tex, n, i1.r, i1.g, i0.b);
    vec3 c001 = texLut3(tex, n, i0.r, i0.g, i1.b);
    vec3 c101 = texLut3(tex, n, i1.r, i0.g, i1.b);
    vec3 c011 = texLut3(tex, n, i0.r, i1.g, i1.b);
    vec3 c111 = texLut3(tex, n, i1.r, i1.g, i1.b);
    vec3 c00 = mix(c000, c100, d.r);
    vec3 c10 = mix(c010, c110, d.r);
    vec3 c01 = mix(c001, c101, d.r);
    vec3 c11 = mix(c011, c111, d.r);
    return mix(mix(c00, c10, d.g), mix(c01, c11, d.g), d.b);
}

// Lineare 1D-Abtastung je Kanal (formelgleich Lut::sample, OneD-Zweig).
vec3 lut1d(sampler2D tex, float n, vec3 c, vec3 dmin, vec3 dmax) {
    vec3 t = clamp((c - dmin) / max(dmax - dmin, vec3(1e-12)), 0.0, 1.0);
    float n1 = n - 1.0;
    vec3 f = t * n1;
    vec3 i0 = floor(f);
    vec3 i1 = min(i0 + 1.0, vec3(n1));
    vec3 d = f - i0;
    vec3 a = vec3(
        texelFetch(tex, ivec2(int(i0.r), 0), 0).r,
        texelFetch(tex, ivec2(int(i0.g), 0), 0).g,
        texelFetch(tex, ivec2(int(i0.b), 0), 0).b);
    vec3 bb = vec3(
        texelFetch(tex, ivec2(int(i1.r), 0), 0).r,
        texelFetch(tex, ivec2(int(i1.g), 0), 0).g,
        texelFetch(tex, ivec2(int(i1.b), 0), 0).b);
    return mix(a, bb, d);
}

// Einen LUT-Slot anwenden (mit Stärke zumischen). mode < 0,5 ⇒ Durchlass,
// der Sampler wird dann NIE gelesen (sicher, auch wenn nicht gebunden).
vec3 applyLut(vec3 c, sampler2D tex, float mode, float n, vec3 dmin, vec3 dmax, float strength) {
    if (mode < 0.5 || strength <= 0.0) return c;
    vec3 s = (mode > 2.5) ? lut3d(tex, n, c, dmin, dmax) : lut1d(tex, n, c, dmin, dmax);
    return mix(c, s, strength);
}

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
    // 0) Input-LUT (technische Normalisierung) — ganz am Anfang, vor dem Grade.
    c = applyLut(c, inputLut, inputLutMode, inputLutSize, inputLutMin, inputLutMax, inputLutStrength);
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
    // 4.5) Tonwertkurven: lineare LUT-Interpolation (a + (b-a)*frac) je Kanal,
    // formelgleich zu core::grade::sample_lut (GL_LINEAR + CLAMP_TO_EDGE).
    if (curvesOn > 0.5) {
        float f, fi; int idx;
        f = clamp(c.r, 0.0, 1.0) * 255.0; fi = floor(f); idx = int(fi);
        c.r = lutR[idx] + (lutR[min(idx + 1, 255)] - lutR[idx]) * (f - fi);
        f = clamp(c.g, 0.0, 1.0) * 255.0; fi = floor(f); idx = int(fi);
        c.g = lutG[idx] + (lutG[min(idx + 1, 255)] - lutG[idx]) * (f - fi);
        f = clamp(c.b, 0.0, 1.0) * 255.0; fi = floor(f); idx = int(fi);
        c.b = lutB[idx] + (lutB[min(idx + 1, 255)] - lutB[idx]) * (f - fi);
    }
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
    // 7) Look-LUT (kreativer Schluss-Stempel) — ganz am Ende, nach Vignette.
    c = applyLut(c, lookLut, lookLutMode, lookLutSize, lookLutMin, lookLutMax, lookLutStrength);
    c = clamp(c, 0.0, 1.0);
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
    loc_curves_on: i32,
    loc_lut_r: i32,
    loc_lut_g: i32,
    loc_lut_b: i32,
    loc_dither: i32,
    // 3D-LUT-Slots (Input/Look): Sampler + Skalar-Uniforms.
    loc_input_lut: i32,
    loc_input_mode: i32,
    loc_input_size: i32,
    loc_input_min: i32,
    loc_input_max: i32,
    loc_input_strength: i32,
    loc_look_lut: i32,
    loc_look_mode: i32,
    loc_look_size: i32,
    loc_look_min: i32,
    loc_look_max: i32,
    loc_look_strength: i32,
    last: Option<GradeParams>,
    last_luts: Option<(LutUniform, LutUniform)>,
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
            loc_curves_on: loc("curvesOn"),
            loc_lut_r: loc("lutR"),
            loc_lut_g: loc("lutG"),
            loc_lut_b: loc("lutB"),
            loc_dither: loc("ditherAmt"),
            loc_input_lut: loc("inputLut"),
            loc_input_mode: loc("inputLutMode"),
            loc_input_size: loc("inputLutSize"),
            loc_input_min: loc("inputLutMin"),
            loc_input_max: loc("inputLutMax"),
            loc_input_strength: loc("inputLutStrength"),
            loc_look_lut: loc("lookLut"),
            loc_look_mode: loc("lookLutMode"),
            loc_look_size: loc("lookLutSize"),
            loc_look_min: loc("lookLutMin"),
            loc_look_max: loc("lookLutMax"),
            loc_look_strength: loc("lookLutStrength"),
            shader,
            last: None,
            last_luts: None,
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
        self.shader
            .set_shader_value(self.loc_curves_on, if p.curves_on { 1.0f32 } else { 0.0f32 });
        // LUT-Arrays nur bei aktiven Kurven hochladen (3×256 floats); sonst
        // bleiben die Stale-Werte stehen, der Shader überspringt sie ohnehin.
        if p.curves_on {
            self.shader.set_shader_value_v(self.loc_lut_r, &p.curve_lut[0][..]);
            self.shader.set_shader_value_v(self.loc_lut_g, &p.curve_lut[1][..]);
            self.shader.set_shader_value_v(self.loc_lut_b, &p.curve_lut[2][..]);
        }
        self.last = Some(*p);
    }

    /// Skalar-Uniforms der beiden LUT-Slots setzen (vor `begin_shader_mode`).
    /// Die Stützwert-Texturen werden separat in [`Self::bind_lut_textures`]
    /// gebunden (muss innerhalb des Shader-Modus passieren).
    pub fn apply_luts(&mut self, input: LutUniform, look: LutUniform) {
        if self.last_luts == Some((input, look)) {
            return;
        }
        self.shader.set_shader_value(self.loc_input_mode, input.mode);
        self.shader.set_shader_value(self.loc_input_size, input.size);
        self.shader.set_shader_value(self.loc_input_min, input.dmin);
        self.shader.set_shader_value(self.loc_input_max, input.dmax);
        self.shader.set_shader_value(self.loc_input_strength, input.strength);
        self.shader.set_shader_value(self.loc_look_mode, look.mode);
        self.shader.set_shader_value(self.loc_look_size, look.size);
        self.shader.set_shader_value(self.loc_look_min, look.dmin);
        self.shader.set_shader_value(self.loc_look_max, look.dmax);
        self.shader.set_shader_value(self.loc_look_strength, look.strength);
        self.last_luts = Some((input, look));
    }

    /// Roh-Shader-Handle + die Sampler-Locations der beiden LUT-Slots. Der
    /// Aufrufer bindet die LUT-Texturen INNERHALB von `begin_shader_mode`
    /// direkt per `ffi::SetShaderValueTexture` (raylib bindet Zusatztexturen
    /// erst beim Zeichnen — Muster aus `fx_shader`), ohne `self` erneut zu
    /// borgen, während `shader` mutabel im Shader-Modus liegt.
    pub fn raw_and_lut_locs(&self) -> (ffi::Shader, i32, i32) {
        (*self.shader.as_ref(), self.loc_input_lut, self.loc_look_lut)
    }
}
