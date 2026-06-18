use super::*;
use crate::core::animation::{AnimatedParam, ClipFx};
use crate::core::compose;
use crate::core::effects::{self, EffectInstance};
use crate::core::grade::ColorGrade;
use crate::core::sequence::SequenceSettings;
use crate::core::timeline::{
    sequence_end, track_name, TimelineClip, TimelineStore, TimelineTrack, TrackKind,
};
use crate::core::transitions::{
    self, Transition, TransitionDirection, TransitionFx, TransitionKind, TransitionRole,
};
use crate::core::types::MediaKind;
use crate::stores::MediaStore;
use std::collections::HashMap;

// ============================================================== Renderplan

/// Übergangs-Fenster eines Layers: Zeiten relativ zum Exportbeginn.
/// Außerhalb des Fensters wird der Fortschritt geklemmt (0 davor, 1 danach)
/// — die Rollen-Formeln liefern dort die Identität.
#[derive(Clone, Debug, PartialEq)]
pub struct PlanTransition {
    pub kind: TransitionKind,
    pub direction: TransitionDirection,
    pub role: TransitionRole,
    pub t0: f64,
    pub t1: f64,
}

impl PlanTransition {
    /// Auswirkung zur Exportzeit `t` (gemeinsame Formeln mit der Vorschau).
    pub fn eval(&self, t: f64) -> TransitionFx {
        let span = (self.t1 - self.t0).max(1e-9);
        let p = ((t - self.t0) / span).clamp(0.0, 1.0);
        transitions::eval_video(self.kind, self.direction, self.role, p)
    }
}

/// Kombinierte Übergangs-Auswirkung mehrerer Fenster (Clipanfang + -ende).
pub fn eval_plan_transitions(list: &[PlanTransition], t: f64) -> TransitionFx {
    list.iter()
        .fold(TransitionFx::IDENTITY, |acc, tr| acc.combine(&tr.eval(t)))
}

/// Ein Video-Layer eines Segments (Zeichenreihenfolge: unten → oben).
#[derive(Clone, Debug)]
pub struct VideoLayerPlan {
    pub clip_id: String,
    pub path: String,
    pub image: bool,
    /// Medienzeit des ersten Segment-Frames.
    pub src_in: f64,
    /// Medienfortschritt pro Ausgabesekunde (signiert): speed vorwärts,
    /// −speed rückwärts, 0 = Standbild. Medienzeit von Frame f =
    /// src_in + (f/fps)·media_step — identisch zur Vorschau-Abbildung.
    pub media_step: f64,
    /// Animierbare Parameter (Keyframes in Medienzeit).
    pub fx: ClipFx,
    /// Farbkorrektur des Clips (statisch).
    pub grade: ColorGrade,
    /// Effekt-Stapel des Clips (Keyframes in Medienzeit).
    pub effects: Vec<EffectInstance>,
    /// Natürliche Quellmaße (0 = unbekannt) — Bezugsrahmen der Vignette
    /// im transparent gepolsterten Decode-Puffer.
    pub natural_w: u32,
    pub natural_h: u32,
    /// Bittiefe der Quelle (8/10/12/16). >8 ⇒ Decode in 16 Bit (rgba64le)
    /// statt 8 (rgba), damit 10-Bit-Material ohne Banding durch die
    /// f32-Pipeline läuft. Generatoren/Farbflächen: 8.
    pub src_bit_depth: u32,
    /// Aktive Übergangs-Fenster dieses Layers im Segment.
    pub transitions: Vec<PlanTransition>,
    /// Farbfläche (Dip zu Schwarz/Weiß) statt Medien — ohne Decoder.
    pub solid: Option<[u8; 3]>,
    /// Titel-Generator statt Medien — CPU-gerastert, ohne Decoder.
    pub title: Option<crate::core::title::TitleSpec>,
    /// Verschachtelte Sequenz statt Medien: die Ebene wird rekursiv aus
    /// `RenderPlan::nests` komponiert (ID der inneren Sequenz).
    pub nest_seq: Option<String>,
    /// Adjustment Layer (Einstellungsebene): kein eigenes Bild, sondern ein
    /// Korrektur-Pass (`effects` → `grade`) auf das bis hierhin zusammengesetzte
    /// Canvas der Spuren darunter. `fx.opacity` regelt die Wirkstärke.
    pub adjustment: bool,
    /// Ebenen-Mischmodus (Normal = Src-over).
    pub blend_mode: crate::core::compose::BlendMode,
}

impl VideoLayerPlan {
    /// Schnellpfad-Kriterium: keinerlei visuelle Transformation/Korrektur.
    /// Titel laufen immer durch den Compositor (es gibt keine Quelldatei,
    /// die ffmpeg direkt durchpumpen könnte).
    pub fn is_identity(&self) -> bool {
        self.transitions.is_empty()
            && self.solid.is_none()
            && self.title.is_none()
            // Nest-Ebenen werden immer rekursiv komponiert (kein Schnellpfad).
            && self.nest_seq.is_none()
            // Adjustment Layer brauchen den Compositing-Pfad (Pass aufs Canvas).
            && !self.adjustment
            && self.media_step >= 0.0
            && self.fx.is_visual_identity()
            && !self.grade.is_active()
            && !effects::has_active_video_effects(&self.effects)
    }
}

#[derive(Clone, Debug)]
pub struct VideoSegment {
    pub frames: u64,
    /// Leer = Schwarzbild (Lücke).
    pub layers: Vec<VideoLayerPlan>,
}

/// Crossfade-Hüllkurve im Mix: Fenster in Mix-Zeit (Sekunden ab Exportbeginn).
#[derive(Clone, Debug, PartialEq)]
pub struct PlanAudioFade {
    pub t0: f64,
    pub t1: f64,
    /// true = eingehende Seite (Einblenden), false = ausgehende (Ausblenden).
    pub fade_in: bool,
    /// Konstante Leistung (sin/cos) statt konstanter Verstärkung (linear).
    pub equal_power: bool,
}

impl PlanAudioFade {
    /// Verstärkungsfaktor zur Mix-Zeit `t` (identische Kurven wie der Player).
    pub fn gain_at(&self, t: f64) -> f64 {
        let span = (self.t1 - self.t0).max(1e-9);
        let p = ((t - self.t0) / span).clamp(0.0, 1.0);
        transitions::audio_gain(self.equal_power, self.fade_in, p)
    }
}

#[derive(Clone, Debug)]
pub struct AudioClipPlan {
    pub path: String,
    /// Startzeit im Mix (Sekunden ab Exportbeginn).
    pub start_in_mix: f64,
    pub duration: f64,
    pub src_in: f64,
    /// Clip-Geschwindigkeit: Medienspanne = duration × speed; die Wiedergabe
    /// läuft pitch-korrigiert über dieselbe atempo-Kette wie der Player.
    pub speed: f64,
    /// Wirksamer Faktor je Seite: Master × Spur × Clip × Balance.
    pub gain_l: f32,
    pub gain_r: f32,
    /// Lautstärke-Kurve des Clips (dB, Keyframes in Medienzeit).
    pub volume: AnimatedParam,
    /// Audio-Effekt-Stapel des Clips (DSP vor Gain/Hüllkurve).
    pub effects: Vec<EffectInstance>,
    /// Crossfade-Fenster (Übergänge an den Clipkanten).
    pub fades: Vec<PlanAudioFade>,
}

/// Audio-Spur mit Bus-Effekten und/oder Automation: wird getrennt gemischt
/// (Clips → Per-Spur-WAV → Bus-FX + Spur-Gain/Pan + Master → Master-WAV),
/// damit die Effekte auf die SUMME der Spur wirken — exakt wie der Player-
/// Mixdown. Spuren ohne FX/Automation laufen über den Schnellpfad
/// (`RenderPlan::audio`, Gains fertig eingebacken).
#[derive(Clone, Debug)]
pub struct AudioTrackPlan {
    /// Spurname (A1, A2, …) — Stream-Titel bzw. Stem-Label.
    pub name: String,
    /// Clips der Spur; `gain_l`/`gain_r` enthalten NUR den Clip-Anteil
    /// (Clip-Gain), Spur-Gain/Pan und Master folgen in der Bus-Verarbeitung.
    pub clips: Vec<AudioClipPlan>,
    /// Bus-Effekt-Kette (Insert) der Spur.
    pub effects: Vec<EffectInstance>,
    /// Lautstärke-Automation (dB-Offset, Keyframes in Sequenzzeit).
    pub volume_auto: AnimatedParam,
    /// Pan-Automation (Offset, Keyframes in Sequenzzeit).
    pub pan_auto: AnimatedParam,
    /// Statischer Spur-Fader (dB) und Balance.
    pub gain_db: f64,
    pub pan: f64,
    /// Master-Fader (dB) — beim Summieren in den Master angewendet.
    pub master_db: f64,
    /// Sequenzzeit, die Mix-Zeit t=0 entspricht (Exportbeginn) — für die
    /// Automations-Auswertung.
    pub seq_start: f64,
}

impl AudioTrackPlan {
    /// Wirksame Spur-Verstärkung (dB) zur Mix-Zeit `mix_t`: Fader + Automation.
    pub(crate) fn gain_db_at(&self, mix_t: f64) -> f64 {
        self.gain_db + self.volume_auto.eval(self.seq_start + mix_t)
    }

    /// Wirksame Balance zur Mix-Zeit `mix_t`: Fader + Automation (geklemmt).
    pub(crate) fn pan_at(&self, mix_t: f64) -> f64 {
        (self.pan + self.pan_auto.eval(self.seq_start + mix_t)).clamp(-1.0, 1.0)
    }
}

/// Untertitel-Spur im Renderplan (Sidecar/Einbetten): Cue-Zeiten relativ
/// zum Exportbeginn, frame-genau aufs Sequenzraster gerundet.
#[derive(Clone, Debug)]
pub struct SubtitlePlanTrack {
    /// Spurname (U1, U2, …) — Dateisuffix bzw. Stream-Titel.
    pub name: String,
    pub cues: Vec<crate::core::subtitle::SrtCue>,
}

/// Renderbare Momentaufnahme einer (verschachtelten) Sequenz für den Worker:
/// reine Daten, aus denen sich eine [`TimelineStore`] rekonstruieren lässt.
#[derive(Clone, Debug)]
pub struct NestSeq {
    pub settings: SequenceSettings,
    pub tracks: Vec<TimelineTrack>,
    pub clips: Vec<TimelineClip>,
    pub transitions: Vec<Transition>,
}

impl NestSeq {
    /// In eine voll funktionsfähige Timeline überführen (für die Komposition).
    pub fn to_timeline(&self) -> TimelineStore {
        let mut tl = TimelineStore::default();
        tl.load_document(
            Some(self.settings),
            self.tracks.clone(),
            self.clips.clone(),
            self.transitions.clone(),
            Vec::new(),
            0.0,
            None,
            None,
            40.0,
            true,
            Vec::new(),
            0.0,
            None,
        );
        tl
    }
}

/// Decode-Info eines Blatt-Clips innerhalb verschachtelter Sequenzen
/// (asset_id → Originalpfad + natürliche Maße). Der Export nutzt IMMER das
/// Original.
#[derive(Clone, Debug)]
pub struct NestMediaInfo {
    pub path: String,
    pub natural_w: u32,
    pub natural_h: u32,
    pub image: bool,
}

#[derive(Clone, Debug, Default)]
pub struct RenderPlan {
    pub duration: f64,
    /// Zielraster (0 bei Audio-only).
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub total_frames: u64,
    pub segments: Vec<VideoSegment>,
    /// Verschachtelte Sequenzen, die von Nest-Clips referenziert werden
    /// (transitiv aufgelöst) — vom Worker zur rekursiven Komposition genutzt.
    pub nests: HashMap<String, NestSeq>,
    /// Medien-Decode-Info der Blatt-Clips in den verschachtelten Sequenzen.
    pub nest_media: HashMap<String, NestMediaInfo>,
    /// Clips von Spuren OHNE Bus-FX/Automation (Schnellpfad: Gains fertig
    /// eingebacken, mischen direkt in den Master).
    pub audio: Vec<AudioClipPlan>,
    /// Spuren MIT Bus-FX und/oder Automation — getrennte Bus-Verarbeitung.
    pub audio_tracks: Vec<AudioTrackPlan>,
    /// Sichtbare Untertitel-Spuren mit Cues im Exportbereich (nur bei
    /// `SubtitleMode::Sidecar`/`Embed` befüllt; Einbrennen läuft über die
    /// Video-Segmente).
    pub subtitle_tracks: Vec<SubtitlePlanTrack>,
    /// Erkannter Ausgabe-Farbraum (ehrliche Tags); aus dem dominanten
    /// Quellmaterial, BT.709 als SDR-Default.
    pub color: OutputColor,
}

impl RenderPlan {
    /// Mindestens ein echtes Video-Segment (kein reines Schwarzbild)?
    pub fn has_video_media(&self) -> bool {
        self.segments.iter().any(|s| !s.layers.is_empty())
    }

    /// Mindestens ein hörbarer Audio-Clip? Berücksichtigt BEIDE Pfade: den
    /// Schnellpfad (`audio`) UND die getrennt verarbeiteten Spuren
    /// (`audio_tracks`, Bus-FX/Automation oder Stems). Spuren landen nur in
    /// `audio_tracks`, wenn sie tatsächlich Clips tragen — eine nicht-leere
    /// Liste bedeutet also hörbares Material.
    pub fn has_audio_media(&self) -> bool {
        !self.audio.is_empty() || !self.audio_tracks.is_empty()
    }

    /// Gesamte Audio-Arbeitseinheiten (Frames) für den Fortschritt: einfache
    /// Clips + Per-Spur-Clips + die Bus-Verarbeitungs-Durchläufe der Spuren.
    pub fn audio_total_units(&self, rate: u32) -> u64 {
        let frames = |d: f64| (d * rate as f64) as u64;
        let simple: u64 = self.audio.iter().map(|c| frames(c.duration)).sum();
        let tracks: u64 = self
            .audio_tracks
            .iter()
            .map(|t| {
                let clips: u64 = t.clips.iter().map(|c| frames(c.duration)).sum();
                // + ein voller Durchlauf über die Sequenzdauer (Bus-FX/Gain).
                clips + frames(self.duration)
            })
            .sum();
        simple + tracks
    }
}

/// dB → linearer Faktor; ≤ −60 dB gilt als −∞ (stumm). Identisch zum Player.
pub(crate) fn db_to_linear(db: f64) -> f32 {
    if db <= -60.0 {
        0.0
    } else {
        10f32.powf(db as f32 / 20.0)
    }
}

/// Stereo-Balance wie im Player: dämpft die abgewandte Seite.
pub(crate) fn pan_gains(pan: f64) -> (f32, f32) {
    let p = pan.clamp(-1.0, 1.0) as f32;
    (1.0 - p.max(0.0), 1.0 + p.min(0.0))
}

/// Exportbereich: ganze Sequenz oder In/Out (sofern gültig gesetzt).
pub fn export_range(timeline: &TimelineStore, use_in_out: bool) -> (f64, f64) {
    let end = sequence_end(&timeline.clips);
    if use_in_out {
        if let (Some(i), Some(o)) = (timeline.in_point, timeline.out_point) {
            let a = i.max(0.0);
            let b = o.max(0.0);
            if b - a > 1e-9 {
                return (a, b);
            }
        }
    }
    (0.0, end)
}

/// Leerer Nest-Resolver für Aufrufer ohne Sequenz-Kontext (Validierung, Tests,
/// Einzelbild-Export). Nest-Clips bleiben dann unaufgelöst (schwarz).
pub struct NoNests;
impl compose::NestResolver for NoNests {
    fn nested_timeline(&self, _id: &str) -> Option<&TimelineStore> {
        None
    }
}

/// Verschachtelte Sequenzen (transitiv) + ihre Blatt-Medien in den Plan
/// einsammeln, damit der Worker sie self-contained rekursiv komponieren kann.
fn gather_nests(
    timeline: &TimelineStore,
    media: &MediaStore,
    nests: &dyn compose::NestResolver,
    plan: &mut RenderPlan,
) {
    use std::collections::VecDeque;
    let mut queue: VecDeque<String> =
        timeline.clips.iter().filter_map(|c| c.nest_seq.clone()).collect();
    while let Some(id) = queue.pop_front() {
        if plan.nests.contains_key(&id) {
            continue;
        }
        let Some(inner) = nests.nested_timeline(&id) else {
            continue;
        };
        plan.nests.insert(
            id.clone(),
            NestSeq {
                settings: inner.settings,
                tracks: inner.tracks.clone(),
                clips: inner.clips.clone(),
                transitions: inner.transitions.clone(),
            },
        );
        for c in &inner.clips {
            if let Some(n) = &c.nest_seq {
                queue.push_back(n.clone());
            } else if !c.is_generator() {
                if let Some(a) = media.asset(&c.asset_id) {
                    if a.kind != MediaKind::Audio {
                        let (nw, nh) = a
                            .info
                            .video
                            .first()
                            .map(|v| (v.width, v.height))
                            .unwrap_or((0, 0));
                        plan.nest_media.entry(c.asset_id.clone()).or_insert(NestMediaInfo {
                            path: a.path.clone(),
                            natural_w: nw,
                            natural_h: nh,
                            image: a.kind == MediaKind::Image,
                        });
                    }
                }
            }
        }
    }
}

/// Audio einer verschachtelten Sequenz in den äußeren Mix einflachen: innere
/// Audio-Clips werden zeitverschoben und mit den inneren (Master/Spur/Clip)
/// und äußeren Gains gefaltet in `out` geschrieben. Rekursiv für tiefere
/// Nests. Bewusste v1-Grenzen: innere Spur-Bus-Effekte/-Automation und
/// Crossfades innerhalb der inneren Sequenz werden (noch) nicht berücksichtigt;
/// Nest-Clips werden mit Geschwindigkeit 1 angenommen.
#[allow(clippy::too_many_arguments)]
fn flatten_nest_audio(
    inner: &TimelineStore,
    media: &MediaStore,
    nests: &dyn compose::NestResolver,
    src_in: f64,
    dur: f64,
    outer_offset: f64,
    gain_l_acc: f32,
    gain_r_acc: f32,
    depth: usize,
    out: &mut Vec<AudioClipPlan>,
) {
    if depth >= compose::MAX_NEST_DEPTH {
        return;
    }
    let (win_lo, win_hi) = (src_in, src_in + dur);
    let solo_any = inner.tracks.iter().any(|t| t.solo);
    let inner_master = db_to_linear(inner.master_gain_db);
    for track in inner
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio && !t.muted && (!solo_any || t.solo))
    {
        let track_gain = db_to_linear(track.gain_db);
        let (pan_l, pan_r) = pan_gains(track.pan);
        for clip in inner.clips.iter().filter(|c| c.track_id == track.id && c.enabled) {
            if clip.reverse || clip.freeze || clip.is_time_remapped() {
                continue;
            }
            let lo = clip.start.max(win_lo);
            let hi = clip.end().min(win_hi);
            if hi - lo <= 1e-9 {
                continue;
            }
            let out_start = outer_offset + (lo - win_lo);
            // Tiefere Verschachtelung rekursiv einflachen.
            if let Some(inner_id) = &clip.nest_seq {
                if let Some(deeper) = nests.nested_timeline(inner_id) {
                    let gl = gain_l_acc * inner_master * track_gain * pan_l;
                    let gr = gain_r_acc * inner_master * track_gain * pan_r;
                    flatten_nest_audio(
                        deeper,
                        media,
                        nests,
                        clip.media_time_at(lo).max(0.0),
                        hi - lo,
                        out_start,
                        gl,
                        gr,
                        depth + 1,
                        out,
                    );
                }
                continue;
            }
            let Some(asset) = media.asset(&clip.asset_id) else {
                continue;
            };
            if asset.offline || asset.info.audio.is_empty() {
                continue;
            }
            let clip_gain = db_to_linear(clip.gain_db);
            out.push(AudioClipPlan {
                path: asset.path.clone(),
                start_in_mix: out_start,
                duration: hi - lo,
                src_in: clip.media_time_at(lo).max(0.0),
                speed: clip.eff_speed(),
                gain_l: gain_l_acc * inner_master * track_gain * pan_l * clip_gain,
                gain_r: gain_r_acc * inner_master * track_gain * pan_r * clip_gain,
                volume: clip.fx.volume_db.clone(),
                effects: clip.effects.iter().filter(|e| e.kind.is_audio()).cloned().collect(),
                fades: Vec::new(),
            });
        }
    }
}

/// Ausgabe-Farbraum aus dem Quellmaterial im Exportbereich erkennen. Ist eine
/// sichtbare Video-Quelle HDR (PQ/HLG) oder BT.2020, wird der „stärkste"
/// Farbraum gewählt (PQ > HLG > BT.2020 > BT.709) und durchgereicht; sonst
/// BT.709. Multicam-Winkel und (rekursiv) Nest-Inhalte werden hier (noch)
/// nicht inspiziert — sie fallen auf BT.709 zurück.
pub(crate) fn detect_output_color(
    timeline: &TimelineStore,
    media: &MediaStore,
    start: f64,
    end: f64,
) -> OutputColor {
    let mut best = OutputColor::Bt709;
    let rank = |c: OutputColor| match c {
        OutputColor::Bt709 => 0,
        OutputColor::Bt2020 => 1,
        OutputColor::Bt2020Hlg => 2,
        OutputColor::Bt2020Pq => 3,
    };
    for clip in &timeline.clips {
        if !clip.enabled || clip.start >= end || clip.end() <= start {
            continue;
        }
        let Some(track) = timeline.tracks.iter().find(|t| t.id == clip.track_id) else {
            continue;
        };
        if track.kind != TrackKind::Video {
            continue;
        }
        if let Some(asset) = media.asset(&clip.asset_id) {
            if let Some(v) = asset.info.video.first() {
                let c = OutputColor::from_stream(v);
                if rank(c) > rank(best) {
                    best = c;
                }
            }
        }
    }
    best
}

pub fn build_render_plan(
    timeline: &TimelineStore,
    media: &MediaStore,
    settings: &ExportSettings,
    nests: &dyn compose::NestResolver,
) -> RenderPlan {
    let (start, end) = export_range(timeline, settings.use_in_out);
    let duration = (end - start).max(0.0);
    let mut plan = RenderPlan {
        duration,
        ..Default::default()
    };
    if duration <= 0.0 {
        return plan;
    }

    // Solo wirkt global über alle Spuren (Player-Semantik).
    let solo_any = timeline.tracks.iter().any(|t| t.solo);
    let master = db_to_linear(timeline.master_gain_db);

    if let Some(video) = &settings.video {
        plan.width = video.width;
        plan.height = video.height;
        plan.fps = video.fps;
        plan.total_frames = ((duration * video.fps).round() as u64).max(1);
        // Ausgabe-Farbraum aus dem Quellmaterial im Exportbereich erkennen:
        // ist eine sichtbare Quelle Wide-Gamut/HDR (BT.2020/PQ/HLG), wird der
        // Farbraum durchgereicht statt nach BT.709 fehlgetaggt. Sonst BT.709.
        plan.color = detect_output_color(timeline, media, start, end);
        plan.segments = plan_video_segments(
            timeline,
            media,
            nests,
            start,
            plan.total_frames,
            video.fps,
            solo_any,
            settings.subtitles == SubtitleMode::BurnIn,
        );
        // Verschachtelte Sequenzen (transitiv) + ihre Blatt-Medien einsammeln,
        // damit der Worker sie self-contained rekursiv komponieren kann.
        gather_nests(timeline, media, nests, &mut plan);
    }

    // Untertitel für Sidecar/Einbetten: sichtbare Spuren (U1 zuerst), Cues
    // auf den Exportbereich beschnitten und auf ihn bezogen (t=0 = Beginn).
    if matches!(settings.subtitles, SubtitleMode::Sidecar | SubtitleMode::Embed) {
        for track in timeline
            .tracks
            .iter()
            .rev()
            .filter(|t| t.kind == TrackKind::Subtitle && !t.muted)
        {
            let cues: Vec<crate::core::subtitle::SrtCue> = timeline
                .subtitle_cues(&track.id)
                .into_iter()
                .filter_map(|c| {
                    let s = c.start.max(start);
                    let e = c.end.min(end);
                    (e - s > 1e-9).then_some(crate::core::subtitle::SrtCue {
                        start: s - start,
                        end: e - start,
                        text: c.text,
                    })
                })
                .collect();
            if !cues.is_empty() {
                plan.subtitle_tracks.push(SubtitlePlanTrack {
                    name: crate::core::timeline::track_name(track, &timeline.tracks),
                    cues,
                });
            }
        }
    }

    if settings.audio.is_some() {
        // Stems-Export: jede Audiospur als eigener Stream/Stem. Erzwingt die
        // getrennte Bus-Verarbeitung für ALLE Spuren (kein Schnellpfad-Master),
        // damit jede Spur einzeln ausgegeben werden kann; die Summe der Stems
        // ergibt exakt den Master-Mix.
        let stems = stems_enabled(settings);
        // Auto-Ducking braucht den Sidechain-Key (Summe der anderen Spuren) auf
        // CLIP-GAIN-Ebene (vor Spur-Fader/Master) — exakt wie der Player. Der
        // Schnellpfad bäckt aber Master×Spur×Pan in die Clip-Gains ein. Sobald
        // also IRGENDEINE Spur duckt, müssen ALLE Spuren über die Bus-
        // Verarbeitung laufen (Clip-Gain bleibt roh) ⇒ Key formelgleich.
        let any_ducking = timeline.tracks.iter().any(|t| {
            t.kind == TrackKind::Audio
                && !t.muted
                && (!solo_any || t.solo)
                && t
                    .effects
                    .iter()
                    .any(|e| e.enabled && e.kind == effects::EffectKind::Ducking)
        });
        for track in timeline
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Audio && !t.muted && (!solo_any || t.solo))
        {
            // Spuren mit Bus-FX oder Automation brauchen die getrennte Bus-
            // Verarbeitung (Effekte/Automation wirken auf die Spur-SUMME);
            // alle anderen laufen über den Schnellpfad mit fertig
            // eingebackenen Gains. Im Stems-Modus UND bei aktivem Ducking geht
            // IMMER jede Spur durch die Bus-Verarbeitung.
            let processed =
                stems || any_ducking || track.has_audio_effects() || track.has_automation();
            let track_gain = db_to_linear(track.gain_db);
            let (pan_l, pan_r) = pan_gains(track.pan);
            let mut track_clips: Vec<AudioClipPlan> = Vec::new();
            for clip in timeline
                .clips
                .iter()
                .filter(|c| c.track_id == track.id && c.enabled)
            {
                // Rückwärts-Clips sind (vorerst) stumm, Standbilder ohnehin,
                // ebenso Time-Remap (variables Tempo) — identisch zur Wiedergabe.
                if clip.reverse || clip.freeze || clip.is_time_remapped() {
                    continue;
                }
                // Verschachtelte Sequenz: das innere Audio rekursiv einflachen
                // (zeitverschoben, Gains gefaltet). Bypasst die Bus-FX der
                // äußeren Spur (v1) → direkt in den Schnellpfad-Master.
                if let Some(inner_id) = &clip.nest_seq {
                    if let Some(inner) = nests.nested_timeline(inner_id) {
                        let lo = clip.start.max(start);
                        let hi = clip.end().min(end);
                        if hi - lo > 1e-9 {
                            let cg = db_to_linear(clip.gain_db);
                            if stems {
                                // Stems: inneres Audio nur mit dem Nest-Clip-Gain
                                // (mono) einflachen — Spur-Gain/Pan + Master folgen
                                // in der Bus-Verarbeitung, damit es im Stem DIESER
                                // Spur landet (Stem-Summe = Master).
                                flatten_nest_audio(
                                    inner,
                                    media,
                                    nests,
                                    clip.media_time_at(lo).max(0.0),
                                    hi - lo,
                                    lo - start,
                                    cg,
                                    cg,
                                    1,
                                    &mut track_clips,
                                );
                            } else {
                                let gl = master * track_gain * pan_l * cg;
                                let gr = master * track_gain * pan_r * cg;
                                flatten_nest_audio(
                                    inner,
                                    media,
                                    nests,
                                    clip.media_time_at(lo).max(0.0),
                                    hi - lo,
                                    lo - start,
                                    gl,
                                    gr,
                                    1,
                                    &mut plan.audio,
                                );
                            }
                        }
                    }
                    continue;
                }
                // Audio-Crossfades verlängern den hörbaren Bereich über die
                // Clipkanten hinaus (Handles im Modell garantiert).
                let fades = timeline.audio_fades(clip);
                let (ext0, ext1) = timeline.audio_extent(clip, &fades);
                let clip_start = ext0.max(start);
                let clip_end = ext1.min(end);
                if clip_end - clip_start <= 1e-9 {
                    continue;
                }
                // Multicam: Audio-Winkel-Asset auflösen (Original; Medienzeit um
                // die Winkel-`pos` versetzt). Sonst das Clip-Asset.
                let (asset, audio_pos) = if let Some(mc) = &clip.multicam {
                    let Some(src) = nests
                        .nested_timeline(&mc.source)
                        .and_then(|t| t.multicam.as_ref())
                    else {
                        continue;
                    };
                    let aidx = src.audio_angle_idx(mc.angle);
                    let Some(angle) = src.angles.get(aidx).filter(|a| a.has_audio) else {
                        continue;
                    };
                    let Some(asset) = media.asset(&angle.asset_id) else {
                        continue;
                    };
                    (asset, angle.pos)
                } else {
                    let Some(asset) = media.asset(&clip.asset_id) else {
                        continue;
                    };
                    (asset, 0.0)
                };
                if asset.offline || asset.info.audio.is_empty() {
                    continue;
                }
                // Schnellpfad: Master × Spur × Clip × Balance eingebacken.
                // Bus-Pfad: nur Clip-Gain (mono) — Spur/Pan/Master folgen in
                // der Bus-Verarbeitung.
                let (gl, gr) = if processed {
                    let g = db_to_linear(clip.gain_db);
                    (g, g)
                } else {
                    let gain = master * track_gain * db_to_linear(clip.gain_db);
                    (gain * pan_l, gain * pan_r)
                };
                let cp = AudioClipPlan {
                    path: asset.path.clone(),
                    start_in_mix: clip_start - start,
                    duration: clip_end - clip_start,
                    // Medienzeit am Mix-Beginn (zentrale Abbildung, vorwärts);
                    // bei Multicam um die Audio-Winkel-`pos` versetzt.
                    src_in: (clip.media_time_at(clip_start) - audio_pos).max(0.0),
                    speed: clip.eff_speed(),
                    gain_l: gl,
                    gain_r: gr,
                    volume: clip.fx.volume_db.clone(),
                    effects: clip
                        .effects
                        .iter()
                        .filter(|e| e.kind.is_audio())
                        .cloned()
                        .collect(),
                    fades: fades
                        .iter()
                        .map(|(w0, w1, fade_in, equal_power)| PlanAudioFade {
                            t0: w0 - start,
                            t1: w1 - start,
                            fade_in: *fade_in,
                            equal_power: *equal_power,
                        })
                        .collect(),
                };
                if processed {
                    track_clips.push(cp);
                } else {
                    plan.audio.push(cp);
                }
            }
            if processed && !track_clips.is_empty() {
                track_clips.sort_by(|a, b| a.start_in_mix.total_cmp(&b.start_in_mix));
                plan.audio_tracks.push(AudioTrackPlan {
                    name: track_name(track, &timeline.tracks),
                    clips: track_clips,
                    effects: track
                        .effects
                        .iter()
                        .filter(|e| e.kind.is_audio())
                        .cloned()
                        .collect(),
                    volume_auto: track.volume_auto.clone(),
                    pan_auto: track.pan_auto.clone(),
                    gain_db: track.gain_db,
                    pan: track.pan,
                    master_db: timeline.master_gain_db,
                    seq_start: start,
                });
            }
        }
        plan.audio
            .sort_by(|a, b| a.start_in_mix.total_cmp(&b.start_in_mix));
    }

    plan
}

/// Video-Segmente: Zeitachse in Ziel-Frames quantisieren, an jeder
/// Clip-Grenze schneiden; je Abschnitt der komplette Layer-Stapel
/// (unten → oben) — der Renderer komponiert wie der Programmmonitor.
#[allow(clippy::too_many_arguments)]
pub(crate) fn plan_video_segments(
    timeline: &TimelineStore,
    media: &MediaStore,
    nests: &dyn compose::NestResolver,
    range_start: f64,
    total_frames: u64,
    fps: f64,
    solo_any: bool,
    burn_subtitles: bool,
) -> Vec<VideoSegment> {
    struct Candidate {
        /// 0 = unterste sichtbare Videospur (Zeichenreihenfolge).
        draw_order: usize,
        /// Innerhalb der Spur: Farbflächen (Dips) über den Clips.
        is_solid: bool,
        f0: u64,
        f1: u64,
        clip_id: String,
        clip_start: f64,
        clip_duration: f64,
        src_in: f64,
        /// Medienfortschritt pro Sequenzsekunde (signiert; 0 = Standbild).
        media_step: f64,
        path: String,
        image: bool,
        fx: ClipFx,
        grade: ColorGrade,
        effects: Vec<EffectInstance>,
        natural_w: u32,
        natural_h: u32,
        src_bit_depth: u32,
        transitions: Vec<PlanTransition>,
        solid: Option<[u8; 3]>,
        title: Option<crate::core::title::TitleSpec>,
        nest_seq: Option<String>,
        adjustment: bool,
        blend_mode: crate::core::compose::BlendMode,
    }

    let frame_of = |t: f64| -> u64 {
        (((t - range_start) * fps).round().max(0.0) as u64).min(total_frames)
    };

    let mut candidates: Vec<Candidate> = Vec::new();
    // Spur-Index 0 ist die OBERSTE Videospur → rückwärts = Zeichenreihenfolge.
    let video_tracks: Vec<&str> = timeline
        .tracks
        .iter()
        .rev()
        .filter(|t| t.kind == TrackKind::Video && !t.muted && (!solo_any || t.solo))
        .map(|t| t.id.as_str())
        .collect();
    // Sichtbare Untertitel-Spuren (nur beim Einbrennen): zeichnen über
    // ALLEN Videospuren, in derselben Reihenfolge wie der Programmmonitor.
    let subtitle_tracks: Vec<&str> = if burn_subtitles {
        timeline
            .tracks
            .iter()
            .rev()
            .filter(|t| t.kind == TrackKind::Subtitle && !t.muted)
            .map(|t| t.id.as_str())
            .collect()
    } else {
        Vec::new()
    };

    for clip in timeline.clips.iter().filter(|c| c.enabled) {
        // Untertitel-Segment: synthetisierter Titel-Spec aus Spurstil + Text
        // (identischer Rasterizer wie der Programmmonitor), ohne Decoder.
        if clip.is_subtitle() {
            let Some(sub_order) = subtitle_tracks.iter().position(|id| *id == clip.track_id)
            else {
                continue;
            };
            let f0 = frame_of(clip.start);
            let f1 = frame_of(clip.end());
            if f1 <= f0 {
                continue;
            }
            let Some(spec) = compose::layer_title_spec(timeline, clip) else {
                continue;
            };
            candidates.push(Candidate {
                draw_order: video_tracks.len() + sub_order,
                is_solid: false,
                f0,
                f1,
                clip_id: clip.id.clone(),
                clip_start: clip.start,
                clip_duration: clip.duration,
                src_in: clip.src_in,
                media_step: clip.media_step(),
                path: String::new(),
                image: false,
                fx: clip.fx.clone(),
                grade: clip.grade.clone(),
                effects: clip
                    .effects
                    .iter()
                    .filter(|e| !e.kind.is_audio())
                    .cloned()
                    .collect(),
                natural_w: 0,
                natural_h: 0,
                src_bit_depth: 8,
                transitions: Vec::new(),
                solid: None,
                title: Some(spec),
                nest_seq: None,
                adjustment: false,
                blend_mode: clip.blend_mode,
            });
            continue;
        }
        let Some(order) = video_tracks.iter().position(|id| *id == clip.track_id) else {
            continue;
        };
        let f0 = frame_of(clip.start);
        let f1 = frame_of(clip.end());
        if f1 <= f0 {
            continue;
        }
        // Adjustment Layer: kein Asset/Decoder — der Renderer wendet `effects`
        // → `grade` als Pass auf das zusammengesetzte Canvas der Spuren darunter
        // an (an seiner Position in der Zeichenreihenfolge).
        if clip.is_adjustment() {
            candidates.push(Candidate {
                draw_order: order,
                is_solid: false,
                f0,
                f1,
                clip_id: clip.id.clone(),
                clip_start: clip.start,
                clip_duration: clip.duration,
                src_in: clip.src_in,
                media_step: clip.media_step(),
                path: String::new(),
                image: false,
                fx: clip.fx.clone(),
                grade: clip.grade.clone(),
                effects: clip
                    .effects
                    .iter()
                    .filter(|e| !e.kind.is_audio())
                    .cloned()
                    .collect(),
                natural_w: 0,
                natural_h: 0,
                src_bit_depth: 8,
                transitions: Vec::new(),
                solid: None,
                title: None,
                nest_seq: None,
                adjustment: true,
                blend_mode: clip.blend_mode,
            });
            continue;
        }
        // Titel-Generator: kein Asset/Decoder — der Renderer rastert den
        // Spec selbst (identischer Rasterizer wie der Programmmonitor).
        if let Some(spec) = &clip.title {
            candidates.push(Candidate {
                draw_order: order,
                is_solid: false,
                f0,
                f1,
                clip_id: clip.id.clone(),
                clip_start: clip.start,
                clip_duration: clip.duration,
                src_in: clip.src_in,
                media_step: clip.media_step(),
                path: String::new(),
                image: false,
                fx: clip.fx.clone(),
                grade: clip.grade.clone(),
                effects: clip
                    .effects
                    .iter()
                    .filter(|e| !e.kind.is_audio())
                    .cloned()
                    .collect(),
                natural_w: 0,
                natural_h: 0,
                src_bit_depth: 8,
                transitions: Vec::new(),
                solid: None,
                title: Some(spec.clone()),
                nest_seq: None,
                adjustment: false,
                blend_mode: clip.blend_mode,
            });
            continue;
        }
        // Verschachtelte Sequenz: kein Asset/Decoder — der Renderer komponiert
        // die innere Sequenz rekursiv (Auflösung = innere Sequenzgröße, damit
        // die Skalierung im äußeren Frame stimmt).
        if let Some(inner_id) = &clip.nest_seq {
            let Some(inner) = nests.nested_timeline(inner_id) else {
                continue;
            };
            let (nw, nh) = (inner.settings.width, inner.settings.height);
            candidates.push(Candidate {
                draw_order: order,
                is_solid: false,
                f0,
                f1,
                clip_id: clip.id.clone(),
                clip_start: clip.start,
                clip_duration: clip.duration,
                src_in: clip.src_in,
                media_step: clip.media_step(),
                path: String::new(),
                image: false,
                fx: clip.fx.clone(),
                grade: clip.grade.clone(),
                effects: clip
                    .effects
                    .iter()
                    .filter(|e| !e.kind.is_audio())
                    .cloned()
                    .collect(),
                natural_w: nw,
                natural_h: nh,
                src_bit_depth: 8,
                transitions: Vec::new(),
                solid: None,
                title: None,
                nest_seq: Some(inner_id.clone()),
                adjustment: false,
                blend_mode: clip.blend_mode,
            });
            continue;
        }
        // Multicam: aktiven Winkel zu einem normalen Medien-Blatt auflösen
        // (Asset = Winkel-Original, Medienzeit = gemeinsame Zeit − Winkel-pos).
        // Der EXPORT nutzt — wie überall — das ORIGINAL, nie den Proxy.
        if let Some(mc) = &clip.multicam {
            let Some(angle) = nests
                .nested_timeline(&mc.source)
                .and_then(|t| t.multicam.as_ref())
                .and_then(|s| s.angle(mc.angle))
            else {
                continue;
            };
            let Some(asset) = media.asset(&angle.asset_id) else {
                continue;
            };
            if asset.offline || asset.kind == MediaKind::Audio {
                continue;
            }
            let image = asset.kind == MediaKind::Image;
            if !image && asset.info.video.is_empty() {
                continue;
            }
            let (natural_w, natural_h, src_bit_depth) = asset
                .info
                .video
                .first()
                .map(|v| (v.width, v.height, v.bit_depth))
                .unwrap_or((0, 0, 8));
            candidates.push(Candidate {
                draw_order: order,
                is_solid: false,
                f0,
                f1,
                clip_id: clip.id.clone(),
                clip_start: clip.start,
                clip_duration: clip.duration,
                src_in: clip.src_in - angle.pos,
                media_step: clip.media_step(),
                path: asset.path.clone(),
                image,
                fx: clip.fx.clone(),
                grade: clip.grade.clone(),
                effects: clip
                    .effects
                    .iter()
                    .filter(|e| !e.kind.is_audio())
                    .cloned()
                    .collect(),
                natural_w,
                natural_h,
                src_bit_depth,
                transitions: Vec::new(),
                solid: None,
                title: None,
                nest_seq: None,
                adjustment: false,
                blend_mode: clip.blend_mode,
            });
            continue;
        }
        let Some(asset) = media.asset(&clip.asset_id) else {
            continue;
        };
        if asset.offline || asset.kind == MediaKind::Audio {
            continue;
        }
        let image = asset.kind == MediaKind::Image;
        if !image && asset.info.video.is_empty() {
            continue;
        }
        let (natural_w, natural_h, src_bit_depth) = asset
            .info
            .video
            .first()
            .map(|v| (v.width, v.height, v.bit_depth))
            .unwrap_or((0, 0, 8));
        candidates.push(Candidate {
            draw_order: order,
            is_solid: false,
            f0,
            f1,
            clip_id: clip.id.clone(),
            clip_start: clip.start,
            clip_duration: clip.duration,
            src_in: clip.src_in,
            media_step: clip.media_step(),
            path: asset.path.clone(),
            image,
            fx: clip.fx.clone(),
            grade: clip.grade.clone(),
            effects: clip
                .effects
                .iter()
                .filter(|e| !e.kind.is_audio())
                .cloned()
                .collect(),
            natural_w,
            natural_h,
            src_bit_depth,
            transitions: Vec::new(),
            solid: None,
            title: None,
            nest_seq: None,
            adjustment: false,
            blend_mode: clip.blend_mode,
        });
    }

    // Übergänge: Kandidaten über die Schnittkante hinaus verlängern (zwei
    // Decoder laufen im Fenster parallel), Fenster anheften, Dips als
    // Farbflächen-Kandidaten einplanen. Fensterkanten werden zusätzliche
    // Segmentgrenzen, damit Abschnitte ohne Übergang den Schnellpfad behalten.
    let mut extra_bounds: Vec<u64> = Vec::new();
    for tr in &timeline.transitions {
        if tr.kind.is_audio() {
            continue;
        }
        let (from, to) = transitions::resolve_clips(&timeline.clips, tr);
        let Some(anchor) = from.or(to) else { continue };
        let Some(track_order) = video_tracks.iter().position(|id| *id == anchor.track_id) else {
            continue;
        };
        let Some((w0, w1)) = transitions::window(from, to, tr.alignment, tr.duration) else {
            continue;
        };
        let (wf0, wf1) = (frame_of(w0), frame_of(w1));
        if wf1 <= wf0 {
            continue;
        }
        extra_bounds.push(wf0);
        extra_bounds.push(wf1);
        let two_sided = from.is_some() && to.is_some();
        let (t0, t1) = (w0 - range_start, w1 - range_start);
        if let Some(f) = from {
            if let Some(c) = candidates.iter_mut().find(|c| c.clip_id == f.id) {
                c.f1 = c.f1.max(wf1);
                c.transitions.push(PlanTransition {
                    kind: tr.kind,
                    direction: tr.direction,
                    role: if two_sided { TransitionRole::Out } else { TransitionRole::OutSolo },
                    t0,
                    t1,
                });
            }
        }
        if let Some(t) = to {
            if let Some(c) = candidates.iter_mut().find(|c| c.clip_id == t.id) {
                c.f0 = c.f0.min(wf0);
                c.transitions.push(PlanTransition {
                    kind: tr.kind,
                    direction: tr.direction,
                    role: if two_sided { TransitionRole::In } else { TransitionRole::InSolo },
                    t0,
                    t1,
                });
            }
        }
        if tr.kind.is_dip() {
            let role = if two_sided {
                TransitionRole::Dip
            } else if from.is_some() {
                TransitionRole::DipOut
            } else {
                TransitionRole::DipIn
            };
            let color = if tr.kind == TransitionKind::DipToWhite {
                [255u8, 255, 255]
            } else {
                [0u8, 0, 0]
            };
            candidates.push(Candidate {
                draw_order: track_order,
                is_solid: true,
                f0: wf0,
                f1: wf1,
                clip_id: format!("solid:{}", tr.id),
                clip_start: w0,
                clip_duration: w1 - w0,
                src_in: 0.0,
                media_step: 1.0,
                path: String::new(),
                image: false,
                fx: ClipFx::default(),
                grade: ColorGrade::default(),
                effects: Vec::new(),
                natural_w: 0,
                natural_h: 0,
                src_bit_depth: 8,
                transitions: vec![PlanTransition {
                    kind: tr.kind,
                    direction: tr.direction,
                    role,
                    t0,
                    t1,
                }],
                solid: Some(color),
                title: None,
                nest_seq: None,
                adjustment: false,
                blend_mode: crate::core::compose::BlendMode::Normal,
            });
        }
    }

    // Time-Remap (variable Geschwindigkeit): die Medienzeit ist NICHT linear in
    // der Sequenzzeit, also kann kein konstanter setpts-Faktor das ganze Segment
    // abbilden. Lösung ohne Decoder-Umbau: je betroffenem Clip an JEDER
    // Frame-Kante eine Segmentgrenze setzen ⇒ jedes Segment ist ein Frame, dessen
    // `src_in` exakt = `media_time_at(seq_t)` (Integral der Kurve). Der Worker
    // seekt pro Segment auf `src_in` und decodiert einen Frame — frame-genau,
    // wenn auch mit einem Decoder-Start je Frame (offline vertretbar).
    struct RemapSampler {
        speed: AnimatedParam,
        reverse: bool,
        duration: f64,
    }
    impl RemapSampler {
        /// Exakte Medienzeit zur Sequenzzeit `seq_t` (gleiche Formel wie
        /// `TimelineClip::media_time_at`, src_in vom Kandidaten übergeben —
        /// trägt damit auch den Multicam-Winkel-Offset `−pos`).
        fn media_at(&self, clip_start: f64, src_in: f64, seq_t: f64) -> f64 {
            let off = self.speed.integral(0.0, seq_t - clip_start);
            if self.reverse {
                let span = self.speed.integral(0.0, self.duration);
                src_in + span - off
            } else {
                src_in + off
            }
        }
        /// Instantaner, signierter Medienfortschritt zur Sequenzzeit `seq_t`.
        /// Hält die Segment-Koaleszenz korrekt: verschmilzt nur dort, wo das
        /// Tempo lokal konstant ist (gleicher Step + lineare Erwartung trifft).
        fn step_at(&self, clip_start: f64, seq_t: f64) -> f64 {
            let s = crate::core::timeline::clamp_speed(self.speed.eval(seq_t - clip_start));
            if self.reverse {
                -s
            } else {
                s
            }
        }
    }
    let remap_by_clip: std::collections::HashMap<String, RemapSampler> = timeline
        .clips
        .iter()
        .filter(|c| c.is_time_remapped())
        .map(|c| {
            (
                c.id.clone(),
                RemapSampler {
                    speed: c.clamped_speed(),
                    reverse: c.reverse,
                    duration: c.duration,
                },
            )
        })
        .collect();

    // Schnittpunkte (in Frames) sammeln.
    let mut bounds: Vec<u64> = vec![0, total_frames];
    for c in &candidates {
        bounds.push(c.f0);
        bounds.push(c.f1);
        // Time-Remap-Kandidaten: jede Frame-Kante wird Segmentgrenze.
        if remap_by_clip.contains_key(&c.clip_id) {
            for f in c.f0..c.f1 {
                bounds.push(f);
            }
        }
    }
    bounds.extend(extra_bounds);
    bounds.sort_unstable();
    bounds.dedup();

    let mut segments: Vec<VideoSegment> = Vec::new();
    for pair in bounds.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if b <= a {
            continue;
        }
        let (seg_t0, seg_t1) = (a as f64 / fps, b as f64 / fps);
        let mut active: Vec<&Candidate> = candidates
            .iter()
            .filter(|c| c.f0 <= a && c.f1 >= b)
            .collect();
        // Zeichenreihenfolge: Spur, darin Clips vor Farbflächen; während
        // eines Übergangs liegt der später startende Clip OBEN.
        active.sort_by(|x, y| {
            x.draw_order
                .cmp(&y.draw_order)
                .then(x.is_solid.cmp(&y.is_solid))
                .then(x.clip_start.total_cmp(&y.clip_start))
        });
        let layers: Vec<VideoLayerPlan> = active
            .iter()
            .map(|c| VideoLayerPlan {
                clip_id: c.clip_id.clone(),
                path: c.path.clone(),
                image: c.image,
                // Medienzeit des Segmentbeginns aus der Sequenzzeit ableiten
                // (identische Formel wie `TimelineClip::media_time_at` —
                // rückwärts läuft die Spanne vom Medien-Out abwärts).
                src_in: {
                    let seq_t = range_start + a as f64 / fps;
                    let m = if let Some(rm) = remap_by_clip.get(&c.clip_id) {
                        // Time-Remap: Medienzeit = Integral der Speed-Kurve
                        // (jedes Segment ist hier ein Frame).
                        rm.media_at(c.clip_start, c.src_in, seq_t)
                    } else if c.media_step == 0.0 {
                        c.src_in
                    } else if c.media_step < 0.0 {
                        c.src_in + (c.clip_start + c.clip_duration - seq_t) * (-c.media_step)
                    } else {
                        c.src_in + (seq_t - c.clip_start) * c.media_step
                    };
                    m.max(0.0)
                },
                media_step: match remap_by_clip.get(&c.clip_id) {
                    Some(rm) => rm.step_at(c.clip_start, range_start + a as f64 / fps),
                    None => c.media_step,
                },
                fx: c.fx.clone(),
                grade: c.grade.clone(),
                effects: c.effects.clone(),
                natural_w: c.natural_w,
                natural_h: c.natural_h,
                src_bit_depth: c.src_bit_depth,
                // Nur Fenster, die dieses Segment berühren — Abschnitte
                // außerhalb behalten den Schnellpfad und dürfen verschmelzen.
                transitions: c
                    .transitions
                    .iter()
                    .filter(|t| t.t1 > seg_t0 && t.t0 < seg_t1)
                    .cloned()
                    .collect(),
                solid: c.solid,
                title: c.title.clone(),
                nest_seq: c.nest_seq.clone(),
                adjustment: c.adjustment,
                blend_mode: c.blend_mode,
            })
            .collect();
        // Fortsetzungen desselben Layer-Stapels verschmelzen (spart
        // Decoder-Starts): gleiche Clips in gleicher Reihenfolge, die
        // Medienzeit jedes Video-Layers läuft nahtlos weiter und die
        // Übergangs-Fenster sind identisch.
        let frames = b - a;
        if let Some(last) = segments.last_mut() {
            let merge = last.layers.len() == layers.len()
                && last
                    .layers
                    .iter()
                    .zip(layers.iter())
                    .all(|(l1, l2)| {
                        let expected = l1.src_in + last.frames as f64 / fps * l1.media_step;
                        let tol = 0.5 / fps * l1.media_step.abs().max(1.0);
                        let continues = l1.media_step == l2.media_step
                            && (expected - l2.src_in).abs() < tol;
                        l1.clip_id == l2.clip_id
                            && (l2.image || l2.solid.is_some() || continues)
                            && l1.transitions == l2.transitions
                            && l1.solid == l2.solid
                    });
            if merge {
                last.frames += frames;
                continue;
            }
        }
        segments.push(VideoSegment { frames, layers });
    }
    segments
}

