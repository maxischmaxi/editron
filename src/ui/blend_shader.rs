//! GPU-Blend-Compositor: Ebenen-Mischmodi (Multiply, Screen, Overlay, …) für
//! den Programmmonitor. Formelgleich zum CPU-Pfad (`compose::blend_composite`):
//! `result = B(src, dst) · α + dst · (1 − α)`.
//!
//! Da die Mischformel das Ziel (bereits komponierte Layer) benötigt, kann sie
//! nicht über raylibs Fixed-Function-Blending laufen — stattdessen wird jeder
//! Layer in eine eigene RenderTexture gezeichnet und per Shader auf die
//! Compositing-Textur aufgemischt (Ping-Pong zwischen zwei RTs).
//!
//! Für Normal-Modus wird der bestehende Direkt-zu-Bildschirm-Pfad genutzt
//! (kein Overhead); dieser Compositor greift nur, sobald mindestens ein Layer
//! einen nicht-Normalen Mischmodus trägt.

use crate::core::compose::BlendMode;
use crate::core::grade::GradeParams;
use crate::ui::grade_shader::{GradeShader, LutUniform};
use crate::ui::lut_gpu::{LutGpuCache, LutTexture};
use raylib::core::shaders::Shader;
use raylib::core::texture::RenderTexture2D;
use raylib::ffi;
use raylib::math::{Rectangle, Vector2};
use raylib::prelude::{
    RaylibBlendModeExt, RaylibDraw, RaylibRenderTexture2D, RaylibShader, RaylibShaderModeExt,
    RaylibTextureModeExt,
};
use raylib::{RaylibHandle, RaylibThread};

const HEADER: &str = "#version 330\nin vec2 fragTexCoord;\nin vec4 fragColor;\nuniform sampler2D texture0;\nuniform vec4 colDiffuse;\nout vec4 finalColor;\n";

const BLEND_SRC: &str = r#"
uniform sampler2D texDst;
uniform int uBlendMode;

float blendChannel(float s, float d, int mode) {
    if (mode == 0) return s;
    if (mode == 1) return s * d;
    if (mode == 2) return s + d - s * d;
    if (mode == 3) {
        if (d <= 0.5) return 2.0 * s * d;
        return 1.0 - 2.0 * (1.0 - s) * (1.0 - d);
    }
    if (mode == 4) return min(1.0, s + d);
    if (mode == 5) return min(s, d);
    if (mode == 6) return max(s, d);
    if (mode == 7) {
        if (s <= 0.5) return d - (1.0 - 2.0 * s) * d * (1.0 - d);
        float D = d <= 0.25 ? ((16.0 * d - 12.0) * d + 4.0) * d : sqrt(d);
        return d + (2.0 * s - 1.0) * (D - d);
    }
    if (mode == 8) {
        if (s <= 0.5) return 2.0 * s * d;
        return 1.0 - 2.0 * (1.0 - s) * (1.0 - d);
    }
    if (mode == 9) return abs(d - s);
    if (mode == 10) return s + d - 2.0 * s * d;
    return s;
}

void main() {
    vec4 src = texture(texture0, fragTexCoord);
    vec4 dst = texture(texDst, fragTexCoord);
    float a = src.a;
    vec3 result;
    for (int c = 0; c < 3; c++) {
        float b = blendChannel(src[c], dst[c], uBlendMode);
        result[c] = b * a + dst[c] * (1.0 - a);
    }
    finalColor = vec4(clamp(result, 0.0, 1.0), 1.0) * colDiffuse * fragColor;
}
"#;

struct BlendShader {
    shader: Shader,
    loc_dst: i32,
    loc_mode: i32,
}

impl BlendShader {
    fn load(rl: &mut RaylibHandle, thread: &RaylibThread) -> Option<BlendShader> {
        let src = format!("{HEADER}{BLEND_SRC}");
        let shader = rl.load_shader_from_memory(thread, None, Some(&src));
        if !shader.is_shader_valid() {
            eprintln!("[blend] Blend-Shader nicht ladbar — Mischmodi in Vorschau deaktiviert");
            return None;
        }
        Some(BlendShader {
            loc_dst: shader.get_shader_location("texDst"),
            loc_mode: shader.get_shader_location("uBlendMode"),
            shader,
        })
    }
}

struct RawTex(ffi::Texture2D);
impl AsRef<ffi::Texture2D> for RawTex {
    fn as_ref(&self) -> &ffi::Texture2D {
        &self.0
    }
}

pub struct BlendLayerRequest {
    pub tex_key: String,
    pub cx: f32,
    pub cy: f32,
    pub w: f32,
    pub h: f32,
    pub rot_deg: f32,
    pub alpha: u8,
    pub blend_mode: BlendMode,
    /// Farbkorrektur des Layers (vorberechnet) — wird beim Zeichnen in den
    /// `src`-Puffer formelgleich zum Export angewandt, damit ein gegradeter
    /// Clip mit Mischmodus in der Vorschau nicht seine Farbe verliert.
    pub grade: GradeParams,
    /// Input-/Look-3D-LUT (Pfad + Stärke 0…1), falls aktiv.
    pub input_lut: Option<(String, f32)>,
    pub look_lut: Option<(String, f32)>,
}

pub struct BlendCompositingRequest {
    pub canvas_w: u32,
    pub canvas_h: u32,
    pub layers: Vec<BlendLayerRequest>,
}

pub fn blend_output_key() -> &'static str {
    "blend://program"
}

pub struct BlendCompositor {
    shader: Option<BlendShader>,
    comp_a: Option<RenderTexture2D>,
    comp_b: Option<RenderTexture2D>,
    src: Option<RenderTexture2D>,
    w: u32,
    h: u32,
    has_output: bool,
    result_in_a: bool,
}

impl BlendCompositor {
    pub fn new() -> BlendCompositor {
        BlendCompositor {
            shader: None,
            comp_a: None,
            comp_b: None,
            src: None,
            w: 0,
            h: 0,
            has_output: false,
            result_in_a: true,
        }
    }

    pub fn load(&mut self, rl: &mut RaylibHandle, thread: &RaylibThread) {
        self.shader = BlendShader::load(rl, thread);
    }

    pub fn output_texture(&self) -> Option<ffi::Texture2D> {
        if !self.has_output {
            return None;
        }
        let rt = if self.result_in_a {
            &self.comp_a
        } else {
            &self.comp_b
        };
        rt.as_ref().map(|rt| *rt.texture().as_ref())
    }

    pub fn needs_compositing(layers: &[BlendLayerRequest]) -> bool {
        layers.iter().any(|l| l.blend_mode != BlendMode::Normal)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn process(
        &mut self,
        rl: &mut RaylibHandle,
        thread: &RaylibThread,
        textures: &crate::ui::textures::TextureCache,
        fx_renderer: Option<&crate::ui::fx_shader::EffectChainRenderer>,
        grade_shader: Option<&mut GradeShader>,
        lut_cache: Option<&LutGpuCache>,
        req: BlendCompositingRequest,
    ) {
        if self.shader.is_none() {
            self.has_output = false;
            return;
        }
        if !Self::needs_compositing(&req.layers) {
            self.has_output = false;
            return;
        }
        let (w, h) = (req.canvas_w.max(1), req.canvas_h.max(1));
        if w != self.w || h != self.h || self.comp_a.is_none() {
            self.comp_a = rl.load_render_texture(thread, w, h).ok();
            self.comp_b = rl.load_render_texture(thread, w, h).ok();
            self.src = rl.load_render_texture(thread, w, h).ok();
            self.w = w;
            self.h = h;
            for rt in [&self.comp_a, &self.comp_b, &self.src]
                .into_iter()
                .flatten()
            {
                unsafe {
                    ffi::SetTextureFilter(
                        *rt.texture().as_ref(),
                        ffi::TextureFilter::TEXTURE_FILTER_BILINEAR as i32,
                    );
                }
            }
        }

        let mut shader = self.shader.take().unwrap();
        let comp_a = self.comp_a.as_mut().unwrap();
        let comp_b = self.comp_b.as_mut().unwrap();
        let src = self.src.as_mut().unwrap();

        {
            let mut d = rl.begin_texture_mode(thread, comp_a);
            d.clear_background(raylib::color::Color::new(0, 0, 0, 255));
        }

        let mut grade_shader = grade_shader;
        let mut result_in_a = true;
        for layer in &req.layers {
            {
                let mut d = rl.begin_texture_mode(thread, src);
                d.clear_background(raylib::color::Color::BLANK);
            }
            draw_layer_to_src(
                rl,
                thread,
                textures,
                fx_renderer,
                lut_cache,
                grade_shader.as_deref_mut(),
                src,
                layer,
            );
            if result_in_a {
                composite_layer(rl, thread, &mut shader, src, comp_a, comp_b, layer.blend_mode);
            } else {
                composite_layer(rl, thread, &mut shader, src, comp_b, comp_a, layer.blend_mode);
            }
            result_in_a = !result_in_a;
        }

        self.shader = Some(shader);
        self.has_output = true;
        self.result_in_a = result_in_a;
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_layer_to_src(
    rl: &mut RaylibHandle,
    thread: &RaylibThread,
    textures: &crate::ui::textures::TextureCache,
    fx_renderer: Option<&crate::ui::fx_shader::EffectChainRenderer>,
    lut_cache: Option<&LutGpuCache>,
    grade_shader: Option<&mut GradeShader>,
    src: &mut RenderTexture2D,
    layer: &BlendLayerRequest,
) {
    let (tex, src_rect) = if layer.tex_key.starts_with("fx://") {
        let Some(fx) = fx_renderer else { return };
        let Some(out) = fx.output(&layer.tex_key) else { return };
        let (tw, th) = (out.tex.width as f32, out.tex.height as f32);
        let src_h = if out.flipped { -th } else { th };
        (out.tex, Rectangle::new(0.0, 0.0, tw, src_h))
    } else {
        let Some(t) = textures.get(&layer.tex_key) else { return };
        let r = Rectangle::new(0.0, 0.0, t.width as f32, t.height as f32);
        (*t.as_ref(), r)
    };

    // LUT-Uniforms auflösen (identisch zu `Ui::draw_texture_quad_graded`).
    let input = layer
        .input_lut
        .as_ref()
        .and_then(|(p, s)| lut_cache.and_then(|c| c.get(p)).map(|lt| (lt, *s)));
    let look = layer
        .look_lut
        .as_ref()
        .and_then(|(p, s)| lut_cache.and_then(|c| c.get(p)).map(|lt| (lt, *s)));
    let uni = |slot: Option<(&LutTexture, f32)>| match slot {
        Some((lt, s)) => LutUniform {
            mode: lt.mode,
            size: lt.size,
            dmin: lt.dmin,
            dmax: lt.dmax,
            strength: s,
        },
        None => LutUniform::OFF,
    };
    let input_uni = uni(input);
    let look_uni = uni(look);
    let any_lut = input_uni.is_active() || look_uni.is_active();
    // Grade nur anwenden, wenn nötig (sonst klassischer Direkt-Kopierpfad).
    let use_grade =
        grade_shader.is_some() && (!layer.grade.is_identity() || any_lut);

    let mut d = rl.begin_texture_mode(thread, src);
    // Direkter Kopier-Blend (src = ONE, dst = ZERO): überträgt RGBA samt der
    // mit `tint.a` skalierten Layer-Deckkraft, ohne Vormultiplikation.
    unsafe {
        ffi::rlSetBlendFactors(1, 0, 0x8006);
    }
    let mut bm = d.begin_blend_mode(raylib::consts::BlendMode::BLEND_CUSTOM);
    let tint = raylib::color::Color::new(255, 255, 255, layer.alpha);
    // raylib-Idiom (wie `Ui::draw_texture_quad_graded`): Ziel an (cx,cy), der
    // Origin (w/2,h/2) verschiebt die obere linke Ecke auf (cx−w/2, cy−h/2)
    // und ist zugleich das Rotationszentrum.
    let dst = Rectangle::new(layer.cx, layer.cy, layer.w, layer.h);
    let origin = Vector2::new(layer.w / 2.0, layer.h / 2.0);
    if use_grade {
        let gs = grade_shader.unwrap();
        gs.apply(&layer.grade);
        gs.apply_luts(input_uni, look_uni);
        let (raw_shader, loc_in, loc_look) = gs.raw_and_lut_locs();
        let in_tex = input
            .filter(|_| input_uni.is_active())
            .map(|(lt, _)| *lt.tex.as_ref());
        let look_tex = look
            .filter(|_| look_uni.is_active())
            .map(|(lt, _)| *lt.tex.as_ref());
        let mut sm = bm.begin_shader_mode(&mut gs.shader);
        unsafe {
            if let Some(t) = in_tex {
                if loc_in >= 0 {
                    ffi::SetShaderValueTexture(raw_shader, loc_in, t);
                }
            }
            if let Some(t) = look_tex {
                if loc_look >= 0 {
                    ffi::SetShaderValueTexture(raw_shader, loc_look, t);
                }
            }
        }
        sm.draw_texture_pro(RawTex(tex), src_rect, dst, origin, layer.rot_deg, tint);
    } else {
        bm.draw_texture_pro(RawTex(tex), src_rect, dst, origin, layer.rot_deg, tint);
    }
}

#[allow(clippy::too_many_arguments)]
fn composite_layer(
    rl: &mut RaylibHandle,
    thread: &RaylibThread,
    shader: &mut BlendShader,
    src: &mut RenderTexture2D,
    dst: &mut RenderTexture2D,
    out: &mut RenderTexture2D,
    blend_mode: BlendMode,
) {
    let raw_shader: ffi::Shader = *shader.shader.as_ref();
    let src_tex: ffi::Texture2D = *src.texture().as_ref();
    let dst_tex: ffi::Texture2D = *dst.texture().as_ref();
    let (w, h) = (out.texture().width as f32, out.texture().height as f32);

    let mut d = rl.begin_texture_mode(thread, out);
    d.clear_background(raylib::color::Color::BLANK);
    unsafe {
        ffi::rlSetBlendFactors(1, 0, 0x8006);
    }
    let mut bm = d.begin_blend_mode(raylib::consts::BlendMode::BLEND_CUSTOM);
    shader.shader.set_shader_value(shader.loc_mode, blend_mode.shader_code());
    let mut sm = bm.begin_shader_mode(&mut shader.shader);
    // Die Ziel-Textur (`texDst`) MUSS innerhalb des aktiven Shader-Modus
    // gebunden werden — raylib bindet Zusatz-Sampler erst beim Zeichnen
    // (Muster aus `fx_shader`/`draw_texture_quad_graded`). Vor
    // `begin_shader_mode` gesetzt, sampelt `texDst` sonst Schwarz, sodass
    // der untere Layer faktisch gegen Null gemischt würde (Multiply → schwarz).
    if shader.loc_dst >= 0 {
        unsafe { ffi::SetShaderValueTexture(raw_shader, shader.loc_dst, dst_tex) };
    }
    sm.draw_texture_pro(
        RawTex(src_tex),
        Rectangle::new(0.0, 0.0, w, -h),
        Rectangle::new(0.0, 0.0, w, h),
        Vector2::new(0.0, 0.0),
        0.0,
        raylib::color::Color::WHITE,
    );
}