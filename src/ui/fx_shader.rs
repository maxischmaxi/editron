//! GPU-Pendant zu `core/effects.rs`: Fragment-Shader + Render-Pass-Kette
//! für die Echtzeit-Effektvorschau im Programmmonitor. Jeder Pass MUSS
//! formelgleich mit dem CPU-Pfad (`apply_effects_buffer`) bleiben, damit
//! Vorschau und Export identisch aussehen.
//!
//! Ablauf: Panels melden pro sichtbarem Layer mit aktiven Effekten einen
//! [`EffectJob`] über `ui.effect_requests`; der Mainloop verarbeitet die
//! Jobs ZWISCHEN den Frames (wie Texture-Uploads): Quelltexture → Ping-Pong
//! über bis zu drei RenderTextures (ein Pass je Effekt, Blur/Sharpen/Glow
//! mehrere) → Ergebnis unter `fx://clip/<id>`. Der Monitor zeichnet dann
//! die Ergebnis-Texture (mit dem Grade-Shader obendrauf).
//!
//! raylib-Falle: In eine RenderTexture Gerendertes erscheint beim späteren
//! Zeichnen vertikal gespiegelt. Alle Pässe zeichnen mit POSITIVEM
//! Quellrechteck; die Parität (gespiegelt ja/nein) wird pro Pass getrackt —
//! ortsabhängige Shader (Crop, Mosaik) bekommen sie als `srcFlip`-Uniform,
//! und der finale Monitor-Draw kompensiert sie über ein negatives
//! Quellrechteck.

use crate::core::effects::{
    blur_sigma, gaussian_kernel, glow_sigma, hue_matrix, pixelate_block, sharpen_sigma,
    EffectKind, ResolvedEffect,
};
use crate::ui::textures::TextureCache;
use raylib::color::Color;
use raylib::consts::BlendMode;
use raylib::core::shaders::Shader;
use raylib::core::texture::RenderTexture2D;
use raylib::ffi;
use raylib::math::{Rectangle, Vector2};
use raylib::prelude::{
    RaylibBlendModeExt, RaylibDraw, RaylibRenderTexture2D, RaylibShader, RaylibShaderModeExt,
    RaylibTextureModeExt,
};
use raylib::{RaylibHandle, RaylibThread};
use std::collections::HashMap;

/// Texture-Schlüssel des Effekt-Ergebnisses eines Clips.
pub fn fx_output_key(clip_id: &str) -> String {
    format!("fx://clip/{clip_id}")
}

/// Auftrag: Quelltexture durch einen aufgelösten Effekt-Stapel schicken.
#[derive(Clone, Debug)]
pub struct EffectJob {
    pub out_key: String,
    pub source_key: String,
    pub effects: Vec<ResolvedEffect>,
}

/// Ergebnis-Texture eines Jobs (roher GL-Handle, Copy — vermeidet
/// Borrow-Konflikte zwischen Renderer und Ui).
#[derive(Clone, Copy)]
pub struct FxOutput {
    pub tex: ffi::Texture2D,
    pub flipped: bool,
}

/// Bis zu drei Ping-Pong-RenderTextures je Ziel (Blur-basierte Effekte
/// brauchen Original + zwei Zwischenstufen gleichzeitig).
struct FxTarget {
    rts: Vec<RenderTexture2D>,
    w: i32,
    h: i32,
    final_idx: usize,
    flipped: bool,
}

/// Wrapper, damit rohe `ffi::Texture2D`-Kopien an draw_*-APIs gehen können.
struct RawTex(ffi::Texture2D);

impl AsRef<ffi::Texture2D> for RawTex {
    fn as_ref(&self) -> &ffi::Texture2D {
        &self.0
    }
}

// ------------------------------------------------------------------ Shader

/// GLSL-330-Header: raylibs Default-Vertex-Stage liefert fragTexCoord.
///
/// WICHTIG: Jeder Pass-Shader MUSS sein `finalColor` mit `* colDiffuse * fragColor`
/// abschließen (raylib-Konvention). Für den eigentlichen Effekt-Render ist das ein
/// No-Op (gezeichnet wird immer mit `Color::WHITE` ⇒ beide = 1), ABER: Die fx-Pässe
/// laufen ZWISCHEN den Frames in RenderTextures, und raylibs gebatchtes 2D-Rendering
/// kann den fx-Shader als aktives GL-Programm in das anschließende BILDSCHIRM-Rendering
/// durchsickern lassen. Ein fx-Shader OHNE `fragColor` ignoriert dann die Füllfarbe und
/// rendert jede `ui.fill()` als nackte weiße Shapes-Textur — das verursachte das
/// weiße Titlebar-Flackern beim Scrubben eines Clips mit animiertem Farbeffekt.
const HEADER: &str = "#version 330\nin vec2 fragTexCoord;\nin vec4 fragColor;\nuniform sampler2D texture0;\nuniform vec4 colDiffuse;\nout vec4 finalColor;\n";

/// Gemeinsame Helfer (formelgleich mit `core/effects.rs`).
const HELPERS: &str = r#"
float lumaOf(vec3 c) { return dot(c, vec3(0.2126, 0.7152, 0.0722)); }
float softEdge(float e, float f, float x) {
    if (f < 1e-6) { return x >= e ? 1.0 : 0.0; }
    return smoothstep(e, e + f, x);
}
"#;

/// Separierbarer Gauß-Pass (premultipliziert, randgeklemmt).
const BLUR_SRC: &str = r#"
uniform vec2 dir;        // Texel-Schritt: (1/w, 0) oder (0, 1/h)
uniform float sigma;
uniform int radius;
uniform vec2 halfTexel;
void main() {
    float inv = 1.0 / max(2.0 * sigma * sigma, 1e-9);
    vec4 c0 = texture(texture0, fragTexCoord);
    vec4 acc = vec4(c0.rgb * c0.a, c0.a);
    float wsum = 1.0;
    for (int i = 1; i <= radius; i++) {
        float w = exp(-float(i * i) * inv);
        vec2 off = dir * float(i);
        vec2 ua = clamp(fragTexCoord + off, halfTexel, vec2(1.0) - halfTexel);
        vec2 ub = clamp(fragTexCoord - off, halfTexel, vec2(1.0) - halfTexel);
        vec4 ca = texture(texture0, ua);
        vec4 cb = texture(texture0, ub);
        acc += (vec4(ca.rgb * ca.a, ca.a) + vec4(cb.rgb * cb.a, cb.a)) * w;
        wsum += 2.0 * w;
    }
    acc /= wsum;
    vec3 rgb = acc.a > 1e-5 ? acc.rgb / acc.a : vec3(0.0);
    finalColor = vec4(clamp(rgb, 0.0, 1.0), clamp(acc.a, 0.0, 1.0)) * colDiffuse * fragColor;
}
"#;

/// Kombinations-Pass: texture0 = weichgezeichnete Kopie, texOrig = Original.
/// mode 0 = Unscharf-Maskierung, 1 = Glühen (Screen-Mischung).
const COMBINE_SRC: &str = r#"
uniform sampler2D texOrig;
uniform float amount;
uniform int mode;
void main() {
    vec4 o = texture(texOrig, fragTexCoord);
    vec4 b = texture(texture0, fragTexCoord);
    vec3 c;
    if (mode == 0) {
        c = o.rgb + (o.rgb - b.rgb) * amount;
    } else {
        c = vec3(1.0) - (vec3(1.0) - o.rgb) * (vec3(1.0) - b.rgb * amount);
    }
    finalColor = vec4(clamp(c, 0.0, 1.0), o.a) * colDiffuse * fragColor;
}
"#;

const CHROMA_SRC: &str = r#"
uniform vec3 keyColor;
uniform vec3 keyParams; // Toleranz, weiche Kante, Spill (je 0–100)
void main() {
    vec4 tex = texture(texture0, fragTexCoord);
    vec3 c = tex.rgb;
    float l = lumaOf(c);
    float cb = c.b - l;
    float cr = c.r - l;
    float kl = lumaOf(keyColor);
    float kcb = keyColor.b - kl;
    float kcr = keyColor.r - kl;
    float klen = max(length(vec2(kcb, kcr)), 1e-5);
    float dist = length(vec2(cb - kcb, cr - kcr));
    float t0 = keyParams.x / 100.0 * 0.4;
    float t1 = t0 + max(keyParams.y / 100.0 * 0.4, 0.01);
    float mask = smoothstep(t0, t1, dist);
    float s = keyParams.z / 100.0;
    if (s > 0.0) {
        vec2 u = vec2(kcb, kcr) / klen;
        float proj = max(cb * u.x + cr * u.y, 0.0);
        float sup = s * (1.0 - smoothstep(t0, t1 + 0.2, dist));
        float cb2 = cb - u.x * proj * sup;
        float cr2 = cr - u.y * proj * sup;
        float r = l + cr2;
        float b = l + cb2;
        float g = (l - 0.2126 * r - 0.0722 * b) / 0.7152;
        c = vec3(r, g, b);
    }
    finalColor = vec4(clamp(c, 0.0, 1.0), tex.a * mask) * colDiffuse * fragColor;
}
"#;

const LUMA_KEY_SRC: &str = r#"
uniform vec3 lumaParams; // Schwelle 0–1, weiche Kante 0–1, invertieren 0/1
void main() {
    vec4 tex = texture(texture0, fragTexCoord);
    float l = lumaOf(tex.rgb);
    float m = softEdge(lumaParams.x - lumaParams.y * 0.5, lumaParams.y, l);
    if (lumaParams.z > 0.5) { m = 1.0 - m; }
    finalColor = vec4(tex.rgb, tex.a * m) * colDiffuse * fragColor;
}
"#;

const CROP_SRC: &str = r#"
uniform vec4 edges;   // links, rechts, oben, unten (0–1)
uniform float feather;
uniform float srcFlip;
void main() {
    vec4 tex = texture(texture0, fragTexCoord);
    vec2 uv = srcFlip > 0.5
        ? vec2(fragTexCoord.x, 1.0 - fragTexCoord.y)
        : fragTexCoord;
    float m = softEdge(edges.x, feather, uv.x)
        * (1.0 - softEdge(1.0 - edges.y - feather, feather, uv.x))
        * softEdge(edges.z, feather, uv.y)
        * (1.0 - softEdge(1.0 - edges.w - feather, feather, uv.y));
    finalColor = vec4(tex.rgb, tex.a * m) * colDiffuse * fragColor;
}
"#;

const FLIP_SRC: &str = r#"
uniform vec2 flipAxes; // horizontal, vertikal (0/1)
void main() {
    vec2 uv = mix(fragTexCoord, vec2(1.0) - fragTexCoord, flipAxes);
    finalColor = texture(texture0, uv) * colDiffuse * fragColor;
}
"#;

const PIXELATE_SRC: &str = r#"
uniform vec2 contentSize;
uniform float blockSize;
uniform float centerOff; // floor(block/2) + 0,5 → Texel-Mitte wie im CPU-Pfad
uniform float srcFlip;
void main() {
    vec2 px = fragTexCoord * contentSize;
    if (srcFlip > 0.5) { px.y = contentSize.y - px.y; }
    vec2 sp = floor(px / blockSize) * blockSize + vec2(centerOff);
    sp = min(sp, contentSize - vec2(0.5));
    if (srcFlip > 0.5) { sp.y = contentSize.y - sp.y; }
    finalColor = texture(texture0, sp / contentSize) * colDiffuse * fragColor;
}
"#;

const POSTERIZE_SRC: &str = r#"
uniform float levels;
void main() {
    vec4 tex = texture(texture0, fragTexCoord);
    vec3 c = floor(tex.rgb * (levels - 1.0) + 0.5) / (levels - 1.0);
    finalColor = vec4(clamp(c, 0.0, 1.0), tex.a) * colDiffuse * fragColor;
}
"#;

const INVERT_SRC: &str = r#"
uniform float mixAmt;
void main() {
    vec4 tex = texture(texture0, fragTexCoord);
    vec3 c = tex.rgb + (vec3(1.0) - 2.0 * tex.rgb) * mixAmt;
    finalColor = vec4(clamp(c, 0.0, 1.0), tex.a) * colDiffuse * fragColor;
}
"#;

const HUE_SAT_SRC: &str = r#"
uniform vec3 hueR;
uniform vec3 hueG;
uniform vec3 hueB;
uniform vec2 satLight; // Sättigung (Faktor), Helligkeit (−1…1)
void main() {
    vec4 tex = texture(texture0, fragTexCoord);
    vec3 c0 = tex.rgb;
    vec3 c = vec3(dot(hueR, c0), dot(hueG, c0), dot(hueB, c0));
    float l = lumaOf(c);
    c = vec3(l) + (c - vec3(l)) * satLight.x + vec3(satLight.y * 0.5);
    finalColor = vec4(clamp(c, 0.0, 1.0), tex.a) * colDiffuse * fragColor;
}
"#;

const BRIGHTNESS_SRC: &str = r#"
uniform vec2 bc; // Helligkeit (−1…1), Kontrast-Steigung
void main() {
    vec4 tex = texture(texture0, fragTexCoord);
    vec3 c = (tex.rgb - 0.5) * bc.y + 0.5 + bc.x * 0.5;
    finalColor = vec4(clamp(c, 0.0, 1.0), tex.a) * colDiffuse * fragColor;
}
"#;

/// Kompilierter Pass-Shader mit aufgelösten Uniform-Locations.
struct PassShader {
    shader: Shader,
    locs: HashMap<&'static str, i32>,
}

impl PassShader {
    fn load(
        rl: &mut RaylibHandle,
        thread: &RaylibThread,
        name: &str,
        body: &str,
        uniforms: &[&'static str],
        with_helpers: bool,
    ) -> Option<PassShader> {
        let src = format!(
            "{HEADER}{}{}",
            if with_helpers { HELPERS } else { "" },
            body
        );
        let shader = rl.load_shader_from_memory(thread, None, Some(&src));
        if !shader.is_shader_valid() {
            eprintln!("[fx] Effekt-Shader „{name}“ nicht ladbar — Vorschau ohne diesen Effekt");
            return None;
        }
        let locs = uniforms
            .iter()
            .map(|u| (*u, shader.get_shader_location(u)))
            .collect();
        Some(PassShader { shader, locs })
    }

    fn loc(&self, name: &str) -> i32 {
        self.locs.get(name).copied().unwrap_or(-1)
    }
}

/// Alle Pass-Shader (None ⇒ Kompilierung fehlgeschlagen, Effekt wird in der
/// Vorschau übersprungen; der CPU-Export bleibt davon unberührt).
struct ShaderSet {
    blur: Option<PassShader>,
    combine: Option<PassShader>,
    chroma: Option<PassShader>,
    luma_key: Option<PassShader>,
    crop: Option<PassShader>,
    flip: Option<PassShader>,
    pixelate: Option<PassShader>,
    posterize: Option<PassShader>,
    invert: Option<PassShader>,
    hue_sat: Option<PassShader>,
    brightness: Option<PassShader>,
}

impl ShaderSet {
    /// Uniforms eines Einzel-Pass-Effekts setzen und den Shader liefern;
    /// None ⇒ Shader fehlt oder der Effekt wirkt nicht (z. B. Block ≤ 1).
    fn setup_single(
        &mut self,
        fx: &ResolvedEffect,
        w: i32,
        h: i32,
        flipped: bool,
    ) -> Option<&mut PassShader> {
        let v = |i: usize| fx.values.get(i).copied().unwrap_or(0.0);
        let flip_val = if flipped { 1.0f32 } else { 0.0 };
        match fx.kind {
            EffectKind::ChromaKey => {
                let ps = self.chroma.as_mut()?;
                ps.shader.set_shader_value(
                    ps.loc("keyColor"),
                    [
                        (v(0) / 255.0) as f32,
                        (v(1) / 255.0) as f32,
                        (v(2) / 255.0) as f32,
                    ],
                );
                ps.shader.set_shader_value(
                    ps.loc("keyParams"),
                    [v(3) as f32, v(4) as f32, v(5) as f32],
                );
                self.chroma.as_mut()
            }
            EffectKind::LumaKey => {
                let ps = self.luma_key.as_mut()?;
                ps.shader.set_shader_value(
                    ps.loc("lumaParams"),
                    [
                        (v(0) / 100.0) as f32,
                        (v(1) / 100.0).max(0.0) as f32,
                        if v(2) >= 0.5 { 1.0f32 } else { 0.0 },
                    ],
                );
                self.luma_key.as_mut()
            }
            EffectKind::Crop => {
                let ps = self.crop.as_mut()?;
                ps.shader.set_shader_value(
                    ps.loc("edges"),
                    [
                        (v(0) / 100.0) as f32,
                        (v(1) / 100.0) as f32,
                        (v(2) / 100.0) as f32,
                        (v(3) / 100.0) as f32,
                    ],
                );
                ps.shader
                    .set_shader_value(ps.loc("feather"), (v(4) / 100.0 * 0.1) as f32);
                ps.shader.set_shader_value(ps.loc("srcFlip"), flip_val);
                self.crop.as_mut()
            }
            EffectKind::Flip => {
                let ps = self.flip.as_mut()?;
                ps.shader.set_shader_value(
                    ps.loc("flipAxes"),
                    [
                        if v(0) >= 0.5 { 1.0f32 } else { 0.0 },
                        if v(1) >= 0.5 { 1.0f32 } else { 0.0 },
                    ],
                );
                self.flip.as_mut()
            }
            EffectKind::Pixelate => {
                let block = pixelate_block(v(0), w as usize);
                if block <= 1 {
                    return None;
                }
                let ps = self.pixelate.as_mut()?;
                ps.shader
                    .set_shader_value(ps.loc("contentSize"), [w as f32, h as f32]);
                ps.shader.set_shader_value(ps.loc("blockSize"), block as f32);
                ps.shader
                    .set_shader_value(ps.loc("centerOff"), (block / 2) as f32 + 0.5);
                ps.shader.set_shader_value(ps.loc("srcFlip"), flip_val);
                self.pixelate.as_mut()
            }
            EffectKind::Posterize => {
                let ps = self.posterize.as_mut()?;
                ps.shader.set_shader_value(
                    ps.loc("levels"),
                    (v(0).round() as f32).clamp(2.0, 256.0),
                );
                self.posterize.as_mut()
            }
            EffectKind::Invert => {
                let ps = self.invert.as_mut()?;
                ps.shader
                    .set_shader_value(ps.loc("mixAmt"), (v(0) / 100.0) as f32);
                self.invert.as_mut()
            }
            EffectKind::HueSaturation => {
                let m = hue_matrix(v(0));
                let ps = self.hue_sat.as_mut()?;
                ps.shader.set_shader_value(ps.loc("hueR"), [m[0], m[1], m[2]]);
                ps.shader.set_shader_value(ps.loc("hueG"), [m[3], m[4], m[5]]);
                ps.shader.set_shader_value(ps.loc("hueB"), [m[6], m[7], m[8]]);
                ps.shader.set_shader_value(
                    ps.loc("satLight"),
                    [(v(1) / 100.0) as f32, (v(2) / 100.0) as f32],
                );
                self.hue_sat.as_mut()
            }
            EffectKind::BrightnessContrast => {
                let ps = self.brightness.as_mut()?;
                ps.shader.set_shader_value(
                    ps.loc("bc"),
                    [
                        (v(0) / 100.0) as f32,
                        (1.0 + v(1) / 100.0).max(0.0) as f32,
                    ],
                );
                self.brightness.as_mut()
            }
            _ => None,
        }
    }
}

// ---------------------------------------------------------------- Renderer

pub struct EffectChainRenderer {
    shaders: ShaderSet,
    targets: HashMap<String, FxTarget>,
}

/// Aktueller Ketteneingang: Quelltexture oder eine der Ping-Pong-RTs.
#[derive(Clone, Copy, PartialEq)]
enum Input {
    Src,
    Rt(usize),
}

impl EffectChainRenderer {
    pub fn load(rl: &mut RaylibHandle, thread: &RaylibThread) -> EffectChainRenderer {
        EffectChainRenderer {
            shaders: ShaderSet {
                blur: PassShader::load(rl, thread, "blur", BLUR_SRC, &["dir", "sigma", "radius", "halfTexel"], false),
                combine: PassShader::load(rl, thread, "combine", COMBINE_SRC, &["texOrig", "amount", "mode"], false),
                chroma: PassShader::load(rl, thread, "chromaKey", CHROMA_SRC, &["keyColor", "keyParams"], true),
                luma_key: PassShader::load(rl, thread, "lumaKey", LUMA_KEY_SRC, &["lumaParams"], true),
                crop: PassShader::load(rl, thread, "crop", CROP_SRC, &["edges", "feather", "srcFlip"], true),
                flip: PassShader::load(rl, thread, "flip", FLIP_SRC, &["flipAxes"], false),
                pixelate: PassShader::load(rl, thread, "pixelate", PIXELATE_SRC, &["contentSize", "blockSize", "centerOff", "srcFlip"], false),
                posterize: PassShader::load(rl, thread, "posterize", POSTERIZE_SRC, &["levels"], false),
                invert: PassShader::load(rl, thread, "invert", INVERT_SRC, &["mixAmt"], false),
                hue_sat: PassShader::load(rl, thread, "hueSaturation", HUE_SAT_SRC, &["hueR", "hueG", "hueB", "satLight"], true),
                brightness: PassShader::load(rl, thread, "brightnessContrast", BRIGHTNESS_SRC, &["bc"], false),
            },
            targets: HashMap::new(),
        }
    }

    /// Ergebnis eines Jobs (für `Ui`-Draws); None ⇒ (noch) nicht gerendert.
    pub fn output(&self, key: &str) -> Option<FxOutput> {
        let t = self.targets.get(key)?;
        let rt = t.rts.get(t.final_idx)?;
        Some(FxOutput {
            tex: *rt.texture().as_ref(),
            flipped: t.flipped,
        })
    }

    /// Alle Jobs eines Frames verarbeiten; nicht mehr angeforderte Ziele
    /// werden freigegeben.
    pub fn process(
        &mut self,
        rl: &mut RaylibHandle,
        thread: &RaylibThread,
        textures: &TextureCache,
        jobs: Vec<EffectJob>,
    ) {
        let wanted: std::collections::HashSet<&str> =
            jobs.iter().map(|j| j.out_key.as_str()).collect();
        self.targets.retain(|k, _| wanted.contains(k.as_str()));
        for job in &jobs {
            self.process_job(rl, thread, textures, job);
        }
    }

    fn process_job(
        &mut self,
        rl: &mut RaylibHandle,
        thread: &RaylibThread,
        textures: &TextureCache,
        job: &EffectJob,
    ) {
        let Some(src_tex) = textures.get(&job.source_key) else {
            self.targets.remove(&job.out_key);
            return;
        };
        let (w, h) = (src_tex.width, src_tex.height);
        if w <= 0 || h <= 0 || job.effects.is_empty() {
            self.targets.remove(&job.out_key);
            return;
        }
        let src_raw: ffi::Texture2D = *src_tex.as_ref();

        // Ziel-RTs sicherstellen (Größe folgt der Quelle).
        let entry = self.targets.entry(job.out_key.clone()).or_insert(FxTarget {
            rts: Vec::new(),
            w,
            h,
            final_idx: 0,
            flipped: false,
        });
        if entry.w != w || entry.h != h {
            entry.rts.clear();
            entry.w = w;
            entry.h = h;
        }

        // Disjunkte Borrows: Shader und Ziel getrennt halten.
        let shaders = &mut self.shaders;
        let target = self.targets.get_mut(&job.out_key).expect("target eben angelegt");

        let mut input = Input::Src;
        let mut flipped = false;
        let mut applied_any = false;
        let raw_of = |target: &FxTarget, input: Input| -> ffi::Texture2D {
            match input {
                Input::Src => src_raw,
                Input::Rt(i) => target
                    .rts
                    .get(i)
                    .map(|rt| *rt.texture().as_ref())
                    .unwrap_or(src_raw),
            }
        };

        for fx in &job.effects {
            match fx.kind {
                EffectKind::GaussianBlur => {
                    let sigma = blur_sigma(fx.values.first().copied().unwrap_or(0.0), w as usize);
                    let Some(blur) = shaders.blur.as_mut() else { continue };
                    if sigma < 0.1 {
                        continue;
                    }
                    let Some((i1, i2)) = pick_two(rl, thread, target, input) else { continue };
                    let in0 = raw_of(target, input);
                    run_blur_pass(rl, thread, target, blur, in0, i1, sigma, w, h, true);
                    let in1 = raw_of(target, Input::Rt(i1));
                    run_blur_pass(rl, thread, target, blur, in1, i2, sigma, w, h, false);
                    input = Input::Rt(i2);
                    applied_any = true;
                    // Zwei Pässe ⇒ Parität unverändert.
                }
                EffectKind::Sharpen | EffectKind::Glow => {
                    if shaders.blur.is_none() || shaders.combine.is_none() {
                        continue;
                    }
                    let (sigma, amount, mode) = if fx.kind == EffectKind::Sharpen {
                        (
                            sharpen_sigma(w as usize),
                            (fx.values.first().copied().unwrap_or(0.0) / 100.0) as f32,
                            0i32,
                        )
                    } else {
                        (
                            glow_sigma(fx.values.get(1).copied().unwrap_or(0.0), w as usize),
                            (fx.values.first().copied().unwrap_or(0.0) / 100.0) as f32,
                            1i32,
                        )
                    };
                    if amount <= 0.0 {
                        continue;
                    }
                    let orig = input;
                    let Some((i1, i2)) = pick_two(rl, thread, target, input) else { continue };
                    {
                        let blur = shaders.blur.as_mut().expect("blur shader");
                        let in0 = raw_of(target, orig);
                        run_blur_pass(rl, thread, target, blur, in0, i1, sigma.max(0.1), w, h, true);
                        let in1 = raw_of(target, Input::Rt(i1));
                        run_blur_pass(rl, thread, target, blur, in1, i2, sigma.max(0.1), w, h, false);
                    }
                    // Combine: texture0 = Blur (i2), texOrig = Original → i1.
                    let combine = shaders.combine.as_mut().expect("combine shader");
                    combine.shader.set_shader_value(combine.loc("amount"), amount);
                    combine.shader.set_shader_value(combine.loc("mode"), mode);
                    let orig_raw = raw_of(target, orig);
                    let blur_raw = raw_of(target, Input::Rt(i2));
                    let extra = Some((combine.loc("texOrig"), orig_raw));
                    run_pass(rl, thread, target, &mut combine.shader, blur_raw, i1, w, h, extra);
                    input = Input::Rt(i1);
                    flipped = !flipped;
                    applied_any = true;
                }
                _ => {
                    let Some(ti) = pick_one(rl, thread, target, input) else { continue };
                    let in_raw = raw_of(target, input);
                    let Some(ps) = shaders.setup_single(fx, w, h, flipped) else {
                        continue;
                    };
                    run_pass(rl, thread, target, &mut ps.shader, in_raw, ti, w, h, None);
                    input = Input::Rt(ti);
                    flipped = !flipped;
                    applied_any = true;
                }
            }
        }

        let keep = match (applied_any, input) {
            (true, Input::Rt(idx)) => {
                target.final_idx = idx;
                target.flipped = flipped;
                true
            }
            _ => false,
        };
        if !keep {
            // Nichts angewendet (Shader fehlen/Identität) → Fallback aufs Original.
            self.targets.remove(&job.out_key);
        }
    }
}

/// RT mit Index `idx` sicherstellen (Liste wächst lazy bis 3).
fn ensure_rt(rl: &mut RaylibHandle, thread: &RaylibThread, target: &mut FxTarget, idx: usize) -> bool {
    while target.rts.len() <= idx {
        match rl.load_render_texture(thread, target.w as u32, target.h as u32) {
            Ok(rt) => {
                unsafe {
                    ffi::SetTextureFilter(
                        *rt.texture().as_ref(),
                        ffi::TextureFilter::TEXTURE_FILTER_BILINEAR as i32,
                    );
                }
                target.rts.push(rt);
            }
            Err(_) => return false,
        }
    }
    true
}

/// Einen freien RT-Index wählen (≠ aktueller Eingang).
fn pick_one(
    rl: &mut RaylibHandle,
    thread: &RaylibThread,
    target: &mut FxTarget,
    input: Input,
) -> Option<usize> {
    let idx = (0..3usize).find(|i| input != Input::Rt(*i))?;
    ensure_rt(rl, thread, target, idx).then_some(idx)
}

/// Zwei freie RT-Indizes wählen (≠ Eingang).
fn pick_two(
    rl: &mut RaylibHandle,
    thread: &RaylibThread,
    target: &mut FxTarget,
    input: Input,
) -> Option<(usize, usize)> {
    let mut free = (0..3usize).filter(|i| input != Input::Rt(*i));
    let a = free.next()?;
    let b = free.next()?;
    (ensure_rt(rl, thread, target, a) && ensure_rt(rl, thread, target, b)).then_some((a, b))
}

/// Blur-Pass mit Richtungs-Uniforms ausführen.
#[allow(clippy::too_many_arguments)]
fn run_blur_pass(
    rl: &mut RaylibHandle,
    thread: &RaylibThread,
    target: &mut FxTarget,
    blur: &mut PassShader,
    input: ffi::Texture2D,
    target_idx: usize,
    sigma: f32,
    w: i32,
    h: i32,
    horizontal: bool,
) {
    let (radius, _) = gaussian_kernel(sigma);
    let dir = if horizontal {
        [1.0f32 / w as f32, 0.0]
    } else {
        [0.0, 1.0f32 / h as f32]
    };
    blur.shader.set_shader_value(blur.loc("dir"), dir);
    blur.shader.set_shader_value(blur.loc("sigma"), sigma);
    blur.shader
        .set_shader_value(blur.loc("radius"), radius as i32);
    blur.shader.set_shader_value(
        blur.loc("halfTexel"),
        [0.5f32 / w as f32, 0.5f32 / h as f32],
    );
    run_pass(rl, thread, target, &mut blur.shader, input, target_idx, w, h, None);
}

/// Einen Render-Pass ausführen: `input` mit `shader` nach RT `target_idx`.
/// Blending ist deaktiviert (GL_ONE/GL_ZERO) — der Shader-Output landet
/// unverändert inkl. Alpha im Ziel.
#[allow(clippy::too_many_arguments)]
fn run_pass(
    rl: &mut RaylibHandle,
    thread: &RaylibThread,
    target: &mut FxTarget,
    shader: &mut Shader,
    input: ffi::Texture2D,
    target_idx: usize,
    w: i32,
    h: i32,
    extra_tex: Option<(i32, ffi::Texture2D)>,
) {
    let Some(rt) = target.rts.get_mut(target_idx) else { return };
    let raw_shader: ffi::Shader = *shader.as_ref();
    let mut d = rl.begin_texture_mode(thread, rt);
    d.clear_background(Color::BLANK);
    // GL_ONE (1) / GL_ZERO (0) / GL_FUNC_ADD (0x8006): Quelle überschreibt das Ziel.
    unsafe { ffi::rlSetBlendFactors(1, 0, 0x8006) };
    let mut bm = d.begin_blend_mode(BlendMode::BLEND_CUSTOM);
    let mut sm = bm.begin_shader_mode(shader);
    if let Some((loc, tex)) = extra_tex {
        if loc >= 0 {
            unsafe { ffi::SetShaderValueTexture(raw_shader, loc, tex) };
        }
    }
    sm.draw_texture_rec(
        RawTex(input),
        Rectangle::new(0.0, 0.0, w as f32, h as f32),
        Vector2::new(0.0, 0.0),
        Color::WHITE,
    );
}
