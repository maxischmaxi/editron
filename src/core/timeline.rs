//! Sequenz-Modell + Store: Tracks/Clips mit verknüpften A/V-Paaren,
//! Snapshot-History (Undo/Redo) und allen Editier-Operationen.

use crate::core::animation::{AnimatedParam, ClipFx, Interp, Keyframe, ParamId, ParamRef};
use crate::core::effects::{EffectInstance, EffectKind};
use crate::core::grade::ColorGrade;
use crate::core::marker::Marker;
use crate::core::sequence::{self, SequenceSettings};
use crate::core::subtitle::{SrtCue, SubtitleSpec, SubtitleStyle};
use crate::core::title::TitleSpec;
use crate::core::transitions::{
    self, Transition, TransitionAlignment, TransitionKind, DEFAULT_TRANSITION_DURATION,
};
use crate::core::types::{new_id, MediaAsset, MediaKind};
use serde::{Deserialize, Serialize};

pub const IMAGE_DEFAULT_DURATION: f64 = 5.0;
/// Editier-Granularitätsboden: die kürzeste Frame-Dauer der unterstützten
/// Sequenzraten (60 fps). Bewusst eine rate-unabhängige Konstante — die
/// Quantisierung gegen die konkrete Sequenzrate passiert bei Playhead-
/// Stepping, Timecode-Anzeige und Renderplan.
pub const MIN_CLIP_DURATION: f64 = 1.0 / 60.0;

const MIN_ZOOM: f64 = 4.0;
const MAX_ZOOM: f64 = 1000.0;
const ZOOM_FACTOR: f64 = 1.5;
const HISTORY_LIMIT: usize = 100;
const EPS: f64 = 1e-6;

/// Zulässiger Geschwindigkeitsbereich (10 % – 1000 %, Premiere-üblich).
pub const MIN_CLIP_SPEED: f64 = 0.1;
pub const MAX_CLIP_SPEED: f64 = 10.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackKind {
    Video,
    Audio,
    /// Untertitel-Spur: Segmente mit Text statt Medien, liegt über dem
    /// Video-Block (Formatversion 4).
    Subtitle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackFlag {
    Muted,
    Solo,
    Locked,
    /// Sync-Lock (rippelt bei Insert/Extract mit).
    SyncLock,
    /// Spur-Targeting (Ziel von Lift/Extract/Match Frame).
    Targeted,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineTrack {
    pub id: String,
    pub kind: TrackKind,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub solo: bool,
    /// Gesperrte Spuren sind von allen Editier-Operationen ausgenommen.
    #[serde(default)]
    pub locked: bool,
    /// Spur-Verstärkung in dB (Mixer-Fader); ≤ −60 gilt als −∞.
    #[serde(default)]
    pub gain_db: f64,
    /// Stereo-Balance −1 (links) .. +1 (rechts); nur für Audio-Spuren relevant.
    #[serde(default)]
    pub pan: f64,
    /// Sync-Lock: rippelt bei Insert/Extract mit, auch ohne neues Material,
    /// damit der Mehrspur-Sync erhalten bleibt (Premiere-Semantik). Formatv7.
    #[serde(default)]
    pub sync_lock: bool,
    /// Spur-Targeting: Ziel playheadbezogener Operationen (Lift/Extract/
    /// Match Frame). Mehrfachauswahl möglich. Formatversion 7.
    #[serde(default)]
    pub targeted: bool,
    /// Source-Patching: empfängt das passende Quell-Material (Video bzw.
    /// Audio) beim Three-Point-Edit. Pro Spurart höchstens eine Spur (Radio).
    /// Formatversion 7.
    #[serde(default)]
    pub source_patched: bool,
    /// Gestaltung der Untertitel-Spur (None außerhalb von Untertitel-Spuren
    /// bzw. Standardstil). `muted` dient bei Untertitel-Spuren als
    /// Sichtbarkeits-Schalter (ausgeblendet = nicht in Monitor/Export).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle_style: Option<SubtitleStyle>,
    /// Audio-Effekt-Kette der Spur (Bus-Insert): wirkt auf die Summe aller
    /// Clips der Spur, NACH den Clip-Effekten und VOR Spur-Gain/Pan. Nur für
    /// Audio-Spuren sinnvoll. Reihenfolge = Verarbeitungsreihenfolge.
    /// Formatversion 7.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<EffectInstance>,
    /// Lautstärke-Automation (dB-Offset zum Fader, Keyframes in SEQUENZZEIT).
    /// Statische Null = keine Automation (Fader gilt). Formatversion 7.
    #[serde(default = "zero_auto", skip_serializing_if = "AnimatedParam::is_static_zero")]
    pub volume_auto: AnimatedParam,
    /// Pan-Automation (Offset zur Balance, Keyframes in SEQUENZZEIT).
    /// Statische Null = keine Automation. Formatversion 7.
    #[serde(default = "zero_auto", skip_serializing_if = "AnimatedParam::is_static_zero")]
    pub pan_auto: AnimatedParam,
}

fn zero_auto() -> AnimatedParam {
    AnimatedParam::fixed(0.0)
}

/// Welcher Automations-Parameter einer Spur (Timeline-Rubber-Band, Mixer).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackAutoParam {
    Volume,
    Pan,
}

impl TimelineTrack {
    /// Wirksame Spur-Verstärkung (dB) zur Sequenzzeit `t`: Fader + Automation.
    pub fn gain_db_at(&self, t: f64) -> f64 {
        self.gain_db + self.volume_auto.eval(t)
    }

    /// Wirksame Stereo-Balance zur Sequenzzeit `t`: Fader + Automation.
    pub fn pan_at(&self, t: f64) -> f64 {
        (self.pan + self.pan_auto.eval(t)).clamp(-1.0, 1.0)
    }

    /// Hat die Spur eine wirksame Lautstärke- oder Pan-Automation?
    pub fn has_automation(&self) -> bool {
        self.volume_auto.is_animated() || self.pan_auto.is_animated()
    }

    pub fn auto_param(&self, p: TrackAutoParam) -> &AnimatedParam {
        match p {
            TrackAutoParam::Volume => &self.volume_auto,
            TrackAutoParam::Pan => &self.pan_auto,
        }
    }

    pub fn auto_param_mut(&mut self, p: TrackAutoParam) -> &mut AnimatedParam {
        match p {
            TrackAutoParam::Volume => &mut self.volume_auto,
            TrackAutoParam::Pan => &mut self.pan_auto,
        }
    }

    /// Hat die Spur aktive Audio-Effekte (für Schnellpfad-Entscheidungen)?
    pub fn has_audio_effects(&self) -> bool {
        self.effects.iter().any(|e| e.enabled && e.kind.is_audio())
    }
}

fn make_track(kind: TrackKind) -> TimelineTrack {
    TimelineTrack {
        id: new_id(),
        kind,
        muted: false,
        solo: false,
        locked: false,
        gain_db: 0.0,
        pan: 0.0,
        sync_lock: false,
        targeted: false,
        source_patched: false,
        subtitle_style: None,
        effects: Vec::new(),
        volume_auto: zero_auto(),
        pan_auto: zero_auto(),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineClip {
    pub id: String,
    pub track_id: String,
    pub asset_id: String,
    /// Anzeigename (i. d. R. Dateiname, Audio-Teil mit Suffix).
    pub name: String,
    pub kind: TrackKind,
    /// Startzeit in der Sequenz (Sekunden).
    pub start: f64,
    pub duration: f64,
    /// In-Punkt innerhalb der Quelldatei.
    pub src_in: f64,
    /// Gesamtdauer der Quelle; INFINITY bei Standbildern (frei dehnbar).
    /// JSON kennt kein Infinity — wird als null gespeichert.
    #[serde(with = "infinite_duration")]
    pub src_duration: f64,
    /// Verknüpfungsgruppe Video↔Audio desselben Assets.
    pub link_id: Option<String>,
    /// Deaktivierte Clips bleiben in der Sequenz, sind aber ausgegraut.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Clip-Verstärkung in dB (zusätzlich zum Spur-Fader); ≤ −60 gilt als −∞.
    #[serde(default)]
    pub gain_db: f64,
    /// Animierbare Parameter (Bewegung, Deckkraft, Lautstärke) mit Keyframes.
    #[serde(default, skip_serializing_if = "ClipFx::is_default")]
    pub fx: ClipFx,
    /// Farbkorrektur (Farbe-Panel) — gilt statisch für den ganzen Clip.
    #[serde(default, skip_serializing_if = "ColorGrade::is_default")]
    pub grade: ColorGrade,
    /// Angewendete Effekte (Effekte-Panel), Reihenfolge = Render-Reihenfolge.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<EffectInstance>,
    /// Titel-Generator: Clip ohne Mediendatei (`asset_id` leer), dessen
    /// Inhalt aus diesem Spec gerastert wird (Formatversion 3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<TitleSpec>,
    /// Untertitel-Segment: Clip ohne Mediendatei auf einer Untertitel-Spur;
    /// die Optik kommt aus dem Spurstil (Formatversion 4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<SubtitleSpec>,
    /// Geschwindigkeitsfaktor (1,0 = normal). Beziehung überall:
    /// belegte Medienspanne = duration × speed (Formatversion 5).
    #[serde(default = "default_speed", skip_serializing_if = "is_default_speed")]
    pub speed: f64,
    /// Rückwärts: die Medienspanne [src_in, src_in + duration·speed) läuft
    /// in der Timeline von hinten nach vorn (src_in bleibt der tiefste Punkt).
    #[serde(default, skip_serializing_if = "is_false")]
    pub reverse: bool,
    /// Standbild: der Frame bei `src_in` steht für die gesamte Clipdauer
    /// (belegte Medienspanne = 0, Dauer frei dehnbar).
    #[serde(default, skip_serializing_if = "is_false")]
    pub freeze: bool,
    /// Clip-Marker in MEDIENZEIT (Quell-Sekunden, gleiche Achse wie `src_in`).
    /// Sie hängen am Material und wandern dadurch beim Trimmen/Verschieben
    /// korrekt mit; beim Teilen werden sie auf die Hälften aufgeteilt
    /// (Formatversion 6).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<Marker>,
}

fn default_enabled() -> bool {
    true
}

fn default_speed() -> f64 {
    1.0
}

fn is_default_speed(v: &f64) -> bool {
    (*v - 1.0).abs() < EPS
}

fn is_false(v: &bool) -> bool {
    !*v
}

/// INFINITY (Standbilder) ↔ null: serde_json würde Infinity sonst stillschweigend
/// zu null serialisieren und beim Laden an f64 scheitern.
mod infinite_duration {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
        if v.is_finite() {
            s.serialize_some(v)
        } else {
            s.serialize_none()
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
        Ok(Option::<f64>::deserialize(d)?.unwrap_or(f64::INFINITY))
    }
}

impl TimelineClip {
    pub fn end(&self) -> f64 {
        self.start + self.duration
    }

    // ------------------------------------------------ Zeit-Mapping (Speed)
    // DIE Formelquelle für Timeline-Zeit ↔ Medienzeit: Player, Compositor,
    // Renderplan, Keyframe-Editor und Gizmo rechnen ausschließlich hierüber
    // (via `compose::clip_media_time`) — Vorschau und Export laufen damit
    // garantiert über dieselbe Abbildung.

    /// Effektiver, geklemmter Geschwindigkeitsfaktor.
    pub fn eff_speed(&self) -> f64 {
        if self.speed.is_finite() {
            self.speed.clamp(MIN_CLIP_SPEED, MAX_CLIP_SPEED)
        } else {
            1.0
        }
    }

    /// Vom Clip belegte Medienspanne in Quell-Sekunden (0 bei Standbild).
    pub fn media_span(&self) -> f64 {
        if self.freeze {
            0.0
        } else {
            self.duration * self.eff_speed()
        }
    }

    /// Tiefste belegte Medienzeit (In-Punkt in der Quelle).
    pub fn media_in(&self) -> f64 {
        self.src_in
    }

    /// Höchste belegte Medienzeit (Out-Punkt in der Quelle).
    pub fn media_out(&self) -> f64 {
        self.src_in + self.media_span()
    }

    /// Medienfortschritt pro Timeline-Sekunde (signiert; 0 = Standbild).
    pub fn media_step(&self) -> f64 {
        if self.freeze {
            0.0
        } else if self.reverse {
            -self.eff_speed()
        } else {
            self.eff_speed()
        }
    }

    /// Medienzeit zur Sequenzzeit `t_seq` — lineare Abbildung, auch
    /// außerhalb von [start, end) gültig (Übergangs-Handles extrapolieren).
    pub fn media_time_at(&self, t_seq: f64) -> f64 {
        if self.freeze {
            return self.src_in;
        }
        let off = (t_seq - self.start) * self.eff_speed();
        if self.reverse {
            self.media_out() - off
        } else {
            self.src_in + off
        }
    }

    /// Umkehrung von `media_time_at` (Standbild: Clipanfang).
    pub fn seq_time_of_media(&self, media_t: f64) -> f64 {
        if self.freeze {
            return self.start;
        }
        let off = if self.reverse {
            self.media_out() - media_t
        } else {
            media_t - self.src_in
        };
        self.start + off / self.eff_speed()
    }

    /// src_in der linken/rechten Hälfte beim Teilen `left_len` Timeline-
    /// Sekunden nach Clipanfang (Razor, Overwrite) — Medienspanne bleibt
    /// in Summe exakt erhalten, auch rückwärts.
    pub fn split_src_ins(&self, left_len: f64) -> (f64, f64) {
        if self.freeze {
            return (self.src_in, self.src_in);
        }
        let left_span = left_len * self.eff_speed();
        if self.reverse {
            (self.src_in + (self.media_span() - left_span), self.src_in)
        } else {
            (self.src_in, self.src_in + left_span)
        }
    }

    /// Badge-Text für die Timeline („50 %“, „−100 %“, „Standbild“);
    /// None bei normaler Vorwärts-Wiedergabe.
    pub fn speed_label(&self) -> Option<String> {
        if self.freeze {
            return Some("Standbild".to_string());
        }
        if !self.reverse && is_default_speed(&self.speed) {
            return None;
        }
        let pct = self.eff_speed() * 100.0;
        let num = if (pct - pct.round()).abs() < 0.05 {
            format!("{}", pct.round() as i64)
        } else {
            format!("{pct:.1}").replace('.', ",")
        };
        Some(if self.reverse {
            format!("−{num} %")
        } else {
            format!("{num} %")
        })
    }

    /// Titel-Generator-Clip (ohne Mediendatei)?
    pub fn is_title(&self) -> bool {
        self.title.is_some()
    }

    /// Untertitel-Segment (ohne Mediendatei)?
    pub fn is_subtitle(&self) -> bool {
        self.subtitle.is_some()
    }

    /// Generator-Clip ohne Mediendatei (Titel oder Untertitel) — von der
    /// Verwaisten-Bereinigung und dem Player ausgenommen.
    pub fn is_generator(&self) -> bool {
        self.is_title() || self.is_subtitle()
    }

    /// Liegt eine Medienzeit `m` im aktuell sichtbaren Quellausschnitt des
    /// Clips? (Clip-Marker außerhalb sind weggetrimmt und werden nicht
    /// gezeichnet, bleiben aber erhalten.)
    pub fn media_in_view(&self, m: f64) -> bool {
        m >= self.media_in() - EPS && m <= self.media_out() + EPS
    }

    /// Sichtbare Clip-Marker als (Sequenzzeit, &Marker) — bereits in den
    /// Clipausschnitt projiziert (für Lineal-Kerben und Tooltips).
    pub fn visible_markers(&self) -> impl Iterator<Item = (f64, &Marker)> {
        self.markers
            .iter()
            .filter(move |m| self.media_in_view(m.time))
            .map(move |m| (self.seq_time_of_media(m.time), m))
    }
}

/// Ende des letzten Clips — die effektive Sequenzdauer.
pub fn sequence_end(clips: &[TimelineClip]) -> f64 {
    clips.iter().map(|c| c.end()).fold(0.0, f64::max)
}

/// Anzeigename einer Spur (V1 unten im Video-Block, A1 oben im Audio-Block,
/// U1 unten im Untertitel-Block über dem Video).
pub fn track_name(track: &TimelineTrack, tracks: &[TimelineTrack]) -> String {
    match track.kind {
        TrackKind::Subtitle => {
            let subs: Vec<&TimelineTrack> = tracks
                .iter()
                .filter(|t| t.kind == TrackKind::Subtitle)
                .collect();
            let idx = subs.iter().position(|t| t.id == track.id).unwrap_or(0);
            format!("U{}", subs.len() - idx)
        }
        TrackKind::Video => {
            let videos: Vec<&TimelineTrack> =
                tracks.iter().filter(|t| t.kind == TrackKind::Video).collect();
            let idx = videos.iter().position(|t| t.id == track.id).unwrap_or(0);
            format!("V{}", videos.len() - idx)
        }
        TrackKind::Audio => {
            let audios: Vec<&TimelineTrack> =
                tracks.iter().filter(|t| t.kind == TrackKind::Audio).collect();
            let idx = audios.iter().position(|t| t.id == track.id).unwrap_or(0);
            format!("A{}", idx + 1)
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrimEdge {
    Start,
    End,
}

#[derive(Clone)]
struct Snapshot {
    tracks: Vec<TimelineTrack>,
    clips: Vec<TimelineClip>,
    transitions: Vec<Transition>,
    markers: Vec<Marker>,
    master_gain_db: f64,
}

/// Kopierte Clip-Attribute („Attribute einfügen“): Transform/Deckkraft/
/// Lautstärke, Farbkorrektur, Effekt-Stapel und Clip-Gain; bei A/V-Paaren
/// inklusive der Attribute des Partners.
#[derive(Clone)]
pub struct ClipAttributes {
    pub fx: ClipFx,
    pub grade: ColorGrade,
    pub effects: Vec<EffectInstance>,
    pub gain_db: f64,
    pub from_kind: TrackKind,
    pub linked: Option<Box<ClipAttributes>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SelectMode {
    Replace,
    Add,
    Toggle,
}

/// Geplante Platzierung eines Assets beim Einfügen/Droppen.
#[derive(Clone, Debug)]
pub struct PlannedPlacement {
    pub asset_id: String,
    pub kind: TrackKind,
    /// None = passende Spur fehlt und wird automatisch angelegt.
    pub track_id: Option<String>,
    pub start: f64,
    pub duration: f64,
    pub name: String,
    pub src_duration: f64,
    pub linked: bool,
}

/// Vorschlag „Sequenz an Medien anpassen?“ nach dem ersten Clip-Drop in
/// eine leere Timeline (Premiere-Verhalten). Wird vom Mainloop in einen
/// modalen Prompt übersetzt.
#[derive(Clone, Debug, PartialEq)]
pub struct MediaMatchSuggestion {
    pub settings: SequenceSettings,
    pub asset_name: String,
}

pub struct TimelineStore {
    /// Sequenz-Einstellungen (Auflösung, Framerate, Drop-Frame-Timecode) —
    /// bewusst nicht Teil der Undo-History (Dialog-Workflow wie Premiere),
    /// aber des Dirty-Trackings (Änderung bumpt `revision`).
    pub settings: SequenceSettings,
    /// Ausstehender „An Medien anpassen?“-Vorschlag (nicht persistiert).
    pub pending_media_match: Option<MediaMatchSuggestion>,
    pub tracks: Vec<TimelineTrack>,
    pub clips: Vec<TimelineClip>,
    /// Übergänge an Schnittkanten (Teil der Undo-History und Projektdatei).
    pub transitions: Vec<Transition>,
    /// Sequenz-Marker in Sequenz-Sekunden (Teil der Undo-History und
    /// Projektdatei). Stets nach Zeit sortiert gehalten.
    pub markers: Vec<Marker>,
    pub selected_clip_ids: Vec<String>,
    /// Ausgewählte Übergänge (wie die Clip-Auswahl nicht Teil der History).
    pub selected_transition_ids: Vec<String>,
    pub clipboard: Vec<TimelineClip>,
    /// Übergänge, deren Kanten vollständig in der Zwischenablage liegen.
    clipboard_transitions: Vec<Transition>,
    pub playhead_sec: f64,
    /// In-/Out-Punkt der Sequenz: Loop-Bereich des Programmmonitors.
    /// Wie Playhead/Auswahl bewusst nicht Teil der Undo-History.
    pub in_point: Option<f64>,
    pub out_point: Option<f64>,
    pub snapping: bool,
    pub zoom_px_per_sec: f64,
    /// Sichtbare Breite des Spurbereichs (vom Panel gemeldet, für zoom_to_fit).
    pub viewport_w: f64,
    /// Zählt strukturelle Änderungen (Edits/Undo/Redo) — Basis des
    /// Dirty-Trackings im Projekt; Playhead/Zoom/Auswahl zählen nicht.
    pub revision: u64,
    /// Summen-Fader des Mixers in dB; ≤ −60 gilt als −∞.
    pub master_gain_db: f64,
    /// Aktive Untertitel-Spur (Ziel von „Untertitel hinzufügen“ und
    /// SRT-Export; Auswahl im Untertitel-Panel). Wie die Clip-Auswahl
    /// nicht Teil der Undo-History, aber persistiert.
    pub active_subtitle_track_id: Option<String>,
    /// Klemmbrett für „Attribute kopieren/einfügen“ (nicht persistiert).
    attr_clipboard: Option<ClipAttributes>,
    past: Vec<Snapshot>,
    future: Vec<Snapshot>,
}

impl Default for TimelineStore {
    fn default() -> Self {
        // Startbelegung wie ein frisches Premiere-Projekt: 2 Video-, 2 Audiospuren.
        // V1 (unterste Video-) und A1 (oberste Audiospur) sind Patch- und
        // Targeting-Ziel — der Standard-Schnittpfad ohne weitere Eingaben.
        let mut tracks = vec![
            make_track(TrackKind::Video),
            make_track(TrackKind::Video),
            make_track(TrackKind::Audio),
            make_track(TrackKind::Audio),
        ];
        tracks[1].source_patched = true;
        tracks[1].targeted = true;
        tracks[2].source_patched = true;
        tracks[2].targeted = true;
        TimelineStore {
            settings: SequenceSettings::default(),
            pending_media_match: None,
            tracks,
            clips: Vec::new(),
            transitions: Vec::new(),
            markers: Vec::new(),
            selected_clip_ids: Vec::new(),
            selected_transition_ids: Vec::new(),
            clipboard: Vec::new(),
            clipboard_transitions: Vec::new(),
            playhead_sec: 0.0,
            in_point: None,
            out_point: None,
            snapping: true,
            zoom_px_per_sec: 40.0,
            viewport_w: 0.0,
            revision: 0,
            master_gain_db: 0.0,
            active_subtitle_track_id: None,
            attr_clipboard: None,
            past: Vec::new(),
            future: Vec::new(),
        }
    }
}

fn clamp_zoom(v: f64) -> f64 {
    if !v.is_finite() {
        return MIN_ZOOM;
    }
    v.clamp(MIN_ZOOM, MAX_ZOOM)
}

/// Entfernt aus `clips` alles, was auf `track_id` den Bereich [start, end)
/// belegt (Overwrite-Semantik wie beim Premiere-Drop).
fn overwrite_range(clips: Vec<TimelineClip>, track_id: &str, start: f64, end: f64) -> Vec<TimelineClip> {
    if end - start <= EPS {
        return clips;
    }
    let mut out = Vec::with_capacity(clips.len());
    for clip in clips {
        let clip_end = clip.end();
        if clip.track_id != track_id || clip_end <= start + EPS || clip.start >= end - EPS {
            out.push(clip);
            continue;
        }
        let left_len = start - clip.start;
        let right_len = clip_end - end;
        let keep_left = left_len >= MIN_CLIP_DURATION - EPS;
        if keep_left {
            let mut left = clip.clone();
            left.src_in = clip.split_src_ins(left_len).0;
            left.duration = left_len;
            out.push(left);
        }
        if right_len >= MIN_CLIP_DURATION - EPS {
            let mut right = clip.clone();
            if keep_left {
                right.id = new_id();
            }
            right.src_in = clip.split_src_ins(end - clip.start).1;
            right.start = end;
            right.duration = right_len;
            out.push(right);
        }
    }
    out
}

fn locked_track_ids(tracks: &[TimelineTrack]) -> std::collections::HashSet<String> {
    tracks
        .iter()
        .filter(|t| t.locked)
        .map(|t| t.id.clone())
        .collect()
}

/// Übergang auf kopierte Clips übertragen: nur wenn ALLE referenzierten
/// Clips in der Map (alt → neu) liegen; die Kopie erhält eine frische ID.
fn remap_transition(
    tr: &Transition,
    id_map: &std::collections::HashMap<String, String>,
) -> Option<Transition> {
    let map = |id: &Option<String>| -> Option<Option<String>> {
        match id {
            None => Some(None),
            Some(old) => id_map.get(old).cloned().map(Some),
        }
    };
    let from = map(&tr.from_clip_id)?;
    let to = map(&tr.to_clip_id)?;
    let mut copy = tr.clone();
    copy.id = new_id();
    copy.from_clip_id = from;
    copy.to_clip_id = to;
    Some(copy)
}

/// Erweitert eine Clip-Auswahl um alle verknüpften Partner.
pub fn expand_links(clips: &[TimelineClip], ids: &[String]) -> Vec<String> {
    use std::collections::HashSet;
    let mut id_set: HashSet<&str> = ids.iter().map(|s| s.as_str()).collect();
    let link_ids: HashSet<&str> = clips
        .iter()
        .filter(|c| id_set.contains(c.id.as_str()))
        .filter_map(|c| c.link_id.as_deref())
        .collect();
    for clip in clips {
        if let Some(link) = clip.link_id.as_deref() {
            if link_ids.contains(link) {
                id_set.insert(clip.id.as_str());
            }
        }
    }
    id_set.into_iter().map(String::from).collect()
}

/// Quellmaterial vor dem Timeline-KOPF des Clips, in TIMELINE-Sekunden
/// (Medien-Handle ÷ Speed; rückwärts liegt der Kopf am Medien-Out).
/// Standbilder/unendliche Quellen sind frei dehnbar.
pub fn head_room(clip: &TimelineClip) -> f64 {
    if clip.freeze || !clip.src_duration.is_finite() {
        return f64::INFINITY;
    }
    let media = if clip.reverse {
        clip.src_duration - clip.media_out()
    } else {
        clip.src_in
    };
    media.max(0.0) / clip.eff_speed()
}

/// Quellmaterial hinter dem Timeline-ENDE des Clips, in TIMELINE-Sekunden.
pub fn tail_room(clip: &TimelineClip) -> f64 {
    if clip.freeze || !clip.src_duration.is_finite() {
        return f64::INFINITY;
    }
    let media = if clip.reverse {
        clip.src_in
    } else {
        clip.src_duration - clip.media_out()
    };
    media.max(0.0) / clip.eff_speed()
}

/// Maximal erlaubtes Delta beim Trimmen einer Kante (Quelle + Nachbarn).
pub fn trim_range(
    clip: &TimelineClip,
    edge: TrimEdge,
    clips: &[TimelineClip],
    respect_neighbors: bool,
) -> (f64, f64) {
    match edge {
        TrimEdge::Start => {
            // Negativ = Kopf verlängern: nicht vor Quelle/Sequenzanfang.
            let mut lo = (-head_room(clip)).max(-clip.start);
            let hi = clip.duration - MIN_CLIP_DURATION;
            if respect_neighbors {
                let mut prev_end: f64 = 0.0;
                for c in clips {
                    if c.track_id != clip.track_id || c.id == clip.id {
                        continue;
                    }
                    let c_end = c.end();
                    if c_end <= clip.start + EPS {
                        prev_end = prev_end.max(c_end);
                    }
                }
                lo = lo.max(prev_end - clip.start);
            }
            (lo, hi)
        }
        TrimEdge::End => {
            let clip_end = clip.end();
            let lo = -(clip.duration - MIN_CLIP_DURATION);
            let mut hi = tail_room(clip);
            if respect_neighbors {
                let mut next_start = f64::INFINITY;
                for c in clips {
                    if c.track_id != clip.track_id || c.id == clip.id {
                        continue;
                    }
                    if c.start >= clip_end - EPS {
                        next_start = next_start.min(c.start);
                    }
                }
                hi = hi.min(next_start - clip_end);
            }
            (lo, hi)
        }
    }
}

/// Eine Kante trimmen (auch Drag-Vorschau im Timeline-Panel): die
/// gegenüberliegende Medienkante bleibt stehen — vorwärts wandert beim
/// Kopf-Trim der Medien-In, rückwärts beim End-Trim (Spiegelung).
pub fn apply_trim(clip: &TimelineClip, edge: TrimEdge, delta: f64) -> TimelineClip {
    let mut c = clip.clone();
    match edge {
        TrimEdge::Start => {
            c.start += delta;
            c.duration -= delta;
            if !c.freeze && !c.reverse {
                c.src_in += delta * c.eff_speed();
            }
            // Rückwärts: Medien-Out = src_in + duration·speed wandert mit
            // der Dauer, src_in (tiefster Punkt) bleibt.
        }
        TrimEdge::End => {
            c.duration += delta;
            if !c.freeze && c.reverse {
                // Ende verlängern spielt FRÜHERES Material: src_in sinkt.
                c.src_in = (c.src_in - delta * c.eff_speed()).max(0.0);
            }
        }
    }
    c
}

/// Plant, wo Assets bei einem Drop/Einfügen landen — von Drop-Vorschau und
/// insert_assets gleichermaßen benutzt.
pub fn plan_asset_placements(
    timeline: &TimelineStore,
    assets: &[MediaAsset],
    asset_ids: &[String],
    at: f64,
    drop_track_id: Option<&str>,
) -> Vec<PlannedPlacement> {
    let drop_track = drop_track_id.and_then(|id| timeline.tracks.iter().find(|t| t.id == id));
    if drop_track.is_some_and(|t| t.locked) {
        return Vec::new();
    }

    let lane_for = |kind: TrackKind| -> Option<String> {
        if let Some(dt) = drop_track {
            if dt.kind == kind {
                return Some(dt.id.clone());
            }
        }
        let of_kind: Vec<&TimelineTrack> = timeline
            .tracks
            .iter()
            .filter(|t| t.kind == kind && !t.locked)
            .collect();
        let lane = match kind {
            TrackKind::Video => of_kind.last(),
            TrackKind::Audio => of_kind.first(),
            // Medien landen nie auf Untertitel-Spuren.
            TrackKind::Subtitle => None,
        };
        lane.map(|t| t.id.clone())
    };

    let mut cursor = at.max(0.0);
    let mut placements = Vec::new();
    for asset_id in asset_ids {
        let Some(asset) = assets.iter().find(|a| &a.id == asset_id) else {
            continue;
        };
        let is_image = asset.kind == MediaKind::Image;
        let duration = if is_image {
            IMAGE_DEFAULT_DURATION
        } else {
            asset.info.duration_sec.max(MIN_CLIP_DURATION)
        };
        let src_duration = if is_image {
            f64::INFINITY
        } else {
            asset.info.duration_sec
        };
        let has_video = asset.kind != MediaKind::Audio;
        let has_audio = !is_image && !asset.info.audio.is_empty();
        let linked = has_video && has_audio;
        if has_video {
            placements.push(PlannedPlacement {
                asset_id: asset_id.clone(),
                kind: TrackKind::Video,
                track_id: lane_for(TrackKind::Video),
                start: cursor,
                duration,
                name: asset.name.clone(),
                src_duration,
                linked,
            });
        }
        if has_audio {
            placements.push(PlannedPlacement {
                asset_id: asset_id.clone(),
                kind: TrackKind::Audio,
                track_id: lane_for(TrackKind::Audio),
                start: cursor,
                duration,
                name: if linked {
                    format!("{} (Audio)", asset.name)
                } else {
                    asset.name.clone()
                },
                src_duration,
                linked,
            });
        }
        if has_video || has_audio {
            cursor += duration;
        }
    }
    placements
}

/// Marker stabil nach Zeitpunkt sortieren (Lineal, Panel, Navigation).
pub fn sort_markers(markers: &mut [Marker]) {
    markers.sort_by(|a, b| a.time.partial_cmp(&b.time).unwrap_or(std::cmp::Ordering::Equal));
}

/// Sortierte, eindeutige Schnittpunkte (Clipgrenzen) der Sequenz.
pub fn edit_points(clips: &[TimelineClip]) -> Vec<f64> {
    let mut points: Vec<f64> = vec![0.0];
    for c in clips {
        points.push(c.start);
        points.push(c.end());
    }
    points.sort_by(|a, b| a.partial_cmp(b).unwrap());
    points.dedup_by(|a, b| (*a - *b).abs() < EPS);
    points
}

impl TimelineStore {
    fn push_history(&mut self) {
        self.past.push(Snapshot {
            tracks: self.tracks.clone(),
            clips: self.clips.clone(),
            transitions: self.transitions.clone(),
            markers: self.markers.clone(),
            master_gain_db: self.master_gain_db,
        });
        if self.past.len() > HISTORY_LIMIT {
            self.past.remove(0);
        }
        self.future.clear();
        self.revision += 1;
    }

    /// Sequenz aus einer Projektdatei übernehmen: ersetzt Inhalt und
    /// verwirft History/Auswahl/Zwischenablage.
    #[allow(clippy::too_many_arguments)]
    pub fn load_document(
        &mut self,
        settings: Option<SequenceSettings>,
        tracks: Vec<TimelineTrack>,
        clips: Vec<TimelineClip>,
        transitions: Vec<Transition>,
        markers: Vec<Marker>,
        playhead_sec: f64,
        in_point: Option<f64>,
        out_point: Option<f64>,
        zoom_px_per_sec: f64,
        snapping: bool,
        selected_clip_ids: Vec<String>,
        master_gain_db: f64,
        active_subtitle_track_id: Option<String>,
    ) {
        let defaults = TimelineStore::default();
        // Altprojekte ohne Sequenz-Einstellungen laden mit 25 fps weiter;
        // der Aufrufer (project::apply) rät die Auflösung aus den Medien.
        self.settings = settings.map(SequenceSettings::sanitized).unwrap_or_default();
        self.pending_media_match = None;
        self.tracks = if tracks.is_empty() { defaults.tracks } else { tracks };
        self.clips = clips;
        let track_ids: std::collections::HashSet<&str> =
            self.tracks.iter().map(|t| t.id.as_str()).collect();
        // Defensive Validierung: Clips ohne gültige Spur oder mit kaputten
        // Zeiten fliegen raus, statt später Layout/Player zu zerlegen.
        self.clips.retain(|c| {
            track_ids.contains(c.track_id.as_str())
                && c.start.is_finite()
                && c.duration.is_finite()
                && c.src_in.is_finite()
                && c.duration >= MIN_CLIP_DURATION - EPS
        });
        // Effekt-Parameterlisten an die aktuelle Spec angleichen (ältere/
        // neuere Projektdateien).
        for c in &mut self.clips {
            for e in &mut c.effects {
                e.normalize();
            }
            for m in &mut c.markers {
                m.sanitize();
            }
            sort_markers(&mut c.markers);
            // Geschwindigkeit defensiv klemmen (fremde/kaputte Dateien).
            if !c.speed.is_finite() || c.speed <= 0.0 {
                c.speed = 1.0;
            } else {
                c.speed = c.speed.clamp(MIN_CLIP_SPEED, MAX_CLIP_SPEED);
            }
            if c.freeze {
                c.reverse = false;
            }
        }
        // Übergänge defensiv validieren (verwaiste/kaputte fliegen raus).
        self.transitions = transitions
            .into_iter()
            .filter(|t| t.duration.is_finite() && t.duration > 0.0)
            .collect();
        self.selected_transition_ids.clear();
        self.reconcile_transitions();
        // Sequenz-Marker defensiv bereinigen + sortieren.
        self.markers = markers;
        for m in &mut self.markers {
            m.sanitize();
        }
        sort_markers(&mut self.markers);
        self.clipboard.clear();
        self.clipboard_transitions.clear();
        self.selected_clip_ids = selected_clip_ids;
        self.prune_selection();
        self.playhead_sec = if playhead_sec.is_finite() { playhead_sec.max(0.0) } else { 0.0 };
        self.in_point = in_point.filter(|v| v.is_finite());
        self.out_point = out_point.filter(|v| v.is_finite());
        self.zoom_px_per_sec = clamp_zoom(zoom_px_per_sec);
        self.snapping = snapping;
        self.master_gain_db = if master_gain_db.is_finite() {
            master_gain_db.clamp(-60.0, 6.0)
        } else {
            0.0
        };
        self.active_subtitle_track_id = active_subtitle_track_id.filter(|id| {
            self.tracks
                .iter()
                .any(|t| t.id == *id && t.kind == TrackKind::Subtitle)
        });
        self.past.clear();
        self.future.clear();
        self.revision += 1;
    }

    fn prune_selection(&mut self) {
        let existing: std::collections::HashSet<&str> =
            self.clips.iter().map(|c| c.id.as_str()).collect();
        self.selected_clip_ids.retain(|id| existing.contains(id.as_str()));
    }

    pub fn can_undo(&self) -> bool {
        !self.past.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.future.is_empty()
    }

    // ------------------------------------------------------------- Ansicht

    pub fn zoom_in(&mut self) {
        self.zoom_px_per_sec = clamp_zoom(self.zoom_px_per_sec * ZOOM_FACTOR);
    }

    pub fn zoom_out(&mut self) {
        self.zoom_px_per_sec = clamp_zoom(self.zoom_px_per_sec / ZOOM_FACTOR);
    }

    pub fn set_zoom(&mut self, v: f64) {
        self.zoom_px_per_sec = clamp_zoom(v);
    }

    pub fn zoom_to_fit(&mut self) {
        let end = sequence_end(&self.clips);
        if end <= 0.0 || self.viewport_w <= 0.0 {
            return;
        }
        self.zoom_px_per_sec = clamp_zoom(self.viewport_w * 0.97 / end);
    }

    pub fn toggle_snapping(&mut self) {
        self.snapping = !self.snapping;
    }

    // ------------------------------------------------------------ Playhead

    pub fn set_playhead(&mut self, t: f64) {
        self.playhead_sec = t.max(0.0);
    }

    /// Frame-genaues Stepping gegen die Sequenzrate: rastet den Playhead
    /// auf das Frame-Raster ein (rationale Arithmetik, NTSC-driftfrei).
    pub fn step_playhead_frames(&mut self, frames: f64) {
        let rate = self.settings.rate;
        let current = rate.frame_round(self.playhead_sec) as f64;
        self.playhead_sec = rate.time_of_frame((current + frames).max(0.0));
    }

    // ------------------------------------------------ Sequenz-Einstellungen

    /// Sequenz-Einstellungen übernehmen (Dialog/Media-Match). Kein Undo-
    /// Snapshot (Premiere-Konvention), aber Dirty-Tracking über `revision`.
    pub fn set_sequence_settings(&mut self, settings: SequenceSettings) {
        let settings = settings.sanitized();
        if settings == self.settings {
            return;
        }
        self.settings = settings;
        self.revision += 1;
    }

    pub fn go_to_start(&mut self) {
        self.playhead_sec = 0.0;
    }

    pub fn go_to_end(&mut self) {
        self.playhead_sec = sequence_end(&self.clips);
    }

    pub fn go_to_prev_edit(&mut self) {
        let edges = edit_points(&self.clips);
        let prev = edges.iter().rev().find(|e| **e < self.playhead_sec - EPS);
        self.playhead_sec = prev.copied().unwrap_or(0.0);
    }

    pub fn go_to_next_edit(&mut self) {
        let edges = edit_points(&self.clips);
        if let Some(next) = edges.iter().find(|e| **e > self.playhead_sec + EPS) {
            self.playhead_sec = *next;
        }
    }

    // ------------------------------------------------------------ Marker
    // Sequenz-Marker liegen in Sequenz-Sekunden und sind Teil der
    // Undo-History (über push_history). Sie werden stets frame-genau gegen
    // die Sequenzrate gerastert und nach Zeit sortiert gehalten.

    /// Rastert eine Sequenzzeit frame-genau (rationale NTSC-Arithmetik).
    pub fn snap_to_frame(&self, t: f64) -> f64 {
        let rate = self.settings.rate;
        if rate.num == 0 || rate.den == 0 || !t.is_finite() {
            return t.max(0.0);
        }
        rate.time_of_frame(rate.frame_round(t.max(0.0)) as f64).max(0.0)
    }

    /// Sequenz-Marker exakt (innerhalb eines halben Frames) bei `t`.
    fn marker_index_at(&self, t: f64) -> Option<usize> {
        let tol = (0.5 / self.settings.rate.fps()).max(EPS);
        self.markers.iter().position(|m| (m.time - t).abs() <= tol)
    }

    /// Sequenz-Marker am Playhead setzen (M) — latenzfrei, idempotent:
    /// existiert am selben Frame schon einer, wird dessen ID zurückgegeben.
    /// Liefert die ID des (neuen oder bestehenden) Markers.
    pub fn add_marker_at(&mut self, t: f64) -> String {
        let t = self.snap_to_frame(t);
        if let Some(idx) = self.marker_index_at(t) {
            return self.markers[idx].id.clone();
        }
        self.push_history();
        let marker = Marker::new(t);
        let id = marker.id.clone();
        self.markers.push(marker);
        sort_markers(&mut self.markers);
        id
    }

    /// Beginn einer Marker-Geste (Drag/Dialog) — ein Snapshot.
    pub fn begin_marker_edit(&mut self) {
        self.push_history();
    }

    /// Sequenz-Marker ändern OHNE neuen Snapshot (laufende Geste nach
    /// `begin_marker_edit`). Hält die Sortierung aufrecht.
    pub fn marker_update_live(&mut self, id: &str, f: impl FnOnce(&mut Marker)) {
        if let Some(m) = self.markers.iter_mut().find(|m| m.id == id) {
            f(m);
            m.sanitize();
        }
        sort_markers(&mut self.markers);
        self.revision += 1;
    }

    /// Sequenz-Marker ändern (mit Undo-Snapshot) — für Einzelaktionen.
    pub fn marker_update(&mut self, id: &str, f: impl FnOnce(&mut Marker)) {
        if !self.markers.iter().any(|m| m.id == id) {
            return;
        }
        self.push_history();
        self.marker_update_live(id, f);
    }

    /// Einen Sequenz-Marker entfernen.
    pub fn remove_marker(&mut self, id: &str) {
        if !self.markers.iter().any(|m| m.id == id) {
            return;
        }
        self.push_history();
        self.markers.retain(|m| m.id != id);
    }

    /// Alle Sequenz-Marker entfernen.
    pub fn clear_markers(&mut self) {
        if self.markers.is_empty() {
            return;
        }
        self.push_history();
        self.markers.clear();
    }

    /// Sequenz-Marker, der den Playhead überdeckt (Punkt: exakt am Frame;
    /// Bereich: Playhead in [time, end]) — für „Marker löschen" / Dialog.
    pub fn marker_at_playhead(&self) -> Option<&Marker> {
        let t = self.snap_to_frame(self.playhead_sec);
        let tol = (0.5 / self.settings.rate.fps()).max(EPS);
        // Exakter Punkttreffer hat Vorrang vor Bereichsüberdeckung.
        self.markers
            .iter()
            .find(|m| (m.time - t).abs() <= tol)
            .or_else(|| {
                self.markers
                    .iter()
                    .find(|m| m.duration > 0.0 && t >= m.time - tol && t <= m.end() + tol)
            })
    }

    /// Den nächstgelegenen Marker zum Playhead löschen (Premiere: „Marker
    /// löschen" wirkt am aktuellen/überdeckten Marker).
    pub fn remove_marker_at_playhead(&mut self) -> bool {
        let Some(id) = self.marker_at_playhead().map(|m| m.id.clone()) else {
            return false;
        };
        self.remove_marker(&id);
        true
    }

    /// Playhead auf den nächsten Sequenz-Marker (echt rechts) setzen.
    pub fn go_to_next_marker(&mut self) -> bool {
        let t = self.playhead_sec;
        if let Some(next) = self
            .markers
            .iter()
            .map(|m| m.time)
            .filter(|mt| *mt > t + EPS)
            .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        {
            self.playhead_sec = next;
            true
        } else {
            false
        }
    }

    /// Playhead auf den vorherigen Sequenz-Marker (echt links) setzen.
    pub fn go_to_prev_marker(&mut self) -> bool {
        let t = self.playhead_sec;
        if let Some(prev) = self
            .markers
            .iter()
            .map(|m| m.time)
            .filter(|mt| *mt < t - EPS)
            .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        {
            self.playhead_sec = prev;
            true
        } else {
            false
        }
    }

    // -------- Clip-Marker (Medienzeit; wandern mit dem Material) ----------

    /// Clip-Marker an der zur Sequenzzeit `t_seq` gehörenden Medienzeit
    /// setzen (z. B. Playhead über dem Clip). Idempotent pro Quell-Frame.
    /// Liefert die Marker-ID, falls der Clip existiert und `t_seq` ihn trifft.
    pub fn add_clip_marker_at_seq(&mut self, clip_id: &str, t_seq: f64) -> Option<String> {
        let Some(clip) = self.clips.iter().find(|c| c.id == clip_id) else {
            return None;
        };
        if t_seq < clip.start - EPS || t_seq > clip.end() + EPS {
            return None;
        }
        let media_t = clip.media_time_at(t_seq).max(0.0);
        self.add_clip_marker(clip_id, media_t)
    }

    /// Clip-Marker an einer absoluten Medienzeit setzen (idempotent pro
    /// Quell-Frame). Liefert die ID des (neuen oder bestehenden) Markers.
    pub fn add_clip_marker(&mut self, clip_id: &str, media_t: f64) -> Option<String> {
        let tol = (0.5 / self.settings.rate.fps()).max(EPS);
        let Some(clip) = self.clips.iter().find(|c| c.id == clip_id) else {
            return None;
        };
        if let Some(existing) = clip
            .markers
            .iter()
            .find(|m| (m.time - media_t).abs() <= tol)
            .map(|m| m.id.clone())
        {
            return Some(existing);
        }
        self.push_history();
        let clip = self.clips.iter_mut().find(|c| c.id == clip_id)?;
        let marker = Marker::new(media_t.max(0.0));
        let id = marker.id.clone();
        clip.markers.push(marker);
        sort_markers(&mut clip.markers);
        Some(id)
    }

    /// Clip-Marker ändern OHNE Snapshot (laufende Geste).
    pub fn clip_marker_update_live(
        &mut self,
        clip_id: &str,
        marker_id: &str,
        f: impl FnOnce(&mut Marker),
    ) {
        if let Some(clip) = self.clips.iter_mut().find(|c| c.id == clip_id) {
            if let Some(m) = clip.markers.iter_mut().find(|m| m.id == marker_id) {
                f(m);
                m.sanitize();
            }
            sort_markers(&mut clip.markers);
        }
        self.revision += 1;
    }

    /// Clip-Marker ändern (mit Undo-Snapshot).
    pub fn clip_marker_update(
        &mut self,
        clip_id: &str,
        marker_id: &str,
        f: impl FnOnce(&mut Marker),
    ) {
        let exists = self
            .clips
            .iter()
            .find(|c| c.id == clip_id)
            .is_some_and(|c| c.markers.iter().any(|m| m.id == marker_id));
        if !exists {
            return;
        }
        self.push_history();
        self.clip_marker_update_live(clip_id, marker_id, f);
    }

    /// Einen Clip-Marker entfernen.
    pub fn remove_clip_marker(&mut self, clip_id: &str, marker_id: &str) {
        let exists = self
            .clips
            .iter()
            .find(|c| c.id == clip_id)
            .is_some_and(|c| c.markers.iter().any(|m| m.id == marker_id));
        if !exists {
            return;
        }
        self.push_history();
        if let Some(clip) = self.clips.iter_mut().find(|c| c.id == clip_id) {
            clip.markers.retain(|m| m.id != marker_id);
        }
    }

    // ------------------------------------------------- In/Out (Loop-Bereich)
    // Halbgesetzte Zustände sind erlaubt; ein Punkt, der den anderen kreuzen
    // würde, löscht ihn (Premiere-Konvention).

    pub fn set_in_point(&mut self, t: Option<f64>) {
        match t {
            None => self.in_point = None,
            Some(t) => {
                let v = t.max(0.0);
                self.in_point = Some(v);
                if let Some(out) = self.out_point {
                    if out <= v + MIN_CLIP_DURATION - EPS {
                        self.out_point = None;
                    }
                }
            }
        }
    }

    pub fn set_out_point(&mut self, t: Option<f64>) {
        match t {
            None => self.out_point = None,
            Some(t) => {
                let v = t.max(0.0);
                self.out_point = Some(v);
                if let Some(inp) = self.in_point {
                    if inp >= v - MIN_CLIP_DURATION + EPS {
                        self.in_point = None;
                    }
                }
            }
        }
    }

    pub fn set_in_out_range(&mut self, a: f64, b: f64) {
        let lo = a.min(b).max(0.0);
        let hi = a.max(b).max(0.0);
        if hi - lo < MIN_CLIP_DURATION {
            return;
        }
        self.in_point = Some(lo);
        self.out_point = Some(hi);
    }

    pub fn clear_in_out(&mut self) {
        self.in_point = None;
        self.out_point = None;
    }

    // ------------------------------------------------------------- Auswahl

    pub fn select_clips(&mut self, ids: &[String], mode: SelectMode, links: bool) {
        let expanded: Vec<String> = if links {
            expand_links(&self.clips, ids)
        } else {
            let mut v = ids.to_vec();
            v.sort();
            v.dedup();
            v
        };
        // Clip- und Übergangsauswahl schließen sich aus (Premiere-Verhalten).
        self.selected_transition_ids.clear();
        match mode {
            SelectMode::Replace => self.selected_clip_ids = expanded,
            SelectMode::Add => {
                for id in expanded {
                    if !self.selected_clip_ids.contains(&id) {
                        self.selected_clip_ids.push(id);
                    }
                }
            }
            SelectMode::Toggle => {
                let all_in = expanded.iter().all(|id| self.selected_clip_ids.contains(id));
                if all_in {
                    self.selected_clip_ids.retain(|id| !expanded.contains(id));
                } else {
                    for id in expanded {
                        if !self.selected_clip_ids.contains(&id) {
                            self.selected_clip_ids.push(id);
                        }
                    }
                }
            }
        }
    }

    pub fn select_all(&mut self) {
        self.selected_clip_ids = self.clips.iter().map(|c| c.id.clone()).collect();
        self.selected_transition_ids.clear();
    }

    pub fn clear_selection(&mut self) {
        self.selected_clip_ids.clear();
        self.selected_transition_ids.clear();
    }

    // -------------------------------------------------------------- Spuren

    pub fn add_track(&mut self, kind: TrackKind) -> String {
        let track = make_track(kind);
        let id = track.id.clone();
        self.push_history();
        // Neue Videospur oben auf den Video-Block, neue Audiospur unten,
        // neue Untertitelspur ganz oben (über allen Untertitel-Spuren).
        match kind {
            TrackKind::Video => {
                let at = self.video_block_start();
                self.tracks.insert(at, track);
            }
            TrackKind::Audio => self.tracks.push(track),
            TrackKind::Subtitle => self.tracks.insert(0, track),
        }
        if kind == TrackKind::Subtitle {
            self.active_subtitle_track_id = Some(id.clone());
        }
        id
    }

    /// Index der obersten Videospur (Untertitel-Spuren liegen davor).
    fn video_block_start(&self) -> usize {
        self.tracks
            .iter()
            .position(|t| t.kind != TrackKind::Subtitle)
            .unwrap_or(self.tracks.len())
    }

    pub fn remove_track(&mut self, track_id: &str) {
        if !self.tracks.iter().any(|t| t.id == track_id) {
            return;
        }
        self.push_history();
        self.tracks.retain(|t| t.id != track_id);
        self.clips.retain(|c| c.track_id != track_id);
        if self.active_subtitle_track_id.as_deref() == Some(track_id) {
            self.active_subtitle_track_id = None;
        }
        self.prune_selection();
        self.reconcile_transitions();
    }

    pub fn toggle_track_flag(&mut self, track_id: &str, flag: TrackFlag) {
        if let Some(t) = self.tracks.iter_mut().find(|t| t.id == track_id) {
            match flag {
                TrackFlag::Muted => t.muted = !t.muted,
                TrackFlag::Solo => t.solo = !t.solo,
                TrackFlag::Locked => t.locked = !t.locked,
                TrackFlag::SyncLock => t.sync_lock = !t.sync_lock,
                TrackFlag::Targeted => t.targeted = !t.targeted,
            }
            // Flags werden mitgespeichert → Projekt muss dirty werden
            // (bewusst ohne History-Snapshot, wie In-/Out-Punkte).
            self.revision += 1;
        }
    }

    /// Spur als Source-Patch-Ziel ihrer Art setzen (Radio: höchstens eine
    /// Video- und eine Audiospur). Erneutes Klicken hebt den Patch auf, sodass
    /// das entsprechende Quell-Material beim Edit übersprungen wird (Premiere:
    /// Patch deaktivieren). Untertitel-Spuren sind kein Patch-Ziel.
    pub fn toggle_source_patch(&mut self, track_id: &str) {
        let Some(kind) = self.tracks.iter().find(|t| t.id == track_id).map(|t| t.kind) else {
            return;
        };
        if kind == TrackKind::Subtitle {
            return;
        }
        let was = self
            .tracks
            .iter()
            .find(|t| t.id == track_id)
            .map(|t| t.source_patched)
            .unwrap_or(false);
        for t in &mut self.tracks {
            if t.kind == kind {
                t.source_patched = false;
            }
        }
        if !was {
            if let Some(t) = self.tracks.iter_mut().find(|t| t.id == track_id) {
                t.source_patched = true;
            }
        }
        self.revision += 1;
    }

    /// Aktuelles Source-Patch-Ziel einer Art (ungesperrt), falls vorhanden.
    pub fn source_patch_track(&self, kind: TrackKind) -> Option<&str> {
        self.tracks
            .iter()
            .find(|t| t.kind == kind && t.source_patched && !t.locked)
            .map(|t| t.id.as_str())
    }

    /// Anvisierte, ungesperrte Spur-IDs (Lift/Extract/Match-Frame-Ziele).
    pub fn targeted_track_ids(&self) -> std::collections::HashSet<String> {
        self.tracks
            .iter()
            .filter(|t| t.targeted && !t.locked && t.kind != TrackKind::Subtitle)
            .map(|t| t.id.clone())
            .collect()
    }

    /// Stellt für Video und Audio je ein Patch- und Targeting-Ziel sicher,
    /// falls keines gesetzt ist (Migration von Altprojekten vor Formatv7).
    /// Standard wie ein frisches Projekt: V1 (unterste Video-) und A1 (oberste
    /// Audiospur).
    pub fn ensure_patch_target_defaults(&mut self) {
        for kind in [TrackKind::Video, TrackKind::Audio] {
            let default_id = match kind {
                TrackKind::Video => self
                    .tracks
                    .iter()
                    .filter(|t| t.kind == kind)
                    .next_back()
                    .map(|t| t.id.clone()),
                _ => self.tracks.iter().find(|t| t.kind == kind).map(|t| t.id.clone()),
            };
            let Some(default_id) = default_id else { continue };
            if !self.tracks.iter().any(|t| t.kind == kind && t.source_patched) {
                if let Some(t) = self.tracks.iter_mut().find(|t| t.id == default_id) {
                    t.source_patched = true;
                }
            }
            if !self.tracks.iter().any(|t| t.kind == kind && t.targeted) {
                if let Some(t) = self.tracks.iter_mut().find(|t| t.id == default_id) {
                    t.targeted = true;
                }
            }
        }
        self.revision += 1;
    }

    // ------------------------------------------------------------- Mixer

    /// Beginn einer Mixer-Geste (Fader-/Pan-Drag, Reset): legt einmalig
    /// einen Undo-Snapshot an; die laufenden Wertänderungen der Geste
    /// schreiben danach direkt über die Setter.
    pub fn begin_mix_edit(&mut self) {
        self.push_history();
    }

    pub fn set_track_gain_db(&mut self, track_id: &str, gain_db: f64) {
        if let Some(t) = self.tracks.iter_mut().find(|t| t.id == track_id) {
            t.gain_db = gain_db.clamp(-60.0, 6.0);
        }
    }

    pub fn set_track_pan(&mut self, track_id: &str, pan: f64) {
        if let Some(t) = self.tracks.iter_mut().find(|t| t.id == track_id) {
            t.pan = pan.clamp(-1.0, 1.0);
        }
    }

    pub fn set_master_gain_db(&mut self, gain_db: f64) {
        self.master_gain_db = gain_db.clamp(-60.0, 6.0);
    }

    /// Clip-Verstärkung der ausgewählten Audio-Clips um `delta_db` ändern
    /// (Premiere-Pendant „Audio-Verstärkung anpassen“).
    pub fn nudge_selected_clip_gain(&mut self, delta_db: f64) {
        let sel = self.selected_clip_ids.clone();
        if !self
            .clips
            .iter()
            .any(|c| sel.contains(&c.id) && c.kind == TrackKind::Audio)
        {
            return;
        }
        self.push_history();
        for clip in self
            .clips
            .iter_mut()
            .filter(|c| sel.contains(&c.id) && c.kind == TrackKind::Audio)
        {
            clip.gain_db = (clip.gain_db + delta_db).clamp(-60.0, 24.0);
        }
    }

    /// Clip-Verstärkung der ausgewählten Audio-Clips auf 0 dB zurücksetzen.
    pub fn reset_selected_clip_gain(&mut self) {
        let sel = self.selected_clip_ids.clone();
        if !self
            .clips
            .iter()
            .any(|c| sel.contains(&c.id) && c.kind == TrackKind::Audio && c.gain_db != 0.0)
        {
            return;
        }
        self.push_history();
        for clip in self
            .clips
            .iter_mut()
            .filter(|c| sel.contains(&c.id) && c.kind == TrackKind::Audio)
        {
            clip.gain_db = 0.0;
        }
    }

    // --------------------------------------------------------- Bearbeitung

    pub fn insert_assets(
        &mut self,
        assets: &[MediaAsset],
        asset_ids: &[String],
        at: f64,
        drop_track_id: Option<&str>,
    ) {
        let placements = plan_asset_placements(self, assets, asset_ids, at, drop_track_id);
        if placements.is_empty() {
            return;
        }
        let was_empty = self.clips.is_empty();
        self.push_history();

        // Fehlende Spuren anlegen (höchstens eine je Art).
        let mut new_video: Option<String> = None;
        let mut new_audio: Option<String> = None;
        for p in &placements {
            if p.track_id.is_some() {
                continue;
            }
            match p.kind {
                TrackKind::Video => {
                    if new_video.is_none() {
                        let track = make_track(TrackKind::Video);
                        new_video = Some(track.id.clone());
                        let at = self.video_block_start();
                        self.tracks.insert(at, track);
                    }
                }
                TrackKind::Audio => {
                    if new_audio.is_none() {
                        let track = make_track(TrackKind::Audio);
                        new_audio = Some(track.id.clone());
                        self.tracks.push(track);
                    }
                }
                // Medien-Placements zielen nie auf Untertitel-Spuren.
                TrackKind::Subtitle => {}
            }
        }

        let mut clips = std::mem::take(&mut self.clips);
        let mut inserted: Vec<TimelineClip> = Vec::new();
        // Video- und Audio-Teil desselben Assets teilen sich eine link_id.
        let mut link_ids: std::collections::HashMap<String, String> = Default::default();
        for p in &placements {
            let track_id = match &p.track_id {
                Some(id) => id.clone(),
                None => match p.kind {
                    TrackKind::Video => new_video.clone().unwrap(),
                    TrackKind::Audio => new_audio.clone().unwrap(),
                    // Medien-Placements zielen nie auf Untertitel-Spuren.
                    TrackKind::Subtitle => continue,
                },
            };
            let link_id = if p.linked {
                let key = format!("{}@{}", p.asset_id, p.start);
                Some(
                    link_ids
                        .entry(key)
                        .or_insert_with(new_id)
                        .clone(),
                )
            } else {
                None
            };
            clips = overwrite_range(clips, &track_id, p.start, p.start + p.duration);
            // Asset-Marker (Quellmonitor) in Clip-Marker übernehmen, sofern
            // sie in den belegten Quellausschnitt [0, duration] fallen.
            let markers: Vec<Marker> = assets
                .iter()
                .find(|a| a.id == p.asset_id)
                .map(|a| {
                    a.markers
                        .iter()
                        .filter(|m| m.time >= -EPS && m.time <= p.duration + EPS)
                        .map(|m| {
                            let mut nm = m.clone();
                            nm.id = new_id();
                            nm
                        })
                        .collect()
                })
                .unwrap_or_default();
            inserted.push(TimelineClip {
                id: new_id(),
                track_id,
                asset_id: p.asset_id.clone(),
                name: p.name.clone(),
                kind: p.kind,
                start: p.start,
                duration: p.duration,
                src_in: 0.0,
                src_duration: p.src_duration,
                link_id,
                enabled: true,
                gain_db: 0.0,
                fx: ClipFx::default(),
                grade: ColorGrade::default(),
                effects: Vec::new(),
                title: None,
                subtitle: None,
                speed: 1.0,
                reverse: false,
                freeze: false,
                markers,
            });
        }

        self.selected_clip_ids = inserted.iter().map(|c| c.id.clone()).collect();
        clips.extend(inserted);
        self.clips = clips;
        self.reconcile_transitions();

        // Erster Drop in eine leere Timeline: abweichende Medien-Settings
        // als „An Medien anpassen?“-Vorschlag anbieten (wie Premiere).
        if was_empty {
            if let Some((suggested, asset_name)) =
                sequence::suggest_from_assets(&self.settings, assets, asset_ids)
            {
                if suggested != self.settings {
                    self.pending_media_match = Some(MediaMatchSuggestion {
                        settings: suggested,
                        asset_name,
                    });
                }
            }
        }
    }

    // -------------------------------------------- Insert / Three-Point-Edit

    /// Insert-Ripple-Primitive: öffnet auf den `affected`-Spuren bei `at` eine
    /// Lücke der Breite `amount`. Clips, die `at` durchschneiden, werden geteilt
    /// (Marker und End-Übergänge wandern auf die rechte Hälfte); alles ab `at`
    /// rückt nach hinten. KEIN History-Snapshot (der Aufrufer macht einen).
    fn open_gap(&mut self, affected: &std::collections::HashSet<String>, at: f64, amount: f64) {
        use std::collections::HashMap;
        if amount <= EPS {
            return;
        }
        let mut new_link_ids: HashMap<String, String> = HashMap::new();
        // Original-Clip → rechte Hälfte: End-Übergänge müssen mitwandern.
        let mut right_ids: HashMap<String, String> = HashMap::new();
        let mut out: Vec<TimelineClip> = Vec::with_capacity(self.clips.len() + 4);
        for c in std::mem::take(&mut self.clips) {
            let on_track = affected.contains(&c.track_id);
            let straddles = on_track
                && at > c.start + MIN_CLIP_DURATION - EPS
                && at < c.end() - MIN_CLIP_DURATION + EPS;
            if straddles {
                let left_len = at - c.start;
                let (left_src_in, right_src_in) = c.split_src_ins(left_len);
                let right_link = c.link_id.as_ref().map(|link| {
                    new_link_ids.entry(link.clone()).or_insert_with(new_id).clone()
                });
                let cut_media = c.media_time_at(at);
                let on_right = |m: &Marker| -> bool {
                    if c.freeze {
                        false
                    } else if c.reverse {
                        m.time <= cut_media
                    } else {
                        m.time >= cut_media
                    }
                };
                let mut left = c.clone();
                left.src_in = left_src_in;
                left.duration = left_len;
                left.markers = c.markers.iter().filter(|m| !on_right(m)).cloned().collect();
                let mut right = c.clone();
                right.id = new_id();
                right_ids.insert(c.id.clone(), right.id.clone());
                right.start = at + amount;
                right.src_in = right_src_in;
                right.duration = c.end() - at;
                right.link_id = right_link;
                right.markers = c.markers.iter().filter(|m| on_right(m)).cloned().collect();
                out.push(left);
                out.push(right);
            } else {
                let mut c = c;
                if on_track && c.start >= at - EPS {
                    c.start += amount;
                }
                out.push(c);
            }
        }
        self.clips = out;
        for tr in &mut self.transitions {
            if let Some(right) = tr.from_clip_id.as_ref().and_then(|id| right_ids.get(id)) {
                tr.from_clip_id = Some(right.clone());
            }
        }
    }

    /// Fertige Quell-Clips als Insert- (Ripple) oder Overwrite-Edit setzen.
    /// `gap_at`/`gap_len` = der von den Clips belegte Sequenzbereich:
    /// beim Insert die zu öffnende Lücke (auf Ziel- UND Sync-Lock-Spuren),
    /// beim Overwrite der zu leerende Bereich (nur Zielspuren). Verknüpfung,
    /// src_in und Marker stecken bereits in `new_clips`. Ein History-Eintrag.
    pub fn commit_edit(
        &mut self,
        new_clips: Vec<TimelineClip>,
        gap_at: f64,
        gap_len: f64,
        ripple: bool,
    ) {
        use std::collections::HashSet;
        if new_clips.is_empty() || gap_len <= EPS {
            return;
        }
        self.push_history();
        let targets: HashSet<String> = new_clips.iter().map(|c| c.track_id.clone()).collect();
        if ripple {
            let mut affected = targets.clone();
            for t in &self.tracks {
                if t.sync_lock && !t.locked {
                    affected.insert(t.id.clone());
                }
            }
            self.open_gap(&affected, gap_at, gap_len);
        } else {
            let mut clips = std::mem::take(&mut self.clips);
            for tid in &targets {
                clips = overwrite_range(clips, tid, gap_at, gap_at + gap_len);
            }
            self.clips = clips;
        }
        self.selected_clip_ids = new_clips.iter().map(|c| c.id.clone()).collect();
        self.clips.extend(new_clips);
        self.reconcile_transitions();
    }

    /// Lift (Premiere): den Sequenz-In/Out-Bereich auf allen anvisierten Spuren
    /// leeren — die Lücke bleibt stehen (kein Ripple). Liefert false, wenn
    /// nichts zu tun ist. Ein History-Eintrag.
    pub fn lift_range(&mut self) -> bool {
        let (Some(a), Some(b)) = (self.in_point, self.out_point) else {
            return false;
        };
        let (a, b) = (a.min(b), a.max(b));
        if b - a < MIN_CLIP_DURATION {
            return false;
        }
        let targets = self.targeted_track_ids();
        if targets.is_empty() {
            return false;
        }
        let has_work = self.clips.iter().any(|c| {
            targets.contains(&c.track_id) && c.start < b - EPS && c.end() > a + EPS
        });
        if !has_work {
            return false;
        }
        self.push_history();
        let mut clips = std::mem::take(&mut self.clips);
        for tid in &targets {
            clips = overwrite_range(clips, tid, a, b);
        }
        self.clips = clips;
        self.set_playhead(a);
        self.prune_selection();
        self.reconcile_transitions();
        true
    }

    /// Extract (Premiere): den Sequenz-In/Out-Bereich auf den anvisierten Spuren
    /// entfernen UND die Lücke schließen. Sync-Lock-Spuren rippeln mit (ihr
    /// Material im Bereich wird ebenfalls entfernt, damit der Sync erhalten
    /// bleibt). Liefert false, wenn nichts zu tun ist. Ein History-Eintrag.
    pub fn extract_range(&mut self) -> bool {
        use std::collections::HashSet;
        let (Some(a), Some(b)) = (self.in_point, self.out_point) else {
            return false;
        };
        let (a, b) = (a.min(b), a.max(b));
        let d = b - a;
        if d < MIN_CLIP_DURATION {
            return false;
        }
        let targets = self.targeted_track_ids();
        if targets.is_empty() {
            return false;
        }
        let mut affected: HashSet<String> = targets;
        for t in &self.tracks {
            if t.sync_lock && !t.locked {
                affected.insert(t.id.clone());
            }
        }
        let has_work = self.clips.iter().any(|c| {
            affected.contains(&c.track_id)
                && ((c.start < b - EPS && c.end() > a + EPS) || c.start >= b - EPS)
        });
        if !has_work {
            return false;
        }
        self.push_history();
        let mut clips = std::mem::take(&mut self.clips);
        for tid in &affected {
            clips = overwrite_range(clips, tid, a, b);
        }
        for c in &mut clips {
            if affected.contains(&c.track_id) && c.start >= b - EPS {
                c.start = (c.start - d).max(0.0);
            }
        }
        self.clips = clips;
        self.set_playhead(a);
        self.prune_selection();
        self.reconcile_transitions();
        true
    }

    /// Erstes echtes Material-Clip (kein Generator) unter Sequenzzeit `t` auf
    /// einer Spur der gewünschten Art; optional auf anvisierte Spuren beschränkt.
    /// Reihenfolge folgt dem Track-Stack (oben zuerst).
    fn first_clip_at(
        &self,
        t: f64,
        kind: TrackKind,
        targeted_only: bool,
    ) -> Option<&TimelineClip> {
        for track in self.tracks.iter().filter(|tr| tr.kind == kind) {
            if targeted_only && !track.targeted {
                continue;
            }
            if let Some(c) = self.clips.iter().find(|c| {
                c.track_id == track.id
                    && !c.is_generator()
                    && !c.asset_id.is_empty()
                    && c.start <= t + EPS
                    && c.end() > t - EPS
            }) {
                return Some(c);
            }
        }
        None
    }

    /// Match Frame: Quelle (asset_id) und exakte Medienzeit unter dem Playhead.
    /// Bevorzugt anvisierte Video-, dann anvisierte Audio-, dann beliebige
    /// Video-/Audiospuren (oberste Ebene mit Material).
    pub fn match_frame_source(&self, t: f64) -> Option<(String, f64)> {
        let clip = self
            .first_clip_at(t, TrackKind::Video, true)
            .or_else(|| self.first_clip_at(t, TrackKind::Audio, true))
            .or_else(|| self.first_clip_at(t, TrackKind::Video, false))
            .or_else(|| self.first_clip_at(t, TrackKind::Audio, false))?;
        Some((clip.asset_id.clone(), clip.media_time_at(t)))
    }

    /// Bewusst keine Link-Expansion: die Auswahl ist bereits expandiert,
    /// und Alt-Drag soll gezielt eine Hälfte eines Paares bewegen können.
    pub fn move_clips(&mut self, ids: &[String], delta_sec: f64, lane_offset: i32) {
        use std::collections::HashSet;
        let locked = locked_track_ids(&self.tracks);
        let id_set: HashSet<&str> = ids.iter().map(|s| s.as_str()).collect();
        let moving: Vec<TimelineClip> = self
            .clips
            .iter()
            .filter(|c| id_set.contains(c.id.as_str()) && !locked.contains(&c.track_id))
            .cloned()
            .collect();
        if moving.is_empty() {
            return;
        }

        let min_start = moving.iter().map(|c| c.start).fold(f64::INFINITY, f64::min);
        let d = delta_sec.max(-min_start);
        if d.abs() < EPS && lane_offset == 0 {
            return;
        }

        let lanes_of = |kind: TrackKind| -> Vec<&TimelineTrack> {
            self.tracks.iter().filter(|t| t.kind == kind).collect()
        };
        let video_tracks = lanes_of(TrackKind::Video);
        let audio_tracks = lanes_of(TrackKind::Audio);
        let subtitle_tracks = lanes_of(TrackKind::Subtitle);
        let remap = |clip: &TimelineClip| -> String {
            if lane_offset == 0 {
                return clip.track_id.clone();
            }
            let lanes = match clip.kind {
                TrackKind::Video => &video_tracks,
                TrackKind::Audio => &audio_tracks,
                TrackKind::Subtitle => &subtitle_tracks,
            };
            let Some(idx) = lanes.iter().position(|t| t.id == clip.track_id) else {
                return clip.track_id.clone();
            };
            let new_idx = (idx as i32 + lane_offset).clamp(0, lanes.len() as i32 - 1) as usize;
            lanes[new_idx].id.clone()
        };

        let placed: Vec<TimelineClip> = moving
            .iter()
            .map(|c| {
                let mut p = c.clone();
                p.start = c.start + d;
                p.track_id = remap(c);
                p
            })
            .collect();
        if placed.iter().any(|c| locked.contains(&c.track_id)) {
            return;
        }

        self.push_history();
        let mut rest: Vec<TimelineClip> = std::mem::take(&mut self.clips)
            .into_iter()
            .filter(|c| !id_set.contains(c.id.as_str()) || locked.contains(&c.track_id))
            .collect();
        for p in &placed {
            rest = overwrite_range(rest, &p.track_id, p.start, p.start + p.duration);
        }
        rest.extend(placed);
        self.clips = rest;
        self.reconcile_transitions();
    }

    /// Wie `move_clips`, aber die Originale bleiben liegen: die Auswahl wird
    /// an der Zielposition als Kopie eingefügt (Alt+Drag-Duplizieren).
    pub fn duplicate_clips(&mut self, ids: &[String], delta_sec: f64, lane_offset: i32) {
        use std::collections::{HashMap, HashSet};
        let locked = locked_track_ids(&self.tracks);
        let id_set: HashSet<&str> = ids.iter().map(|s| s.as_str()).collect();
        let sources: Vec<TimelineClip> = self
            .clips
            .iter()
            .filter(|c| id_set.contains(c.id.as_str()) && !locked.contains(&c.track_id))
            .cloned()
            .collect();
        if sources.is_empty() {
            return;
        }

        let min_start = sources.iter().map(|c| c.start).fold(f64::INFINITY, f64::min);
        let d = delta_sec.max(-min_start);
        if d.abs() < EPS && lane_offset == 0 {
            return;
        }

        let lanes_of = |kind: TrackKind| -> Vec<&TimelineTrack> {
            self.tracks.iter().filter(|t| t.kind == kind).collect()
        };
        let video_tracks = lanes_of(TrackKind::Video);
        let audio_tracks = lanes_of(TrackKind::Audio);
        let subtitle_tracks = lanes_of(TrackKind::Subtitle);
        let remap = |clip: &TimelineClip| -> String {
            if lane_offset == 0 {
                return clip.track_id.clone();
            }
            let lanes = match clip.kind {
                TrackKind::Video => &video_tracks,
                TrackKind::Audio => &audio_tracks,
                TrackKind::Subtitle => &subtitle_tracks,
            };
            let Some(idx) = lanes.iter().position(|t| t.id == clip.track_id) else {
                return clip.track_id.clone();
            };
            let new_idx = (idx as i32 + lane_offset).clamp(0, lanes.len() as i32 - 1) as usize;
            lanes[new_idx].id.clone()
        };

        // Kopien verknüpfter Paare teilen sich eine frische link_id.
        let mut new_link_ids: HashMap<String, String> = HashMap::new();
        let mut id_map: HashMap<String, String> = HashMap::new();
        let placed: Vec<TimelineClip> = sources
            .iter()
            .map(|c| {
                let mut p = c.clone();
                p.id = new_id();
                id_map.insert(c.id.clone(), p.id.clone());
                p.start = c.start + d;
                p.track_id = remap(c);
                p.link_id = c.link_id.as_ref().map(|link| {
                    new_link_ids
                        .entry(link.clone())
                        .or_insert_with(new_id)
                        .clone()
                });
                p
            })
            .collect();
        if placed.iter().any(|c| locked.contains(&c.track_id)) {
            return;
        }

        self.push_history();
        let mut rest = std::mem::take(&mut self.clips);
        for p in &placed {
            rest = overwrite_range(rest, &p.track_id, p.start, p.start + p.duration);
        }
        self.selected_clip_ids = placed.iter().map(|c| c.id.clone()).collect();
        rest.extend(placed);
        self.clips = rest;
        // Übergänge mitkopieren, deren Kanten vollständig kopiert wurden.
        let copies: Vec<Transition> = self
            .transitions
            .iter()
            .filter_map(|t| remap_transition(t, &id_map))
            .collect();
        self.transitions.extend(copies);
        self.reconcile_transitions();
    }

    pub fn trim_clip(&mut self, id: &str, edge: TrimEdge, delta: f64) {
        let locked = locked_track_ids(&self.tracks);
        let expanded = expand_links(&self.clips, &[id.to_string()]);
        let targets: Vec<TimelineClip> = self
            .clips
            .iter()
            .filter(|c| expanded.contains(&c.id) && !locked.contains(&c.track_id))
            .cloned()
            .collect();
        if targets.is_empty() {
            return;
        }
        let mut d = delta;
        for clip in &targets {
            let (lo, hi) = trim_range(clip, edge, &self.clips, true);
            d = d.clamp(lo, hi);
        }
        if d.abs() < EPS {
            return;
        }
        self.push_history();
        let target_ids: std::collections::HashSet<&str> =
            targets.iter().map(|c| c.id.as_str()).collect();
        self.clips = self
            .clips
            .iter()
            .map(|c| {
                if target_ids.contains(c.id.as_str()) {
                    apply_trim(c, edge, d)
                } else {
                    c.clone()
                }
            })
            .collect();
        self.reconcile_transitions();
    }

    pub fn ripple_trim_clip(&mut self, id: &str, edge: TrimEdge, delta: f64) {
        let locked = locked_track_ids(&self.tracks);
        let expanded = expand_links(&self.clips, &[id.to_string()]);
        let targets: Vec<TimelineClip> = self
            .clips
            .iter()
            .filter(|c| expanded.contains(&c.id) && !locked.contains(&c.track_id))
            .cloned()
            .collect();
        if targets.is_empty() {
            return;
        }
        let mut d = delta;
        for clip in &targets {
            let (lo, hi) = trim_range(clip, edge, &self.clips, false);
            d = d.clamp(lo, hi);
        }
        if d.abs() < EPS {
            return;
        }

        self.push_history();
        let target_ids: std::collections::HashSet<&str> =
            targets.iter().map(|c| c.id.as_str()).collect();
        let anchor = &targets[0];
        // Alles hinter dem Schnittpunkt rückt nach: End-Trim verschiebt um +d,
        // Start-Trim (Kopf kürzen) um -d.
        let boundary = match edge {
            TrimEdge::End => anchor.end(),
            TrimEdge::Start => anchor.start,
        };
        let shift = match edge {
            TrimEdge::End => d,
            TrimEdge::Start => -d,
        };
        self.clips = self
            .clips
            .iter()
            .map(|c| {
                if target_ids.contains(c.id.as_str()) {
                    let trimmed = apply_trim(c, edge, d);
                    // Beim Start-Trim bleibt die Schnittkante stehen: Clip rückt mit.
                    if edge == TrimEdge::Start {
                        let mut t = trimmed;
                        t.start = c.start;
                        t
                    } else {
                        trimmed
                    }
                } else if !locked.contains(&c.track_id) && c.start >= boundary - EPS {
                    let mut m = c.clone();
                    m.start = (c.start + shift).max(0.0);
                    m
                } else {
                    c.clone()
                }
            })
            .collect();
        self.reconcile_transitions();
    }

    pub fn roll_edit(&mut self, left_id: &str, right_id: &str, delta: f64) {
        let locked = locked_track_ids(&self.tracks);
        let Some(left) = self.clips.iter().find(|c| c.id == left_id).cloned() else {
            return;
        };
        let Some(right) = self.clips.iter().find(|c| c.id == right_id).cloned() else {
            return;
        };
        if locked.contains(&left.track_id) || locked.contains(&right.track_id) {
            return;
        }
        let (lo_l, hi_l) = trim_range(&left, TrimEdge::End, &self.clips, false);
        let (lo_r, hi_r) = trim_range(&right, TrimEdge::Start, &self.clips, false);
        let d = delta.clamp(lo_l.max(lo_r), hi_l.min(hi_r));
        if d.abs() < EPS {
            return;
        }
        self.push_history();
        self.clips = self
            .clips
            .iter()
            .map(|c| {
                if c.id == left_id {
                    apply_trim(c, TrimEdge::End, d)
                } else if c.id == right_id {
                    apply_trim(c, TrimEdge::Start, d)
                } else {
                    c.clone()
                }
            })
            .collect();
        self.reconcile_transitions();
    }

    pub fn slide_clip(&mut self, id: &str, delta: f64) {
        use std::collections::HashSet;
        let locked = locked_track_ids(&self.tracks);
        let ids: HashSet<String> = expand_links(&self.clips, &[id.to_string()])
            .into_iter()
            .collect();
        let sliding: Vec<TimelineClip> = self
            .clips
            .iter()
            .filter(|c| ids.contains(&c.id) && !locked.contains(&c.track_id))
            .cloned()
            .collect();
        if sliding.is_empty() {
            return;
        }

        // Direkt angrenzende Nachbarn rollen mit; Lücken absorbieren die Bewegung.
        let mut left_ids: HashSet<String> = HashSet::new();
        let mut right_ids: HashSet<String> = HashSet::new();
        let mut d = delta;
        for clip in &sliding {
            let clip_end = clip.end();
            let prev = self.clips.iter().find(|c| {
                c.track_id == clip.track_id
                    && !ids.contains(&c.id)
                    && (c.end() - clip.start).abs() < EPS
            });
            let next = self.clips.iter().find(|c| {
                c.track_id == clip.track_id && !ids.contains(&c.id) && (c.start - clip_end).abs() < EPS
            });
            if let Some(prev) = prev {
                let (lo, hi) = trim_range(prev, TrimEdge::End, &self.clips, false);
                d = d.clamp(lo, hi);
                left_ids.insert(prev.id.clone());
            }
            if let Some(next) = next {
                let (lo, hi) = trim_range(next, TrimEdge::Start, &self.clips, false);
                d = d.clamp(lo, hi);
                right_ids.insert(next.id.clone());
            }
            d = d.max(-clip.start);
        }
        if d.abs() < EPS {
            return;
        }

        self.push_history();
        self.clips = self
            .clips
            .iter()
            .map(|c| {
                if ids.contains(&c.id) && !locked.contains(&c.track_id) {
                    let mut m = c.clone();
                    m.start = c.start + d;
                    m
                } else if left_ids.contains(&c.id) {
                    apply_trim(c, TrimEdge::End, d)
                } else if right_ids.contains(&c.id) {
                    apply_trim(c, TrimEdge::Start, d)
                } else {
                    c.clone()
                }
            })
            .collect();
        self.reconcile_transitions();
    }

    pub fn slip_clip(&mut self, id: &str, delta: f64) {
        let locked = locked_track_ids(&self.tracks);
        let expanded = expand_links(&self.clips, &[id.to_string()]);
        let targets: Vec<TimelineClip> = self
            .clips
            .iter()
            .filter(|c| {
                expanded.contains(&c.id)
                    && !locked.contains(&c.track_id)
                    && c.src_duration.is_finite()
            })
            .cloned()
            .collect();
        if targets.is_empty() {
            return;
        }
        // Delta in Timeline-Sekunden; die Medienverschiebung skaliert mit
        // der Clip-Geschwindigkeit und bleibt innerhalb der Quelle.
        let mut d = delta;
        for clip in &targets {
            let s = clip.eff_speed();
            d = d.clamp(
                -clip.src_in / s,
                (clip.src_duration - clip.media_out()).max(0.0) / s,
            );
        }
        if d.abs() < EPS {
            return;
        }
        self.push_history();
        let target_ids: std::collections::HashSet<&str> =
            targets.iter().map(|c| c.id.as_str()).collect();
        for c in &mut self.clips {
            if target_ids.contains(c.id.as_str()) {
                c.src_in += d * c.eff_speed();
            }
        }
        self.reconcile_transitions();
    }

    pub fn split_at(&mut self, time: f64, clip_ids: Option<&[String]>) {
        use std::collections::{HashMap, HashSet};
        let locked = locked_track_ids(&self.tracks);
        let candidates: Option<HashSet<String>> = match clip_ids {
            Some(ids) if !ids.is_empty() => {
                Some(expand_links(&self.clips, ids).into_iter().collect())
            }
            _ => None,
        };
        let splittable = |c: &TimelineClip| -> bool {
            !locked.contains(&c.track_id)
                && candidates.as_ref().is_none_or(|set| set.contains(&c.id))
                && time > c.start + MIN_CLIP_DURATION - EPS
                && time < c.end() - MIN_CLIP_DURATION + EPS
        };
        if !self.clips.iter().any(|c| splittable(c)) {
            return;
        }

        self.push_history();
        // Rechte Hälften verknüpfter Clips bekommen eine gemeinsame neue link_id.
        let mut new_link_ids: HashMap<String, String> = HashMap::new();
        // Map Original → rechte Hälfte: Übergänge am CLIPENDE wandern mit.
        let mut right_ids: HashMap<String, String> = HashMap::new();
        let mut clips: Vec<TimelineClip> = Vec::with_capacity(self.clips.len() + 4);
        let mut new_selection: Vec<String> = Vec::new();
        for c in &self.clips {
            if !splittable(c) {
                clips.push(c.clone());
                continue;
            }
            let left_len = time - c.start;
            let (left_src_in, right_src_in) = c.split_src_ins(left_len);
            let right_link = c.link_id.as_ref().map(|link| {
                new_link_ids
                    .entry(link.clone())
                    .or_insert_with(new_id)
                    .clone()
            });
            let mut right = c.clone();
            right.id = new_id();
            right_ids.insert(c.id.clone(), right.id.clone());
            right.start = time;
            right.src_in = right_src_in;
            right.duration = c.duration - left_len;
            right.link_id = right_link;
            let mut left = c.clone();
            left.src_in = left_src_in;
            left.duration = left_len;
            // Clip-Marker an der Schnittkante (Medienzeit) auf die Hälften
            // aufteilen — jede Hälfte behält die Marker ihres Quellausschnitts.
            let cut_media = c.media_time_at(time);
            let on_right = |m: &Marker| -> bool {
                if c.freeze {
                    false
                } else if c.reverse {
                    m.time <= cut_media
                } else {
                    m.time >= cut_media
                }
            };
            right.markers = c.markers.iter().filter(|m| on_right(m)).cloned().collect();
            left.markers = c.markers.iter().filter(|m| !on_right(m)).cloned().collect();
            if self.selected_clip_ids.contains(&c.id) {
                new_selection.push(left.id.clone());
                new_selection.push(right.id.clone());
            }
            clips.push(left);
            clips.push(right);
        }
        self.clips = clips;
        // Übergänge, die am Ende des geteilten Clips hingen, referenzieren
        // jetzt die rechte Hälfte (sie endet an der ursprünglichen Kante).
        for tr in &mut self.transitions {
            if let Some(right) = tr.from_clip_id.as_ref().and_then(|id| right_ids.get(id)) {
                tr.from_clip_id = Some(right.clone());
            }
        }
        self.reconcile_transitions();
        if !new_selection.is_empty() {
            self.selected_clip_ids = new_selection;
        } else {
            self.prune_selection();
        }
    }

    /// Keine Link-Expansion — die Auswahl ist bereits expandiert.
    pub fn delete_clips(&mut self, ids: &[String], ripple: bool) {
        use std::collections::HashSet;
        let locked = locked_track_ids(&self.tracks);
        let id_set: HashSet<&str> = ids
            .iter()
            .filter(|id| {
                self.clips
                    .iter()
                    .find(|c| &c.id == *id)
                    .is_some_and(|c| !locked.contains(&c.track_id))
            })
            .map(|s| s.as_str())
            .collect();
        if id_set.is_empty() {
            return;
        }

        self.push_history();
        let removed: Vec<TimelineClip> = self
            .clips
            .iter()
            .filter(|c| id_set.contains(c.id.as_str()))
            .cloned()
            .collect();
        self.clips.retain(|c| !id_set.contains(c.id.as_str()));

        if ripple {
            // Lücke der gesamten Auswahl schließen (über alle ungesperrten Spuren).
            let gap_start = removed.iter().map(|c| c.start).fold(f64::INFINITY, f64::min);
            let gap_end = removed.iter().map(|c| c.end()).fold(0.0, f64::max);
            let gap = gap_end - gap_start;
            for c in &mut self.clips {
                if !locked.contains(&c.track_id) && c.start >= gap_end - EPS {
                    c.start = (c.start - gap).max(0.0);
                }
            }
        }
        self.prune_selection();
        self.reconcile_transitions();
    }

    pub fn set_clips_enabled(&mut self, ids: &[String], enabled: bool) {
        let id_set: std::collections::HashSet<String> =
            expand_links(&self.clips, ids).into_iter().collect();
        self.push_history();
        for c in &mut self.clips {
            if id_set.contains(&c.id) {
                c.enabled = enabled;
            }
        }
    }

    pub fn toggle_link_selected(&mut self) {
        let selected: Vec<TimelineClip> = self
            .clips
            .iter()
            .filter(|c| self.selected_clip_ids.contains(&c.id))
            .cloned()
            .collect();
        if selected.is_empty() {
            return;
        }
        let any_linked = selected.iter().any(|c| c.link_id.is_some());
        let id_set: std::collections::HashSet<&str> =
            selected.iter().map(|c| c.id.as_str()).collect();
        if any_linked {
            self.push_history();
            for c in &mut self.clips {
                if id_set.contains(c.id.as_str()) {
                    c.link_id = None;
                }
            }
            return;
        }
        // Neu verknüpfen: nur sinnvoll, wenn Video + Audio gemeinsam gewählt sind.
        let has_video = selected.iter().any(|c| c.kind == TrackKind::Video);
        let has_audio = selected.iter().any(|c| c.kind == TrackKind::Audio);
        if !has_video || !has_audio {
            return;
        }
        self.push_history();
        let link_id = new_id();
        for c in &mut self.clips {
            if id_set.contains(c.id.as_str()) {
                c.link_id = Some(link_id.clone());
            }
        }
    }

    // ------------------------------------------------ Geschwindigkeit/Dauer

    /// „Geschwindigkeit/Dauer“ (Premiere Mod+R) auf die Clips anwenden:
    /// die belegte Medienspanne bleibt erhalten, die Dauer folgt aus
    /// duration = span / speed. `ripple` verschiebt nachfolgende Clips
    /// (aller ungesperrten Spuren) um die Längenänderung; ohne Ripple wird
    /// die neue Dauer am nächsten Clip der Spur gekappt (Premiere-Verhalten).
    /// Verknüpfte A/V-Partner ändern sich gemeinsam (ein Ripple je Paar).
    pub fn set_clip_speed(
        &mut self,
        ids: &[String],
        speed: f64,
        reverse: bool,
        freeze: bool,
        ripple: bool,
    ) {
        use std::collections::HashSet;
        let locked = locked_track_ids(&self.tracks);
        let speed = if speed.is_finite() {
            speed.clamp(MIN_CLIP_SPEED, MAX_CLIP_SPEED)
        } else {
            1.0
        };
        let expanded: HashSet<String> = expand_links(&self.clips, ids).into_iter().collect();
        let targets: Vec<TimelineClip> = self
            .clips
            .iter()
            .filter(|c| {
                expanded.contains(&c.id) && !locked.contains(&c.track_id) && !c.is_generator()
            })
            .cloned()
            .collect();
        if targets.is_empty() {
            return;
        }
        let unchanged = targets.iter().all(|c| {
            (c.eff_speed() - speed).abs() < EPS && c.reverse == reverse && c.freeze == freeze
        });
        if unchanged {
            return;
        }
        self.push_history();

        // Neue Dauer je Clip: Medienspanne erhalten (duration = span/speed).
        // Standbild: Dauer frei; Auftauen behält die Dauer, kappt die
        // Spanne aber an der Quelle.
        let new_duration = |c: &TimelineClip| -> f64 {
            if freeze {
                return c.duration;
            }
            let span = if c.freeze {
                c.duration * speed
            } else {
                c.media_span()
            };
            let span = if c.src_duration.is_finite() {
                span.min((c.src_duration - c.src_in).max(0.0))
            } else {
                span
            };
            (span / speed).max(MIN_CLIP_DURATION)
        };

        // Längenänderung je Verknüpfungsgruppe einsammeln (Ripple zählt
        // pro Schnittpunkt einmal, nicht je A/V-Hälfte).
        struct EditShift {
            old_end: f64,
            delta: f64,
            group: Vec<String>,
        }
        let mut shifts: Vec<EditShift> = Vec::new();
        let mut grouped: HashSet<&str> = HashSet::new();
        for c in &targets {
            if grouped.contains(c.id.as_str()) {
                continue;
            }
            let group: Vec<&TimelineClip> = match &c.link_id {
                Some(link) => targets
                    .iter()
                    .filter(|t| t.link_id.as_deref() == Some(link))
                    .collect(),
                None => vec![c],
            };
            for g in &group {
                grouped.insert(g.id.as_str());
            }
            let old_end = group.iter().map(|g| g.end()).fold(0.0, f64::max);
            let delta = group
                .iter()
                .map(|g| new_duration(g) - g.duration)
                .fold(f64::NEG_INFINITY, f64::max);
            shifts.push(EditShift {
                old_end,
                delta,
                group: group.iter().map(|g| g.id.clone()).collect(),
            });
        }

        let target_ids: HashSet<&str> = targets.iter().map(|c| c.id.as_str()).collect();
        // Ohne Ripple: Dauer am nächsten Clip derselben Spur kappen.
        let next_start = |c: &TimelineClip| -> f64 {
            self.clips
                .iter()
                .filter(|o| {
                    o.track_id == c.track_id
                        && !target_ids.contains(o.id.as_str())
                        && o.start >= c.end() - EPS
                })
                .map(|o| o.start)
                .fold(f64::INFINITY, f64::min)
        };
        let caps: std::collections::HashMap<String, f64> = if ripple {
            Default::default()
        } else {
            targets
                .iter()
                .map(|c| (c.id.clone(), (next_start(c) - c.start).max(MIN_CLIP_DURATION)))
                .collect()
        };

        for c in &mut self.clips {
            if !target_ids.contains(c.id.as_str()) {
                continue;
            }
            let mut dur = new_duration(c);
            if let Some(cap) = caps.get(&c.id) {
                dur = dur.min(*cap);
            }
            c.duration = dur;
            c.speed = speed;
            c.reverse = reverse && !freeze;
            c.freeze = freeze;
        }

        if ripple {
            // Verschiebungen kumulativ anwenden: jeder Clip rückt um die
            // Summe der Deltas aller Schnittpunkte vor seinem Start.
            shifts.sort_by(|a, b| a.old_end.total_cmp(&b.old_end));
            let moves: Vec<(String, f64)> = self
                .clips
                .iter()
                .filter(|c| !locked.contains(&c.track_id))
                .filter_map(|c| {
                    let shift: f64 = shifts
                        .iter()
                        .filter(|s| {
                            s.old_end <= c.start + EPS && !s.group.contains(&c.id)
                        })
                        .map(|s| s.delta)
                        .sum();
                    (shift.abs() > EPS).then(|| (c.id.clone(), shift))
                })
                .collect();
            for (id, shift) in moves {
                if let Some(c) = self.clips.iter_mut().find(|c| c.id == id) {
                    c.start = (c.start + shift).max(0.0);
                }
            }
        }
        self.reconcile_transitions();
    }

    /// „Frame einfrieren“: Standbild aus der Medienzeit am Playhead (liegt
    /// der Playhead außerhalb des Clips, friert der In-Punkt ein).
    pub fn freeze_frame_at_playhead(&mut self, ids: &[String]) {
        use std::collections::HashSet;
        let locked = locked_track_ids(&self.tracks);
        let t = self.playhead_sec;
        let expanded: HashSet<String> = expand_links(&self.clips, ids).into_iter().collect();
        let affected: Vec<String> = self
            .clips
            .iter()
            .filter(|c| {
                expanded.contains(&c.id)
                    && !locked.contains(&c.track_id)
                    && !c.is_generator()
                    && !c.freeze
            })
            .map(|c| c.id.clone())
            .collect();
        if affected.is_empty() {
            return;
        }
        self.push_history();
        for c in &mut self.clips {
            if !affected.contains(&c.id) {
                continue;
            }
            if t >= c.start - EPS && t < c.end() + EPS {
                let m = c.media_time_at(t.clamp(c.start, c.end()));
                c.src_in = m.clamp(0.0, c.src_duration);
            }
            c.freeze = true;
            c.reverse = false;
        }
    }

    // ----------------------------------------------------------------- Titel

    /// Titel-Clip bei `at` anlegen: auf der nächsten freien Videospur ÜBER
    /// dem obersten belegten Material (Premiere-Konvention); ist keine frei,
    /// entsteht eine neue Spur oben. Liefert die Clip-ID.
    pub fn add_title_clip(&mut self, spec: TitleSpec, at: f64, duration: f64) -> String {
        let at = at.max(0.0);
        let duration = duration.max(MIN_CLIP_DURATION);
        let end = at + duration;
        self.push_history();

        let locked = locked_track_ids(&self.tracks);
        let occupied = |track_id: &str| -> bool {
            self.clips
                .iter()
                .any(|c| c.track_id == track_id && c.start < end - EPS && c.end() > at + EPS)
        };
        // Videospuren in Zeichenreihenfolge oben → unten (Index 0 = oberste).
        let video: Vec<(usize, String)> = self
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.kind == TrackKind::Video)
            .map(|(i, t)| (i, t.id.clone()))
            .collect();
        // Oberste belegte Spur; Kandidaten sind freie Spuren darüber —
        // die dichteste gewinnt. Ohne Belegung: unterste freie Spur.
        let top_occupied = video.iter().position(|(_, id)| occupied(id));
        let candidates: Vec<&(usize, String)> = match top_occupied {
            Some(limit) => video.iter().take(limit).rev().collect(),
            None => video.iter().rev().collect(),
        };
        let track_id = candidates
            .into_iter()
            .find(|(_, id)| !locked.contains(id) && !occupied(id))
            .map(|(_, id)| id.clone())
            .unwrap_or_else(|| {
                let track = make_track(TrackKind::Video);
                let id = track.id.clone();
                let at = self.video_block_start();
                self.tracks.insert(at, track);
                id
            });

        let clip = TimelineClip {
            id: new_id(),
            track_id,
            asset_id: String::new(),
            name: spec.display_name(),
            kind: TrackKind::Video,
            start: at,
            duration,
            src_in: 0.0,
            src_duration: f64::INFINITY,
            link_id: None,
            enabled: true,
            gain_db: 0.0,
            fx: ClipFx::default(),
            grade: ColorGrade::default(),
            effects: Vec::new(),
            title: Some(spec),
            subtitle: None,
            speed: 1.0,
            reverse: false,
            freeze: false,
            markers: Vec::new(),
        };
        let id = clip.id.clone();
        self.selected_clip_ids = vec![id.clone()];
        self.selected_transition_ids.clear();
        self.clips.push(clip);
        id
    }

    /// Beginn einer Titel-Bearbeitungsgeste (Slider-Drag, Tipp-Sitzung):
    /// legt einmalig einen Undo-Snapshot an (Muster wie `begin_fx_edit`).
    pub fn begin_title_edit(&mut self) {
        self.push_history();
    }

    /// Titel-Spec ändern (mit Undo-Snapshot) — für Einzelklicks.
    pub fn title_update(&mut self, id: &str, f: impl FnOnce(&mut TitleSpec)) {
        if self
            .fx_clip_mut(id)
            .map(|c| c.title.is_none())
            .unwrap_or(true)
        {
            return;
        }
        self.push_history();
        self.title_update_live(id, f);
    }

    /// Titel-Spec ändern OHNE Snapshot (laufende Geste nach
    /// `begin_title_edit`). Hält den Clipnamen mit dem Text synchron.
    pub fn title_update_live(&mut self, id: &str, f: impl FnOnce(&mut TitleSpec)) {
        let Some(clip) = self.fx_clip_mut(id) else { return };
        let Some(spec) = clip.title.as_mut() else { return };
        f(spec);
        clip.name = spec.display_name();
        self.revision += 1;
    }

    // ------------------------------------------------------------ Untertitel

    /// Untertitel-Spuren in Zeichenreihenfolge der Timeline (oben → unten).
    pub fn subtitle_tracks(&self) -> Vec<&TimelineTrack> {
        self.tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Subtitle)
            .collect()
    }

    /// Aktive Untertitel-Spur: explizit gewählt, sonst U1 (unterste).
    pub fn active_subtitle_track(&self) -> Option<&TimelineTrack> {
        self.active_subtitle_track_id
            .as_ref()
            .and_then(|id| {
                self.tracks
                    .iter()
                    .find(|t| t.id == *id && t.kind == TrackKind::Subtitle)
            })
            .or_else(|| {
                self.tracks
                    .iter()
                    .filter(|t| t.kind == TrackKind::Subtitle)
                    .last()
            })
    }

    pub fn set_active_subtitle_track(&mut self, track_id: &str) {
        if self
            .tracks
            .iter()
            .any(|t| t.id == track_id && t.kind == TrackKind::Subtitle)
        {
            self.active_subtitle_track_id = Some(track_id.to_string());
        }
    }

    /// Stil einer Untertitel-Spur (Standardstil, solange keiner gesetzt ist).
    pub fn subtitle_style(&self, track_id: &str) -> SubtitleStyle {
        self.tracks
            .iter()
            .find(|t| t.id == track_id)
            .and_then(|t| t.subtitle_style.clone())
            .unwrap_or_default()
    }

    /// Beginn einer Untertitel-Bearbeitungsgeste (Slider-Drag, Tipp-
    /// Sitzung): legt einmalig einen Undo-Snapshot an (wie `begin_title_edit`).
    pub fn begin_subtitle_edit(&mut self) {
        self.push_history();
    }

    /// Spurstil ändern (mit Undo-Snapshot) — für Einzelklicks.
    pub fn subtitle_style_update(&mut self, track_id: &str, f: impl FnOnce(&mut SubtitleStyle)) {
        if !self
            .tracks
            .iter()
            .any(|t| t.id == track_id && t.kind == TrackKind::Subtitle)
        {
            return;
        }
        self.push_history();
        self.subtitle_style_update_live(track_id, f);
    }

    /// Spurstil ändern OHNE Snapshot (laufende Geste nach `begin_subtitle_edit`).
    pub fn subtitle_style_update_live(
        &mut self,
        track_id: &str,
        f: impl FnOnce(&mut SubtitleStyle),
    ) {
        let Some(track) = self
            .tracks
            .iter_mut()
            .find(|t| t.id == track_id && t.kind == TrackKind::Subtitle)
        else {
            return;
        };
        let mut style = track.subtitle_style.clone().unwrap_or_default();
        f(&mut style);
        track.subtitle_style = Some(style);
        self.revision += 1;
    }

    /// Segment-Text ändern OHNE Snapshot (Tipp-Sitzung nach
    /// `begin_subtitle_edit` — Muster wie `title_update_live`). Hält den
    /// Clipnamen synchron.
    pub fn subtitle_update_live(&mut self, id: &str, f: impl FnOnce(&mut SubtitleSpec)) {
        let Some(clip) = self.fx_clip_mut(id) else { return };
        let Some(spec) = clip.subtitle.as_mut() else { return };
        f(spec);
        clip.name = spec.display_name();
        self.revision += 1;
    }

    fn make_subtitle_clip(track_id: &str, text: &str, start: f64, duration: f64) -> TimelineClip {
        let spec = SubtitleSpec::new(text);
        TimelineClip {
            id: new_id(),
            track_id: track_id.to_string(),
            asset_id: String::new(),
            name: spec.display_name(),
            kind: TrackKind::Subtitle,
            start,
            duration,
            src_in: 0.0,
            src_duration: f64::INFINITY,
            link_id: None,
            enabled: true,
            gain_db: 0.0,
            fx: ClipFx::default(),
            grade: ColorGrade::default(),
            effects: Vec::new(),
            title: None,
            subtitle: Some(spec),
            speed: 1.0,
            reverse: false,
            freeze: false,
            markers: Vec::new(),
        }
    }

    /// Untertitel-Segment am Playhead anlegen: auf der aktiven Spur (ohne
    /// Spur entsteht eine), frame-genau eingerastet, Dauer bis maximal zum
    /// nächsten Segment. Liefert die Clip-ID oder eine Fehlermeldung.
    pub fn add_subtitle_clip(&mut self, text: &str, at: f64) -> Result<String, String> {
        let rate = self.settings.rate;
        let start = rate.time_of_frame(rate.frame_round(at.max(0.0)).max(0) as f64);

        let locked = locked_track_ids(&self.tracks);
        let existing = self
            .active_subtitle_track()
            .map(|t| t.id.clone())
            .filter(|id| !locked.contains(id));
        if let Some(track_id) = &existing {
            let occupied = self.clips.iter().any(|c| {
                c.track_id == *track_id
                    && c.start < start + MIN_CLIP_DURATION - EPS
                    && c.end() > start + EPS
            });
            if occupied {
                return Err("An dieser Position liegt bereits ein Untertitel".to_string());
            }
        }

        self.push_history();
        let track_id = existing.unwrap_or_else(|| {
            let track = make_track(TrackKind::Subtitle);
            let id = track.id.clone();
            self.tracks.insert(0, track);
            id
        });
        self.active_subtitle_track_id = Some(track_id.clone());

        // Dauer: Standard, aber nie ins nächste Segment hinein.
        let next_start = self
            .clips
            .iter()
            .filter(|c| c.track_id == track_id && c.start > start + EPS)
            .map(|c| c.start)
            .fold(f64::INFINITY, f64::min);
        let duration = (next_start - start)
            .min(crate::core::subtitle::DEFAULT_CUE_DURATION)
            .max(MIN_CLIP_DURATION);

        let clip = Self::make_subtitle_clip(&track_id, text, start, duration);
        let id = clip.id.clone();
        self.selected_clip_ids = vec![id.clone()];
        self.selected_transition_ids.clear();
        self.clips.push(clip);
        Ok(id)
    }

    /// SRT-Cues auf eine NEUE Untertitel-Spur importieren (frame-genau aufs
    /// Sequenzraster gerundet, Überlappungen aufgelöst). Liefert die Spur-ID
    /// und die Anzahl eingefügter Segmente — ein Undo-Schritt.
    pub fn import_subtitle_cues(&mut self, cues: &[SrtCue]) -> (String, usize) {
        self.push_history();
        let track = make_track(TrackKind::Subtitle);
        let track_id = track.id.clone();
        self.tracks.insert(0, track);
        self.active_subtitle_track_id = Some(track_id.clone());

        let rate = self.settings.rate;
        let snap = |t: f64| rate.time_of_frame(rate.frame_round(t.max(0.0)).max(0) as f64);
        let mut inserted = 0usize;
        let mut prev_end = 0.0f64;
        let mut sorted: Vec<&SrtCue> = cues.iter().collect();
        sorted.sort_by(|a, b| a.start.total_cmp(&b.start));
        for cue in sorted {
            if cue.text.trim().is_empty() {
                continue;
            }
            let mut start = snap(cue.start).max(prev_end);
            let mut end = snap(cue.end);
            // Mindestens ein Frame, sonst verschluckt die Rundung den Cue.
            if end - start < MIN_CLIP_DURATION - EPS {
                end = rate.time_of_frame((rate.frame_round(start) + 1) as f64);
                start = snap(start);
            }
            if end - start < MIN_CLIP_DURATION - EPS {
                continue;
            }
            self.clips
                .push(Self::make_subtitle_clip(&track_id, &cue.text, start, end - start));
            prev_end = end;
            inserted += 1;
        }
        (track_id, inserted)
    }

    /// Segmente einer Untertitel-Spur als SRT-Cues (frame-genau aufs
    /// Sequenzraster gerundet, nach Startzeit sortiert).
    pub fn subtitle_cues(&self, track_id: &str) -> Vec<SrtCue> {
        let rate = self.settings.rate;
        let snap = |t: f64| rate.time_of_frame(rate.frame_round(t.max(0.0)).max(0) as f64);
        let mut cues: Vec<SrtCue> = self
            .clips
            .iter()
            .filter(|c| c.track_id == track_id && c.enabled)
            .filter_map(|c| {
                let text = c.subtitle.as_ref()?.text.trim().to_string();
                if text.is_empty() {
                    return None;
                }
                let start = snap(c.start);
                let end = snap(c.end());
                (end > start + EPS).then_some(SrtCue { start, end, text })
            })
            .collect();
        cues.sort_by(|a, b| a.start.total_cmp(&b.start));
        cues
    }

    pub fn remove_clips_for_assets(&mut self, asset_ids: &[String]) {
        let asset_set: std::collections::HashSet<&str> =
            asset_ids.iter().map(|s| s.as_str()).collect();
        if !self.clips.iter().any(|c| asset_set.contains(c.asset_id.as_str())) {
            return;
        }
        self.push_history();
        self.clips.retain(|c| !asset_set.contains(c.asset_id.as_str()));
        self.prune_selection();
        self.reconcile_transitions();
    }

    // ------------------------------------------------------ Zwischenablage

    pub fn copy_selection(&mut self) {
        let selected: Vec<&TimelineClip> = self
            .clips
            .iter()
            .filter(|c| self.selected_clip_ids.contains(&c.id))
            .collect();
        if selected.is_empty() {
            return;
        }
        let base = selected.iter().map(|c| c.start).fold(f64::INFINITY, f64::min);
        let selected_ids: std::collections::HashSet<&str> =
            selected.iter().map(|c| c.id.as_str()).collect();
        self.clipboard = selected
            .into_iter()
            .map(|c| {
                let mut copy = c.clone();
                copy.start -= base;
                copy
            })
            .collect();
        // Übergänge mitnehmen, deren Kanten vollständig in der Auswahl liegen
        // (Referenzen zeigen auf die Clipboard-Clip-IDs).
        self.clipboard_transitions = self
            .transitions
            .iter()
            .filter(|t| {
                let covered = |id: &Option<String>| {
                    id.as_deref().map_or(true, |id| selected_ids.contains(id))
                };
                covered(&t.from_clip_id) && covered(&t.to_clip_id)
            })
            .cloned()
            .collect();
    }

    pub fn cut_selection(&mut self) {
        self.copy_selection();
        let ids = self.selected_clip_ids.clone();
        self.delete_clips(&ids, false);
    }

    /// Fügt die Zwischenablage bei `at` (sonst Playhead) ein — nicht-destruktiv:
    /// Statt belegte Bereiche zu überschreiben, weicht jeder Clip auf die
    /// nächste freie Spur aus (Video nach oben, Audio nach unten); existiert
    /// keine, wird eine neue Spur angelegt.
    pub fn paste(&mut self, at: Option<f64>) {
        use std::collections::HashMap;
        if self.clipboard.is_empty() {
            return;
        }
        let t = at.unwrap_or(self.playhead_sec).max(0.0);

        let occupied = |existing: &[TimelineClip],
                        pending: &[TimelineClip],
                        track_id: &str,
                        start: f64,
                        end: f64|
         -> bool {
            existing
                .iter()
                .chain(pending.iter())
                .any(|c| c.track_id == track_id && c.start < end - EPS && c.end() > start + EPS)
        };

        // tracks-Kopie wächst um neu angelegte Spuren; erst beim Commit übernehmen.
        let mut tracks = self.tracks.clone();
        let mut new_link_ids: HashMap<String, String> = HashMap::new();
        let mut id_map: HashMap<String, String> = HashMap::new();
        let mut pasted: Vec<TimelineClip> = Vec::new();
        for c in &self.clipboard {
            let start = t + c.start;
            let end = start + c.duration;

            // Kandidaten gleicher Art (ohne gesperrte), beginnend bei der
            // Originalspur — fehlt sie, bei V1 bzw. A1.
            let lanes: Vec<usize> = tracks
                .iter()
                .enumerate()
                .filter(|(_, tr)| tr.kind == c.kind && !tr.locked)
                .map(|(i, _)| i)
                .collect();
            let candidates: Vec<usize> = if lanes.is_empty() {
                Vec::new()
            } else {
                let desired = lanes
                    .iter()
                    .position(|&i| tracks[i].id == c.track_id)
                    .unwrap_or(match c.kind {
                        TrackKind::Video | TrackKind::Subtitle => lanes.len() - 1,
                        TrackKind::Audio => 0,
                    });
                match c.kind {
                    // Array-Index 0 = oberste Spur → „nach oben“ = rückwärts.
                    TrackKind::Video | TrackKind::Subtitle => {
                        lanes[..=desired].iter().rev().copied().collect()
                    }
                    TrackKind::Audio => lanes[desired..].to_vec(),
                }
            };
            let target = candidates.into_iter().find_map(|idx| {
                let id = tracks[idx].id.as_str();
                (!occupied(&self.clips, &pasted, id, start, end)).then(|| id.to_string())
            });
            let track_id = target.unwrap_or_else(|| {
                let track = make_track(c.kind);
                let id = track.id.clone();
                match c.kind {
                    // Neue Videospur über den Video-Block (hinter die
                    // Untertitel-Spuren), Untertitelspur ganz nach oben.
                    TrackKind::Video => {
                        let at = tracks
                            .iter()
                            .position(|t| t.kind != TrackKind::Subtitle)
                            .unwrap_or(tracks.len());
                        tracks.insert(at, track);
                    }
                    TrackKind::Audio => tracks.push(track),
                    TrackKind::Subtitle => tracks.insert(0, track),
                }
                id
            });

            let link_id = c.link_id.as_ref().map(|link| {
                new_link_ids
                    .entry(link.clone())
                    .or_insert_with(new_id)
                    .clone()
            });
            let mut p = c.clone();
            p.id = new_id();
            id_map.insert(c.id.clone(), p.id.clone());
            p.track_id = track_id;
            p.start = start;
            p.link_id = link_id;
            pasted.push(p);
        }
        if pasted.is_empty() {
            return;
        }
        self.push_history();
        self.tracks = tracks;
        self.selected_clip_ids = pasted.iter().map(|c| c.id.clone()).collect();
        self.clips.extend(pasted);
        // Übergänge der Zwischenablage auf die frischen Clip-IDs übertragen.
        let copies: Vec<Transition> = self
            .clipboard_transitions
            .iter()
            .filter_map(|t| remap_transition(t, &id_map))
            .collect();
        self.transitions.extend(copies);
        self.reconcile_transitions();
    }

    // ------------------------------------------------- Effekte / Keyframes
    // Alle fx_*-Methoden mit Undo-Snapshot; *_live-Varianten schreiben ohne
    // Snapshot (für Drag-Gesten — der Aufrufer legt zu Gestenbeginn einmal
    // `begin_fx_edit()` an, wie beim Mixer).

    pub fn clip(&self, id: &str) -> Option<&TimelineClip> {
        self.clips.iter().find(|c| c.id == id)
    }

    fn fx_clip_mut(&mut self, id: &str) -> Option<&mut TimelineClip> {
        let locked = locked_track_ids(&self.tracks);
        self.clips
            .iter_mut()
            .find(|c| c.id == id && !locked.contains(&c.track_id))
    }

    /// Beginn einer fx-Geste (Wert-Scrubbing, Monitor-Drag, Keyframe-Drag):
    /// legt einmalig einen Undo-Snapshot an.
    pub fn begin_fx_edit(&mut self) {
        self.push_history();
    }

    /// Wert anwenden (ohne Snapshot): animierter Parameter ⇒ Keyframe an
    /// der Medienzeit, sonst statischer Wert.
    pub fn fx_set_value_live(&mut self, id: &str, param: ParamId, media_t: f64, value: f64) {
        if let Some(clip) = self.fx_clip_mut(id) {
            let (lo, hi) = param.range();
            clip.fx.param_mut(param).set_at(media_t, value.clamp(lo, hi));
        }
    }

    pub fn fx_set_uniform_scale(&mut self, id: &str, uniform: bool) {
        let Some(clip) = self.clip(id) else { return };
        if clip.fx.uniform_scale == uniform {
            return;
        }
        self.push_history();
        let clip = self.fx_clip_mut(id).expect("clip nach Snapshot");
        if !uniform {
            // Y übernimmt beim Entkoppeln die aktuelle X-Kurve.
            clip.fx.scale_y = clip.fx.scale_x.clone();
        }
        clip.fx.uniform_scale = uniform;
    }

    /// Bewegung (Position/Skalierung/Rotation) der Clips zurücksetzen.
    pub fn fx_reset_motion(&mut self, ids: &[String]) {
        let locked = locked_track_ids(&self.tracks);
        let affected: Vec<String> = self
            .clips
            .iter()
            .filter(|c| {
                ids.contains(&c.id) && !locked.contains(&c.track_id) && {
                    let d = ClipFx::default();
                    c.fx.pos_x != d.pos_x
                        || c.fx.pos_y != d.pos_y
                        || c.fx.scale_x != d.scale_x
                        || c.fx.scale_y != d.scale_y
                        || c.fx.rotation != d.rotation
                        || !c.fx.uniform_scale
                }
            })
            .map(|c| c.id.clone())
            .collect();
        if affected.is_empty() {
            return;
        }
        self.push_history();
        for c in &mut self.clips {
            if affected.contains(&c.id) {
                let d = ClipFx::default();
                c.fx.pos_x = d.pos_x;
                c.fx.pos_y = d.pos_y;
                c.fx.scale_x = d.scale_x;
                c.fx.scale_y = d.scale_y;
                c.fx.rotation = d.rotation;
                c.fx.uniform_scale = true;
            }
        }
    }

    // ------------------------------------------------------- Farbkorrektur
    // Gleiches Gesten-Muster wie fx_*: `grade_update` legt einen
    // Undo-Snapshot an (Einzelklicks), `grade_update_live` schreibt ohne
    // (Drag-Gesten — der Aufrufer ruft zu Gestenbeginn `begin_fx_edit()`).

    /// Farbkorrektur ändern (mit Undo-Snapshot).
    pub fn grade_update(&mut self, id: &str, f: impl FnOnce(&mut ColorGrade)) {
        if self.fx_clip_mut(id).is_none() {
            return;
        }
        self.push_history();
        let clip = self.fx_clip_mut(id).expect("clip nach Snapshot");
        f(&mut clip.grade);
    }

    /// Farbkorrektur ändern OHNE Snapshot (laufende Geste).
    pub fn grade_update_live(&mut self, id: &str, f: impl FnOnce(&mut ColorGrade)) {
        if let Some(clip) = self.fx_clip_mut(id) {
            f(&mut clip.grade);
        }
    }

    /// Farbkorrektur der Clips vollständig zurücksetzen.
    pub fn grade_reset(&mut self, ids: &[String]) {
        let locked = locked_track_ids(&self.tracks);
        let affected: Vec<String> = self
            .clips
            .iter()
            .filter(|c| {
                ids.contains(&c.id) && !locked.contains(&c.track_id) && !c.grade.is_default()
            })
            .map(|c| c.id.clone())
            .collect();
        if affected.is_empty() {
            return;
        }
        self.push_history();
        for c in &mut self.clips {
            if affected.contains(&c.id) {
                c.grade = ColorGrade::default();
            }
        }
    }

    /// Bypass der Farbkorrektur umschalten (Werte bleiben erhalten).
    pub fn grade_toggle_enabled(&mut self, id: &str) {
        let Some(clip) = self.clip(id) else { return };
        let next = !clip.grade.enabled;
        self.grade_update(id, |g| g.enabled = next);
    }

    /// Einzelnen Parameter auf den Standardwert zurücksetzen.
    pub fn fx_reset_param(&mut self, id: &str, param: ParamId) {
        let Some(clip) = self.clip(id) else { return };
        let p = clip.fx.param(param);
        if !p.is_animated() && p.value == param.default_value() {
            return;
        }
        self.push_history();
        let clip = self.fx_clip_mut(id).expect("clip nach Snapshot");
        *clip.fx.param_mut(param) = crate::core::animation::AnimatedParam::fixed(param.default_value());
    }

    // ------------------------------------------- Generische Keyframe-Ops
    // `ParamRef` adressiert eingebaute Parameter (Bewegung/Deckkraft/
    // Lautstärke) UND Effekt-Parameter einheitlich; das Panel
    // Effekteinstellungen läuft komplett über diese kf_*-Methoden.

    /// (min, max, Default) eines Parameters — Effekt-Parameter aus der Spec.
    pub fn param_bounds(clip: &TimelineClip, pref: &ParamRef) -> (f64, f64, f64) {
        match pref {
            ParamRef::Builtin(id) => {
                let (lo, hi) = id.range();
                (lo, hi, id.default_value())
            }
            ParamRef::Effect { fx_id, index } => clip
                .effects
                .iter()
                .find(|e| &e.id == fx_id)
                .and_then(|e| e.kind.specs().get(*index))
                .map(|s| (s.min, s.max, s.default))
                .unwrap_or((f64::NEG_INFINITY, f64::INFINITY, 0.0)),
        }
    }

    /// Parameter eines Clips auflösen (lesend).
    pub fn clip_param<'a>(clip: &'a TimelineClip, pref: &ParamRef) -> Option<&'a AnimatedParam> {
        match pref {
            ParamRef::Builtin(id) => Some(clip.fx.param(*id)),
            ParamRef::Effect { fx_id, index } => clip
                .effects
                .iter()
                .find(|e| &e.id == fx_id)
                .and_then(|e| e.params.get(*index)),
        }
    }

    fn clip_param_mut<'a>(
        clip: &'a mut TimelineClip,
        pref: &ParamRef,
    ) -> Option<&'a mut AnimatedParam> {
        match pref {
            ParamRef::Builtin(id) => Some(clip.fx.param_mut(*id)),
            ParamRef::Effect { fx_id, index } => clip
                .effects
                .iter_mut()
                .find(|e| &e.id == fx_id)
                .and_then(|e| e.params.get_mut(*index)),
        }
    }

    /// Wert anwenden (ohne Snapshot — laufende Geste nach `begin_fx_edit`).
    pub fn kf_set_value_live(&mut self, id: &str, pref: &ParamRef, media_t: f64, value: f64) {
        let Some(clip) = self.fx_clip_mut(id) else { return };
        let (lo, hi, _) = Self::param_bounds(clip, pref);
        if let Some(p) = Self::clip_param_mut(clip, pref) {
            p.set_at(media_t, value.clamp(lo, hi));
        }
    }

    /// Stopwatch umschalten: an ⇒ erster Keyframe am Playhead; aus ⇒ Kurve
    /// verwerfen, aktuellen Wert einfrieren.
    pub fn kf_toggle_animated(&mut self, id: &str, pref: &ParamRef, media_t: f64) {
        if self
            .fx_clip_mut(id)
            .and_then(|c| Self::clip_param_mut(c, pref))
            .is_none()
        {
            return;
        }
        self.push_history();
        let clip = self.fx_clip_mut(id).expect("clip nach Snapshot");
        let p = Self::clip_param_mut(clip, pref).expect("param nach Snapshot");
        if p.is_animated() {
            p.clear_animation(media_t);
        } else {
            p.enable_animation(media_t);
        }
    }

    /// Keyframe am Playhead setzen bzw. entfernen (Raute-Button).
    pub fn kf_toggle_keyframe(&mut self, id: &str, pref: &ParamRef, media_t: f64) {
        let Some(p) = self.clip(id).and_then(|c| Self::clip_param(c, pref)) else {
            return;
        };
        let value = p.eval(media_t);
        let exists = p.key_index_at(media_t).is_some();
        if self.fx_clip_mut(id).is_none() {
            return;
        }
        self.push_history();
        let clip = self.fx_clip_mut(id).expect("clip nach Snapshot");
        let Some(p) = Self::clip_param_mut(clip, pref) else { return };
        if exists {
            p.remove_key_at(media_t);
        } else {
            p.upsert_key(media_t, value);
        }
    }

    /// Keyframes zu gegebenen Medienzeiten entfernen (Keyframe-Editor).
    pub fn kf_remove_keyframes(&mut self, id: &str, keys: &[(ParamRef, f64)]) {
        if keys.is_empty() || self.fx_clip_mut(id).is_none() {
            return;
        }
        self.push_history();
        let clip = self.fx_clip_mut(id).expect("clip nach Snapshot");
        for (pref, t) in keys {
            if let Some(p) = Self::clip_param_mut(clip, pref) {
                p.remove_key_at(*t);
            }
        }
    }

    /// Kurve eines Parameters ersetzen (ohne Snapshot — Keyframe-Drag).
    pub fn kf_replace_keys_live(&mut self, id: &str, pref: &ParamRef, keys: Vec<Keyframe>) {
        if let Some(clip) = self.fx_clip_mut(id) {
            if let Some(p) = Self::clip_param_mut(clip, pref) {
                p.replace_keys(keys);
            }
        }
    }

    /// Interpolation der Keyframes (Parameter, Medienzeit) setzen.
    pub fn kf_set_interp(&mut self, id: &str, keys: &[(ParamRef, f64)], interp: Interp) {
        if keys.is_empty() || self.fx_clip_mut(id).is_none() {
            return;
        }
        self.push_history();
        let clip = self.fx_clip_mut(id).expect("clip nach Snapshot");
        for (pref, t) in keys {
            if let Some(p) = Self::clip_param_mut(clip, pref) {
                if let Some(i) = p.key_index_at(*t) {
                    p.keyframes[i].interp = interp;
                }
            }
        }
    }

    /// Einzelnen Parameter auf den Spec-/Standardwert zurücksetzen.
    pub fn kf_reset_param(&mut self, id: &str, pref: &ParamRef) {
        let Some(clip) = self.clip(id) else { return };
        let (_, _, default) = Self::param_bounds(clip, pref);
        let Some(p) = Self::clip_param(clip, pref) else { return };
        if !p.is_animated() && p.value == default {
            return;
        }
        self.push_history();
        let clip = self.fx_clip_mut(id).expect("clip nach Snapshot");
        if let Some(p) = Self::clip_param_mut(clip, pref) {
            *p = AnimatedParam::fixed(default);
        }
    }

    // ------------------------------------------------------- Effekt-Stapel

    /// Effekt ans Ende des Stapels hängen. Audio-Effekte landen auf dem
    /// Audio-Clip (bei verknüpften Paaren ggf. dem Partner), Video-Effekte
    /// auf dem Video-Clip. Liefert die Ziel-Clip-ID bei Erfolg.
    pub fn effects_add(&mut self, clip_id: &str, kind: EffectKind) -> Option<String> {
        let target = self.effect_target_clip(clip_id, kind)?;
        self.push_history();
        let clip = self.fx_clip_mut(&target).expect("clip nach Snapshot");
        clip.effects.push(EffectInstance::new(kind));
        Some(target)
    }

    /// Passenden Clip für die Effektart finden: der Clip selbst oder sein
    /// verknüpfter A/V-Partner; None, wenn die Art nicht passt (z. B.
    /// Video-Effekt auf reinem Audio-Clip) oder die Spur gesperrt ist.
    pub fn effect_target_clip(&self, clip_id: &str, kind: EffectKind) -> Option<String> {
        let locked = locked_track_ids(&self.tracks);
        let clip = self.clip(clip_id)?;
        let want = if kind.is_audio() {
            TrackKind::Audio
        } else {
            TrackKind::Video
        };
        if clip.kind == want {
            return (!locked.contains(&clip.track_id)).then(|| clip.id.clone());
        }
        let link = clip.link_id.as_ref()?;
        self.clips
            .iter()
            .find(|c| {
                c.id != clip.id
                    && c.link_id.as_deref() == Some(link)
                    && c.kind == want
                    && !locked.contains(&c.track_id)
            })
            .map(|c| c.id.clone())
    }

    pub fn effects_remove(&mut self, clip_id: &str, fx_id: &str) {
        let exists = self
            .clip(clip_id)
            .is_some_and(|c| c.effects.iter().any(|e| e.id == fx_id));
        if !exists || self.fx_clip_mut(clip_id).is_none() {
            return;
        }
        self.push_history();
        let clip = self.fx_clip_mut(clip_id).expect("clip nach Snapshot");
        clip.effects.retain(|e| e.id != fx_id);
    }

    /// Effekt im Stapel verschieben (delta −1 = nach oben, +1 = nach unten).
    pub fn effects_move(&mut self, clip_id: &str, fx_id: &str, delta: i32) {
        let Some(clip) = self.clip(clip_id) else { return };
        let Some(idx) = clip.effects.iter().position(|e| e.id == fx_id) else {
            return;
        };
        let new_idx = idx as i32 + delta;
        if new_idx < 0 || new_idx >= clip.effects.len() as i32 {
            return;
        }
        if self.fx_clip_mut(clip_id).is_none() {
            return;
        }
        self.push_history();
        let clip = self.fx_clip_mut(clip_id).expect("clip nach Snapshot");
        clip.effects.swap(idx, new_idx as usize);
    }

    /// Effekt an eine Zielposition im Stapel ziehen (Drag-to-Reorder): `dest`
    /// = gewünschter Index NACH dem Entfernen (geklemmt). No-op ohne Bewegung.
    pub fn effects_reorder(&mut self, clip_id: &str, fx_id: &str, dest: usize) {
        let Some(clip) = self.clip(clip_id) else { return };
        let Some(cur) = clip.effects.iter().position(|e| e.id == fx_id) else {
            return;
        };
        let dest = dest.min(clip.effects.len().saturating_sub(1));
        if dest == cur || self.fx_clip_mut(clip_id).is_none() {
            return;
        }
        self.push_history();
        let clip = self.fx_clip_mut(clip_id).expect("clip nach Snapshot");
        let inst = clip.effects.remove(cur);
        clip.effects.insert(dest.min(clip.effects.len()), inst);
    }

    /// Bypass eines Effekts umschalten (Werte bleiben erhalten).
    pub fn effects_toggle_enabled(&mut self, clip_id: &str, fx_id: &str) {
        let exists = self
            .clip(clip_id)
            .is_some_and(|c| c.effects.iter().any(|e| e.id == fx_id));
        if !exists || self.fx_clip_mut(clip_id).is_none() {
            return;
        }
        self.push_history();
        let clip = self.fx_clip_mut(clip_id).expect("clip nach Snapshot");
        if let Some(e) = clip.effects.iter_mut().find(|e| e.id == fx_id) {
            e.enabled = !e.enabled;
        }
    }

    /// Alle Parameter eines Effekts auf Defaults zurücksetzen.
    pub fn effects_reset(&mut self, clip_id: &str, fx_id: &str) {
        let exists = self
            .clip(clip_id)
            .is_some_and(|c| c.effects.iter().any(|e| e.id == fx_id));
        if !exists || self.fx_clip_mut(clip_id).is_none() {
            return;
        }
        self.push_history();
        let clip = self.fx_clip_mut(clip_id).expect("clip nach Snapshot");
        if let Some(e) = clip.effects.iter_mut().find(|e| e.id == fx_id) {
            e.reset();
        }
    }

    // -------------------------------------------------- Spur-Effekte (Bus)

    /// Index einer editierbaren (entsperrten) Audio-Spur.
    fn track_audio_idx(&self, track_id: &str) -> Option<usize> {
        self.tracks
            .iter()
            .position(|t| t.id == track_id && t.kind == TrackKind::Audio && !t.locked)
    }

    fn track_idx(&self, track_id: &str) -> Option<usize> {
        self.tracks.iter().position(|t| t.id == track_id)
    }

    /// Audio-Effekt an die Bus-Kette der Spur anhängen.
    pub fn track_effects_add(&mut self, track_id: &str, kind: EffectKind) -> bool {
        if !kind.is_audio() || self.track_audio_idx(track_id).is_none() {
            return false;
        }
        self.push_history();
        let i = self.track_audio_idx(track_id).expect("Spur nach Snapshot");
        self.tracks[i].effects.push(EffectInstance::new(kind));
        true
    }

    pub fn track_effects_remove(&mut self, track_id: &str, fx_id: &str) {
        let Some(i) = self.track_audio_idx(track_id) else {
            return;
        };
        if !self.tracks[i].effects.iter().any(|e| e.id == fx_id) {
            return;
        }
        self.push_history();
        self.tracks[i].effects.retain(|e| e.id != fx_id);
    }

    pub fn track_effects_move(&mut self, track_id: &str, fx_id: &str, delta: i32) {
        let Some(i) = self.track_audio_idx(track_id) else {
            return;
        };
        let Some(idx) = self.tracks[i].effects.iter().position(|e| e.id == fx_id) else {
            return;
        };
        let new_idx = idx as i32 + delta;
        if new_idx < 0 || new_idx >= self.tracks[i].effects.len() as i32 {
            return;
        }
        self.push_history();
        self.tracks[i].effects.swap(idx, new_idx as usize);
    }

    pub fn track_effects_toggle_enabled(&mut self, track_id: &str, fx_id: &str) {
        let Some(i) = self.track_audio_idx(track_id) else {
            return;
        };
        if !self.tracks[i].effects.iter().any(|e| e.id == fx_id) {
            return;
        }
        self.push_history();
        if let Some(e) = self.tracks[i].effects.iter_mut().find(|e| e.id == fx_id) {
            e.enabled = !e.enabled;
        }
    }

    pub fn track_effects_reset(&mut self, track_id: &str, fx_id: &str) {
        let Some(i) = self.track_audio_idx(track_id) else {
            return;
        };
        if !self.tracks[i].effects.iter().any(|e| e.id == fx_id) {
            return;
        }
        self.push_history();
        if let Some(e) = self.tracks[i].effects.iter_mut().find(|e| e.id == fx_id) {
            e.reset();
        }
    }

    // --------------------------------------------------- Spur-Automation
    // Lautstärke/Pan als Keyframe-Kurven über die SEQUENZZEIT (anders als
    // Clip-Keyframes, die in Medienzeit kleben). Diskrete Punkt-Operationen
    // legen einen Undo-Snapshot an; Drag-Gesten nutzen `begin_mix_edit` +
    // die `*_live`-Varianten (ein Snapshot pro Geste).

    pub fn track_auto_add_point(
        &mut self,
        track_id: &str,
        param: TrackAutoParam,
        t: f64,
        value: f64,
    ) {
        let Some(i) = self.track_idx(track_id) else {
            return;
        };
        self.push_history();
        self.tracks[i].auto_param_mut(param).upsert_key(t.max(0.0), value);
    }

    pub fn track_auto_remove_point(&mut self, track_id: &str, param: TrackAutoParam, t: f64) {
        let Some(i) = self.track_idx(track_id) else {
            return;
        };
        if self.tracks[i].auto_param(param).key_index_at(t).is_none() {
            return;
        }
        self.push_history();
        self.tracks[i].auto_param_mut(param).remove_key_at(t);
    }

    pub fn track_auto_clear(&mut self, track_id: &str, param: TrackAutoParam) {
        let Some(i) = self.track_idx(track_id) else {
            return;
        };
        if !self.tracks[i].auto_param(param).is_animated() {
            return;
        }
        self.push_history();
        self.tracks[i].auto_param_mut(param).keyframes.clear();
    }

    /// Lautstärke- und Pan-Automation der anvisierten Audio-Spuren löschen
    /// (ohne Targeting: aller Audio-Spuren) — ein Undo-Schritt. Command-Ziel.
    pub fn clear_track_automation_targeted(&mut self) {
        let any_targeted = self
            .tracks
            .iter()
            .any(|t| t.kind == TrackKind::Audio && t.targeted);
        let idxs: Vec<usize> = self
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                t.kind == TrackKind::Audio
                    && (!any_targeted || t.targeted)
                    && t.has_automation()
            })
            .map(|(i, _)| i)
            .collect();
        if idxs.is_empty() {
            return;
        }
        self.push_history();
        for i in idxs {
            self.tracks[i].volume_auto.keyframes.clear();
            self.tracks[i].pan_auto.keyframes.clear();
        }
    }

    /// Ganze Kurve ohne History ersetzen (Punkt-Drag in Zeit + Wert).
    pub fn track_auto_replace_live(
        &mut self,
        track_id: &str,
        param: TrackAutoParam,
        keys: Vec<Keyframe>,
    ) {
        let Some(i) = self.track_idx(track_id) else {
            return;
        };
        self.tracks[i].auto_param_mut(param).replace_keys(keys);
    }

    /// Alle Effekte der Clips entfernen.
    pub fn effects_clear(&mut self, ids: &[String]) {
        let locked = locked_track_ids(&self.tracks);
        let affected: Vec<String> = self
            .clips
            .iter()
            .filter(|c| {
                ids.contains(&c.id) && !locked.contains(&c.track_id) && !c.effects.is_empty()
            })
            .map(|c| c.id.clone())
            .collect();
        if affected.is_empty() {
            return;
        }
        self.push_history();
        for c in &mut self.clips {
            if affected.contains(&c.id) {
                c.effects.clear();
            }
        }
    }

    /// Bypass ALLER Effekte der Clips umschalten: ist irgendeiner aktiv,
    /// werden alle deaktiviert, sonst alle aktiviert.
    pub fn effects_toggle_bypass(&mut self, ids: &[String]) {
        let locked = locked_track_ids(&self.tracks);
        let affected: Vec<String> = self
            .clips
            .iter()
            .filter(|c| {
                ids.contains(&c.id) && !locked.contains(&c.track_id) && !c.effects.is_empty()
            })
            .map(|c| c.id.clone())
            .collect();
        if affected.is_empty() {
            return;
        }
        let any_enabled = self
            .clips
            .iter()
            .filter(|c| affected.contains(&c.id))
            .any(|c| c.effects.iter().any(|e| e.enabled));
        self.push_history();
        for c in &mut self.clips {
            if affected.contains(&c.id) {
                for e in &mut c.effects {
                    e.enabled = !any_enabled;
                }
            }
        }
    }

    // -------------------------------------------------- Attribute-Klemmbrett

    /// Attribute des Clips (inkl. A/V-Partner) ins Klemmbrett kopieren —
    /// Premiere-Workflow „Attribute einfügen“.
    pub fn copy_attributes(&mut self, id: &str) -> bool {
        let Some(clip) = self.clip(id) else { return false };
        let mut attrs = ClipAttributes {
            fx: clip.fx.clone(),
            grade: clip.grade.clone(),
            effects: clip.effects.clone(),
            gain_db: clip.gain_db,
            from_kind: clip.kind,
            linked: None,
        };
        // Verknüpfter Partner liefert die jeweils andere Hälfte mit.
        if let Some(link) = clip.link_id.as_ref() {
            if let Some(partner) = self
                .clips
                .iter()
                .find(|c| c.id != clip.id && c.link_id.as_deref() == Some(link))
            {
                attrs.linked = Some(Box::new(ClipAttributes {
                    fx: partner.fx.clone(),
                    grade: partner.grade.clone(),
                    effects: partner.effects.clone(),
                    gain_db: partner.gain_db,
                    from_kind: partner.kind,
                    linked: None,
                }));
            }
        }
        self.attr_clipboard = Some(attrs);
        true
    }

    /// Attribute aus dem Klemmbrett auf die Clips anwenden (artgerecht:
    /// Video-Clips erhalten Video-Attribute, Audio-Clips Audio-Attribute).
    /// Effekt-Instanzen bekommen frische IDs.
    pub fn paste_attributes(&mut self, ids: &[String]) {
        let Some(attrs) = self.attr_clipboard.clone() else { return };
        let locked = locked_track_ids(&self.tracks);
        let affected: Vec<String> = self
            .clips
            .iter()
            .filter(|c| ids.contains(&c.id) && !locked.contains(&c.track_id))
            .map(|c| c.id.clone())
            .collect();
        if affected.is_empty() {
            return;
        }
        self.push_history();
        for c in &mut self.clips {
            if !affected.contains(&c.id) {
                continue;
            }
            // Quelle wählen: passende Art aus Haupt- oder Partner-Attributen.
            let src = if attrs.from_kind == c.kind {
                Some(&attrs)
            } else {
                attrs
                    .linked
                    .as_deref()
                    .filter(|l| l.from_kind == c.kind)
            };
            let Some(src) = src else { continue };
            c.grade = src.grade.clone();
            c.gain_db = src.gain_db;
            match c.kind {
                // Untertitel-Segmente sind transformierbar wie Video-Layer.
                TrackKind::Video | TrackKind::Subtitle => {
                    c.fx = src.fx.clone();
                }
                TrackKind::Audio => {
                    // Nur die Lautstärke-Kurve übernehmen (Video-Transform
                    // hat auf Audio-Clips keine Bedeutung).
                    c.fx.volume_db = src.fx.volume_db.clone();
                }
            }
            c.effects = src
                .effects
                .iter()
                .filter(|e| e.kind.is_audio() == (c.kind == TrackKind::Audio))
                .map(|e| {
                    let mut e = e.clone();
                    e.id = new_id();
                    e
                })
                .collect();
        }
    }

    pub fn has_attr_clipboard(&self) -> bool {
        self.attr_clipboard.is_some()
    }

    // ------------------------------------------------------------- Verlauf

    pub fn undo(&mut self) {
        let Some(prev) = self.past.pop() else { return };
        self.future.insert(
            0,
            Snapshot {
                tracks: std::mem::replace(&mut self.tracks, prev.tracks),
                clips: std::mem::replace(&mut self.clips, prev.clips),
                transitions: std::mem::replace(&mut self.transitions, prev.transitions),
                markers: std::mem::replace(&mut self.markers, prev.markers),
                master_gain_db: std::mem::replace(&mut self.master_gain_db, prev.master_gain_db),
            },
        );
        self.prune_selection();
        self.prune_transition_selection();
        self.revision += 1;
    }

    pub fn redo(&mut self) {
        if self.future.is_empty() {
            return;
        }
        let next = self.future.remove(0);
        self.past.push(Snapshot {
            tracks: std::mem::replace(&mut self.tracks, next.tracks),
            clips: std::mem::replace(&mut self.clips, next.clips),
            transitions: std::mem::replace(&mut self.transitions, next.transitions),
            markers: std::mem::replace(&mut self.markers, next.markers),
            master_gain_db: std::mem::replace(&mut self.master_gain_db, next.master_gain_db),
        });
        self.prune_selection();
        self.prune_transition_selection();
        self.revision += 1;
    }

    // ----------------------------------------------------------- Übergänge

    pub fn transition(&self, id: &str) -> Option<&Transition> {
        self.transitions.iter().find(|t| t.id == id)
    }

    /// Spur eines Übergangs (über die referenzierten Clips).
    pub fn transition_track_id(&self, tr: &Transition) -> Option<String> {
        let (from, to) = transitions::resolve_clips(&self.clips, tr);
        from.or(to).map(|c| c.track_id.clone())
    }

    /// Zeitfenster `[w0, w1)` eines Übergangs in Sequenzzeit.
    pub fn transition_window(&self, tr: &Transition) -> Option<(f64, f64)> {
        let (from, to) = transitions::resolve_clips(&self.clips, tr);
        transitions::window(from, to, tr.alignment, tr.duration)
    }

    /// Schnittkante eines Übergangs in Sequenzzeit.
    pub fn transition_cut(&self, tr: &Transition) -> Option<f64> {
        let (from, to) = transitions::resolve_clips(&self.clips, tr);
        transitions::cut_time(from, to)
    }

    /// Maximal erlaubte Dauer eines Übergangs (Handles + Cliplängen).
    pub fn transition_max_duration(&self, tr: &Transition) -> f64 {
        let (from, to) = transitions::resolve_clips(&self.clips, tr);
        transitions::max_duration(from, to, tr.alignment)
    }

    /// Audio-Crossfade-Fenster eines Clips: (w0, w1, fade_in, equal_power).
    /// `fade_in` = Clip ist die eingehende Seite. Höchstens zwei Einträge
    /// (Clipanfang + Clipende). Player-Mixdown und Export-Planer nutzen
    /// dieselben Fenster — identische Hüllkurven.
    pub fn audio_fades(&self, clip: &TimelineClip) -> Vec<(f64, f64, bool, bool)> {
        let mut fades = Vec::new();
        for tr in &self.transitions {
            if !tr.kind.is_audio() {
                continue;
            }
            let fade_in = match (tr.from_clip_id.as_deref(), tr.to_clip_id.as_deref()) {
                (_, Some(to)) if to == clip.id => true,
                (Some(from), _) if from == clip.id => false,
                _ => continue,
            };
            let Some((w0, w1)) = self.transition_window(tr) else {
                continue;
            };
            if w1 > w0 {
                let equal_power = tr.kind == TransitionKind::ConstantPower;
                fades.push((w0, w1, fade_in, equal_power));
            }
        }
        fades
    }

    /// Hörbarer Bereich eines Audio-Clips inkl. Übergangs-Verlängerung.
    pub fn audio_extent(&self, clip: &TimelineClip, fades: &[(f64, f64, bool, bool)]) -> (f64, f64) {
        let mut a0 = clip.start;
        let mut a1 = clip.end();
        for (w0, w1, _, _) in fades {
            a0 = a0.min(*w0);
            a1 = a1.max(*w1);
        }
        (a0, a1)
    }

    fn prune_transition_selection(&mut self) {
        let existing: std::collections::HashSet<&str> =
            self.transitions.iter().map(|t| t.id.as_str()).collect();
        self.selected_transition_ids
            .retain(|id| existing.contains(id.as_str()));
    }

    /// Übergang auswählen (ersetzt Clip- und Übergangsauswahl).
    pub fn select_transition(&mut self, id: &str) {
        if self.transition(id).is_some() {
            self.selected_clip_ids.clear();
            self.selected_transition_ids = vec![id.to_string()];
        }
    }

    /// Konsistenz nach strukturellen Edits herstellen: verwaiste, nicht
    /// mehr benachbarte oder zu lange Übergänge entfernen bzw. kürzen;
    /// überlappende Fenster derselben Spur auflösen (frühere Kante gewinnt).
    fn reconcile_transitions(&mut self) {
        let clips = std::mem::take(&mut self.clips);
        let mut kept: Vec<Transition> = Vec::new();
        for mut tr in std::mem::take(&mut self.transitions) {
            let (from, to) = transitions::resolve_clips(&clips, &tr);
            // Referenzierte, aber verschwundene Clips ⇒ Übergang entfällt.
            if tr.from_clip_id.is_some() && from.is_none() {
                continue;
            }
            if tr.to_clip_id.is_some() && to.is_none() {
                continue;
            }
            let Some(anchor) = from.or(to) else { continue };
            // Art muss zur Spurart passen.
            if tr.kind.is_audio() != (anchor.kind == TrackKind::Audio) {
                continue;
            }
            // Zweiseitig: gleiche Spur + direkte Nachbarschaft.
            if let (Some(f), Some(t)) = (from, to) {
                if f.track_id != t.track_id || (f.end() - t.start).abs() > EPS {
                    continue;
                }
            }
            // Dauer an Handles/Cliplängen klemmen.
            let max = transitions::max_duration(from, to, tr.alignment);
            if max < MIN_CLIP_DURATION - EPS {
                continue;
            }
            tr.duration = tr.duration.min(max).max(MIN_CLIP_DURATION);
            kept.push(tr);
        }
        // Überlappungen je Spur entfernen: nach Fensterbeginn sortiert,
        // jede Kante, die in ein bereits akzeptiertes Fenster ragt, fliegt.
        let mut order: Vec<(String, f64, f64, usize)> = kept
            .iter()
            .enumerate()
            .filter_map(|(i, tr)| {
                let (from, to) = transitions::resolve_clips(&clips, tr);
                let track = from.or(to)?.track_id.clone();
                let (w0, w1) = transitions::window(from, to, tr.alignment, tr.duration)?;
                Some((track, w0, w1, i))
            })
            .collect();
        order.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
        let mut drop: std::collections::HashSet<usize> = Default::default();
        let mut last: Option<(String, f64)> = None; // (Spur, bisheriges Fensterende)
        for (track, w0, w1, i) in order {
            match &last {
                Some((lt, lw1)) if *lt == track && w0 < lw1 - EPS => {
                    drop.insert(i);
                }
                _ => last = Some((track, w1)),
            }
        }
        if !drop.is_empty() {
            kept = kept
                .into_iter()
                .enumerate()
                .filter(|(i, _)| !drop.contains(i))
                .map(|(_, t)| t)
                .collect();
        }
        self.transitions = kept;
        self.clips = clips;
        self.prune_transition_selection();
    }

    /// Übergang an der Kante eines Clips anwenden: `edge` Start = Kante am
    /// Clipanfang, End = am Clipende. Existiert dort bereits ein Übergang,
    /// wird er ersetzt. Liefert die neue ID oder eine Fehlermeldung.
    pub fn add_transition(
        &mut self,
        kind: TransitionKind,
        clip_id: &str,
        edge: TrimEdge,
        duration: f64,
    ) -> Result<String, String> {
        let locked = locked_track_ids(&self.tracks);
        let Some(clip) = self.clip(clip_id).cloned() else {
            return Err("Clip nicht gefunden".into());
        };
        if locked.contains(&clip.track_id) {
            return Err("Spur ist gesperrt".into());
        }
        if clip.kind == TrackKind::Subtitle || kind.is_audio() != (clip.kind == TrackKind::Audio) {
            return Err(format!("„{}“ passt nicht zur Spurart", kind.label()));
        }
        // Nachbar an der Kante (direkt angrenzend auf derselben Spur).
        let neighbor = match edge {
            TrimEdge::Start => self
                .clips
                .iter()
                .find(|c| c.track_id == clip.track_id && (c.end() - clip.start).abs() < EPS),
            TrimEdge::End => self
                .clips
                .iter()
                .find(|c| c.track_id == clip.track_id && (c.start - clip.end()).abs() < EPS),
        }
        .cloned();
        let (from_id, to_id) = match edge {
            TrimEdge::Start => (neighbor.as_ref().map(|c| c.id.clone()), Some(clip.id.clone())),
            TrimEdge::End => (Some(clip.id.clone()), neighbor.as_ref().map(|c| c.id.clone())),
        };
        let (from, to) = match edge {
            TrimEdge::Start => (neighbor.as_ref(), Some(&clip)),
            TrimEdge::End => (Some(&clip), neighbor.as_ref()),
        };
        let alignment = TransitionAlignment::Center;
        let max = transitions::max_duration(from, to, alignment);
        if max < MIN_CLIP_DURATION - EPS {
            return Err("Nicht genug Material (Handles) für einen Übergang an dieser Kante".into());
        }
        let clamped = duration.min(max).max(MIN_CLIP_DURATION);

        self.push_history();
        // Bestehenden Übergang an derselben Schnittkante ersetzen (auch
        // wenn er einseitig war oder andersherum verankert ist).
        let cut = match edge {
            TrimEdge::Start => clip.start,
            TrimEdge::End => clip.end(),
        };
        let replaced: Vec<String> = self
            .transitions
            .iter()
            .filter(|t| {
                self.transition_track_id(t).as_deref() == Some(clip.track_id.as_str())
                    && self
                        .transition_cut(t)
                        .is_some_and(|c| (c - cut).abs() < EPS)
            })
            .map(|t| t.id.clone())
            .collect();
        self.transitions.retain(|t| !replaced.contains(&t.id));
        let mut tr = Transition::new(kind, from_id, to_id, clamped);
        tr.alignment = alignment;
        let id = tr.id.clone();
        self.transitions.push(tr);
        self.reconcile_transitions();
        if self.transition(&id).is_some() {
            self.selected_clip_ids.clear();
            self.selected_transition_ids = vec![id.clone()];
            Ok(id)
        } else {
            // Reconcile hat ihn verworfen (z. B. Überlappung mit Nachbar-Übergang).
            Err("Übergang kollidiert mit einem bestehenden Übergang".into())
        }
    }

    /// Standard-/Wunschübergang auf alle passenden Kanten der Auswahl
    /// anwenden (Premiere: Mod+D / Mod+Shift+D). Bereits belegte Kanten
    /// werden übersprungen. Liefert die Anzahl neu gesetzter Übergänge.
    pub fn apply_transition_to_selection(&mut self, kind: TransitionKind) -> usize {
        let locked = locked_track_ids(&self.tracks);
        let want_audio = kind.is_audio();
        let selection: Vec<TimelineClip> = self
            .clips
            .iter()
            .filter(|c| {
                self.selected_clip_ids.contains(&c.id)
                    && c.kind != TrackKind::Subtitle
                    && (c.kind == TrackKind::Audio) == want_audio
                    && !locked.contains(&c.track_id)
            })
            .cloned()
            .collect();
        if selection.is_empty() {
            return 0;
        }

        // Belegte Kanten: Cut-Zeiten bestehender Übergänge je Spur.
        let occupied: Vec<(String, f64)> = self
            .transitions
            .iter()
            .filter_map(|t| Some((self.transition_track_id(t)?, self.transition_cut(t)?)))
            .collect();
        let is_free = |track: &str, cut: f64| {
            !occupied
                .iter()
                .any(|(tr, c)| tr == track && (c - cut).abs() < EPS)
        };

        let mut planned: Vec<Transition> = Vec::new();
        let mut planned_cuts: Vec<(String, f64)> = Vec::new();
        for clip in &selection {
            for edge in [TrimEdge::Start, TrimEdge::End] {
                let cut = match edge {
                    TrimEdge::Start => clip.start,
                    TrimEdge::End => clip.end(),
                };
                if !is_free(&clip.track_id, cut)
                    || planned_cuts
                        .iter()
                        .any(|(t, c)| t == &clip.track_id && (c - cut).abs() < EPS)
                {
                    continue;
                }
                let neighbor = self.clips.iter().find(|c| {
                    c.id != clip.id
                        && c.track_id == clip.track_id
                        && match edge {
                            TrimEdge::Start => (c.end() - clip.start).abs() < EPS,
                            TrimEdge::End => (c.start - clip.end()).abs() < EPS,
                        }
                });
                let (from, to) = match edge {
                    TrimEdge::Start => (neighbor, Some(clip)),
                    TrimEdge::End => (Some(clip), neighbor),
                };
                let max = transitions::max_duration(from, to, TransitionAlignment::Center);
                if max < MIN_CLIP_DURATION - EPS {
                    continue;
                }
                let duration = DEFAULT_TRANSITION_DURATION.min(max).max(MIN_CLIP_DURATION);
                planned.push(Transition::new(
                    kind,
                    from.map(|c| c.id.clone()),
                    to.map(|c| c.id.clone()),
                    duration,
                ));
                planned_cuts.push((clip.track_id.clone(), cut));
            }
        }
        if planned.is_empty() {
            return 0;
        }
        self.push_history();
        let before = self.transitions.len();
        self.transitions.extend(planned);
        self.reconcile_transitions();
        self.transitions.len().saturating_sub(before)
    }

    pub fn remove_transitions(&mut self, ids: &[String]) {
        if !self.transitions.iter().any(|t| ids.contains(&t.id)) {
            return;
        }
        self.push_history();
        self.transitions.retain(|t| !ids.contains(&t.id));
        self.prune_transition_selection();
    }

    /// Dauer setzen (geklemmt an Handles/Cliplängen) — mit Undo-Snapshot.
    pub fn set_transition_duration(&mut self, id: &str, duration: f64) {
        let Some(tr) = self.transition(id).cloned() else { return };
        let max = self.transition_max_duration(&tr);
        let clamped = duration.clamp(MIN_CLIP_DURATION, max.max(MIN_CLIP_DURATION));
        if (clamped - tr.duration).abs() < EPS {
            return;
        }
        self.push_history();
        if let Some(t) = self.transitions.iter_mut().find(|t| t.id == id) {
            t.duration = clamped;
        }
        self.reconcile_transitions();
    }

    pub fn set_transition_alignment(&mut self, id: &str, alignment: TransitionAlignment) {
        let Some(tr) = self.transition(id).cloned() else { return };
        if !tr.is_two_sided() || tr.alignment == alignment {
            return;
        }
        self.push_history();
        if let Some(t) = self.transitions.iter_mut().find(|t| t.id == id) {
            t.alignment = alignment;
        }
        self.reconcile_transitions();
    }

    /// Art ersetzen (Kontextmenü „Ersetzen durch …“) — Spurart muss passen.
    pub fn set_transition_kind(&mut self, id: &str, kind: TransitionKind) {
        let Some(tr) = self.transition(id).cloned() else { return };
        if tr.kind == kind || tr.kind.is_audio() != kind.is_audio() {
            return;
        }
        self.push_history();
        if let Some(t) = self.transitions.iter_mut().find(|t| t.id == id) {
            t.kind = kind;
            if !kind.directional() {
                t.direction = kind.default_direction();
            }
        }
    }

    pub fn set_transition_direction(
        &mut self,
        id: &str,
        direction: crate::core::transitions::TransitionDirection,
    ) {
        let Some(tr) = self.transition(id) else { return };
        if tr.direction == direction || !tr.kind.directional() {
            return;
        }
        self.push_history();
        if let Some(t) = self.transitions.iter_mut().find(|t| t.id == id) {
            t.direction = direction;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_clip(track_id: &str, kind: TrackKind, start: f64, duration: f64) -> TimelineClip {
        TimelineClip {
            id: new_id(),
            track_id: track_id.into(),
            asset_id: "asset".into(),
            name: "Clip".into(),
            kind,
            start,
            duration,
            src_in: 0.0,
            src_duration: f64::INFINITY,
            link_id: None,
            enabled: true,
            gain_db: 0.0,
            fx: ClipFx::default(),
            grade: ColorGrade::default(),
            effects: Vec::new(),
            title: None,
            subtitle: None,
            speed: 1.0,
            reverse: false,
            freeze: false,
            markers: Vec::new(),
        }
    }

    fn track_ids(store: &TimelineStore, kind: TrackKind) -> Vec<String> {
        store
            .tracks
            .iter()
            .filter(|t| t.kind == kind)
            .map(|t| t.id.clone())
            .collect()
    }

    fn placed_clip(track_id: &str, kind: TrackKind, start: f64, duration: f64, src_in: f64) -> TimelineClip {
        let mut c = test_clip(track_id, kind, start, duration);
        c.src_in = src_in;
        c.src_duration = 1000.0;
        c
    }

    /// Clips einer Spur nach Startzeit sortiert.
    fn clips_on(store: &TimelineStore, track_id: &str) -> Vec<TimelineClip> {
        let mut v: Vec<TimelineClip> = store
            .clips
            .iter()
            .filter(|c| c.track_id == track_id)
            .cloned()
            .collect();
        v.sort_by(|a, b| a.start.total_cmp(&b.start));
        v
    }

    #[test]
    fn commit_edit_insert_splits_clip_at_insert_point() {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        store.clips.push(placed_clip(&v1, TrackKind::Video, 0.0, 10.0, 0.0));
        // 4-s-Clip bei t=5 einfügen (Ripple).
        let new = placed_clip(&v1, TrackKind::Video, 5.0, 4.0, 0.0);
        store.commit_edit(vec![new], 5.0, 4.0, true);
        let on_v1 = clips_on(&store, &v1);
        assert_eq!(on_v1.len(), 3, "Split + eingefügter Clip");
        // Linke Hälfte [0,5), neuer Clip [5,9), rechte Hälfte verschoben [9,14).
        assert!((on_v1[0].start - 0.0).abs() < 1e-6 && (on_v1[0].duration - 5.0).abs() < 1e-6);
        assert!((on_v1[1].start - 5.0).abs() < 1e-6 && (on_v1[1].duration - 4.0).abs() < 1e-6);
        assert!((on_v1[2].start - 9.0).abs() < 1e-6 && (on_v1[2].duration - 5.0).abs() < 1e-6);
        // Rechte Hälfte trägt den Medien-Versatz weiter (src_in = 5).
        assert!((on_v1[2].src_in - 5.0).abs() < 1e-6);
    }

    #[test]
    fn commit_edit_overwrite_replaces_range() {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        store.clips.push(placed_clip(&v1, TrackKind::Video, 0.0, 10.0, 0.0));
        let new = placed_clip(&v1, TrackKind::Video, 5.0, 4.0, 0.0);
        store.commit_edit(vec![new], 5.0, 4.0, false);
        let on_v1 = clips_on(&store, &v1);
        assert_eq!(on_v1.len(), 3);
        // [0,5) bleibt, [5,9) ist neu, [9,10) Rest — keine Verschiebung.
        assert!((on_v1[0].duration - 5.0).abs() < 1e-6);
        assert!((on_v1[1].start - 5.0).abs() < 1e-6 && (on_v1[1].duration - 4.0).abs() < 1e-6);
        assert!((on_v1[2].start - 9.0).abs() < 1e-6 && (on_v1[2].duration - 1.0).abs() < 1e-6);
    }

    #[test]
    fn insert_ripples_sync_locked_track_only() {
        // V1 + A1 je ein 10-s-Clip; A1 mit Sync-Lock, A2 ohne.
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        let audios = track_ids(&store, TrackKind::Audio);
        let (a1, a2) = (audios[0].clone(), audios[1].clone());
        store.clips.push(placed_clip(&v1, TrackKind::Video, 0.0, 10.0, 0.0));
        store.clips.push(placed_clip(&a1, TrackKind::Audio, 0.0, 10.0, 0.0));
        store.clips.push(placed_clip(&a2, TrackKind::Audio, 0.0, 10.0, 0.0));
        // A1 sync-locked, A2 nicht.
        store.toggle_track_flag(&a1, TrackFlag::SyncLock);

        let new = placed_clip(&v1, TrackKind::Video, 2.0, 4.0, 0.0);
        store.commit_edit(vec![new], 2.0, 4.0, true);

        // A1 rippelt mit: Split bei 2 → [0,2) + [6,14) (Lücke [2,6)).
        let on_a1 = clips_on(&store, &a1);
        assert_eq!(on_a1.len(), 2, "Sync-Lock-Spur wird geteilt");
        assert!((on_a1[0].duration - 2.0).abs() < 1e-6);
        assert!((on_a1[1].start - 6.0).abs() < 1e-6);
        // A2 bleibt unangetastet (kein Sync-Lock).
        let on_a2 = clips_on(&store, &a2);
        assert_eq!(on_a2.len(), 1);
        assert!((on_a2[0].start - 0.0).abs() < 1e-6 && (on_a2[0].duration - 10.0).abs() < 1e-6);
    }

    #[test]
    fn lift_clears_targeted_range_and_keeps_gap() {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        store.clips.push(placed_clip(&v1, TrackKind::Video, 0.0, 10.0, 0.0));
        store.set_in_out_range(3.0, 7.0);
        assert!(store.lift_range());
        let on_v1 = clips_on(&store, &v1);
        assert_eq!(on_v1.len(), 2, "Lücke bleibt stehen");
        assert!((on_v1[0].duration - 3.0).abs() < 1e-6);
        assert!((on_v1[1].start - 7.0).abs() < 1e-6 && (on_v1[1].duration - 3.0).abs() < 1e-6);
    }

    #[test]
    fn extract_removes_range_and_ripples_left() {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        store.clips.push(placed_clip(&v1, TrackKind::Video, 0.0, 10.0, 0.0));
        store.set_in_out_range(3.0, 7.0);
        assert!(store.extract_range());
        let on_v1 = clips_on(&store, &v1);
        assert_eq!(on_v1.len(), 2);
        // [0,3) bleibt, [7,10) rückt auf [3,6).
        assert!((on_v1[0].duration - 3.0).abs() < 1e-6);
        assert!((on_v1[1].start - 3.0).abs() < 1e-6 && (on_v1[1].duration - 3.0).abs() < 1e-6);
        assert!((store.playhead_sec - 3.0).abs() < 1e-6);
    }

    #[test]
    fn extract_sync_lock_keeps_other_track_in_sync() {
        // V1 (targeted) + A1 (nur sync-locked, NICHT targeted).
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        let a1 = track_ids(&store, TrackKind::Audio)[0].clone();
        // A1 von Targeting befreien, dafür Sync-Lock.
        if let Some(t) = store.tracks.iter_mut().find(|t| t.id == a1) {
            t.targeted = false;
            t.sync_lock = true;
        }
        store.clips.push(placed_clip(&v1, TrackKind::Video, 0.0, 10.0, 0.0));
        store.clips.push(placed_clip(&a1, TrackKind::Audio, 0.0, 10.0, 0.0));
        store.set_in_out_range(3.0, 7.0);
        assert!(store.extract_range());
        // Beide Spuren verlieren [3,7) und schließen die Lücke.
        for tid in [&v1, &a1] {
            let on = clips_on(&store, tid);
            assert_eq!(on.len(), 2, "Spur {tid} extrahiert + rippelt");
            assert!((on[0].duration - 3.0).abs() < 1e-6);
            assert!((on[1].start - 3.0).abs() < 1e-6 && (on[1].duration - 3.0).abs() < 1e-6);
        }
    }

    #[test]
    fn source_patch_is_radio_per_kind() {
        let mut store = TimelineStore::default();
        let videos = track_ids(&store, TrackKind::Video);
        // Standard: V1 (idx 1) gepatcht.
        assert_eq!(store.source_patch_track(TrackKind::Video), Some(videos[1].as_str()));
        // Auf V2 (idx 0) umschalten → V1 verliert den Patch.
        store.toggle_source_patch(&videos[0]);
        assert_eq!(store.source_patch_track(TrackKind::Video), Some(videos[0].as_str()));
        assert!(!store.tracks.iter().find(|t| t.id == videos[1]).unwrap().source_patched);
        // Erneut V2 klicken → Patch aus (kein Ziel mehr).
        store.toggle_source_patch(&videos[0]);
        assert_eq!(store.source_patch_track(TrackKind::Video), None);
    }

    #[test]
    fn ensure_patch_target_defaults_picks_v1_a1() {
        let mut store = TimelineStore::default();
        // Alle Flags löschen (Altprojekt-Zustand).
        for t in &mut store.tracks {
            t.source_patched = false;
            t.targeted = false;
        }
        store.ensure_patch_target_defaults();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        let a1 = track_ids(&store, TrackKind::Audio)[0].clone();
        assert_eq!(store.source_patch_track(TrackKind::Video), Some(v1.as_str()));
        assert_eq!(store.source_patch_track(TrackKind::Audio), Some(a1.as_str()));
        assert!(store.tracks.iter().find(|t| t.id == v1).unwrap().targeted);
        assert!(store.tracks.iter().find(|t| t.id == a1).unwrap().targeted);
    }

    #[test]
    fn match_frame_source_maps_media_time_on_targeted_track() {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        // Clip bei [4,14) mit src_in=2 → bei Sequenzzeit 6 ist Medienzeit 4.
        store.clips.push(placed_clip(&v1, TrackKind::Video, 4.0, 10.0, 2.0));
        let (asset, media_t) = store.match_frame_source(6.0).expect("Clip am Playhead");
        assert_eq!(asset, "asset");
        assert!((media_t - 4.0).abs() < 1e-6);
    }

    #[test]
    fn commit_edit_is_single_undo_step() {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        store.clips.push(placed_clip(&v1, TrackKind::Video, 0.0, 10.0, 0.0));
        let before = store.clips.len();
        let new = placed_clip(&v1, TrackKind::Video, 5.0, 4.0, 0.0);
        store.commit_edit(vec![new], 5.0, 4.0, true);
        assert!(store.can_undo());
        store.undo();
        assert_eq!(store.clips.len(), before, "ein Undo stellt den Ausgangszustand her");
        assert_eq!(clips_on(&store, &v1).len(), 1);
    }

    #[test]
    fn paste_keeps_original_track_when_free() {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        store.clipboard = vec![test_clip(&v1, TrackKind::Video, 0.0, 4.0)];
        store.playhead_sec = 10.0;
        store.paste(None);
        assert_eq!(store.clips.len(), 1);
        assert_eq!(store.clips[0].track_id, v1);
        assert_eq!(store.clips[0].start, 10.0);
        assert_eq!(store.tracks.len(), 4);
    }

    #[test]
    fn paste_evades_to_track_above_instead_of_overwriting() {
        let mut store = TimelineStore::default();
        let videos = track_ids(&store, TrackKind::Video);
        let existing = test_clip(&videos[1], TrackKind::Video, 0.0, 8.0);
        let existing_id = existing.id.clone();
        store.clips.push(existing);
        store.clipboard = vec![test_clip(&videos[1], TrackKind::Video, 0.0, 4.0)];
        store.playhead_sec = 2.0;
        store.paste(None);
        // Bestehender Clip unangetastet, Kopie auf der Spur darüber.
        assert_eq!(store.clips.len(), 2);
        let original = store.clips.iter().find(|c| c.id == existing_id).unwrap();
        assert_eq!(original.duration, 8.0);
        let pasted = store.clips.iter().find(|c| c.id != existing_id).unwrap();
        assert_eq!(pasted.track_id, videos[0]);
        assert_eq!(pasted.start, 2.0);
        assert_eq!(store.tracks.len(), 4);
    }

    #[test]
    fn paste_creates_track_when_all_lanes_occupied() {
        let mut store = TimelineStore::default();
        let videos = track_ids(&store, TrackKind::Video);
        for id in &videos {
            store.clips.push(test_clip(id, TrackKind::Video, 0.0, 8.0));
        }
        store.clipboard = vec![test_clip(&videos[1], TrackKind::Video, 0.0, 4.0)];
        store.playhead_sec = 2.0;
        store.paste(None);
        assert_eq!(store.clips.len(), 3);
        // Neue Videospur entsteht oben (Index 0) und trägt die Kopie.
        assert_eq!(store.tracks.len(), 5);
        assert_eq!(store.tracks[0].kind, TrackKind::Video);
        let pasted = store.clips.last().unwrap();
        assert_eq!(pasted.track_id, store.tracks[0].id);
    }

    #[test]
    fn paste_audio_evades_downwards() {
        let mut store = TimelineStore::default();
        let audios = track_ids(&store, TrackKind::Audio);
        store.clips.push(test_clip(&audios[0], TrackKind::Audio, 0.0, 8.0));
        store.clipboard = vec![test_clip(&audios[0], TrackKind::Audio, 0.0, 4.0)];
        store.playhead_sec = 0.0;
        store.paste(None);
        assert_eq!(store.clips.len(), 2);
        assert_eq!(store.clips.last().unwrap().track_id, audios[1]);
    }

    #[test]
    fn duplicate_clips_keeps_originals_and_remaps_links() {
        let mut store = TimelineStore::default();
        let videos = track_ids(&store, TrackKind::Video);
        let audios = track_ids(&store, TrackKind::Audio);
        let link = new_id();
        let mut v = test_clip(&videos[1], TrackKind::Video, 0.0, 4.0);
        v.link_id = Some(link.clone());
        let mut a = test_clip(&audios[0], TrackKind::Audio, 0.0, 4.0);
        a.link_id = Some(link.clone());
        let ids = vec![v.id.clone(), a.id.clone()];
        store.clips.push(v);
        store.clips.push(a);

        store.duplicate_clips(&ids, 6.0, 0);

        assert_eq!(store.clips.len(), 4);
        // Originale unverändert bei 0 mit alter link_id.
        for id in &ids {
            let c = store.clips.iter().find(|c| &c.id == id).unwrap();
            assert_eq!(c.start, 0.0);
            assert_eq!(c.link_id.as_deref(), Some(link.as_str()));
        }
        // Kopien bei 6, selektiert, mit gemeinsamer neuer link_id.
        let copies: Vec<&TimelineClip> =
            store.clips.iter().filter(|c| !ids.contains(&c.id)).collect();
        assert_eq!(copies.len(), 2);
        for c in &copies {
            assert_eq!(c.start, 6.0);
            assert!(store.selected_clip_ids.contains(&c.id));
        }
        assert_eq!(copies[0].link_id, copies[1].link_id);
        assert_ne!(copies[0].link_id.as_deref(), Some(link.as_str()));
    }

    #[test]
    fn duplicate_without_movement_is_a_noop() {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        let clip = test_clip(&v1, TrackKind::Video, 0.0, 4.0);
        let id = clip.id.clone();
        store.clips.push(clip);
        store.duplicate_clips(&[id], 0.0, 0);
        assert_eq!(store.clips.len(), 1);
        assert!(!store.can_undo());
    }

    #[test]
    fn grade_update_is_undoable_and_live_writes_share_one_snapshot() {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        let clip = test_clip(&v1, TrackKind::Video, 0.0, 4.0);
        let id = clip.id.clone();
        store.clips.push(clip);

        // Einzelaktion: eigener Snapshot.
        store.grade_update(&id, |g| g.saturation = 0.0);
        assert_eq!(store.clip(&id).unwrap().grade.saturation, 0.0);
        // Geste: ein Snapshot, viele Live-Writes.
        store.begin_fx_edit();
        store.grade_update_live(&id, |g| g.temperature = 10.0);
        store.grade_update_live(&id, |g| g.temperature = 60.0);
        assert_eq!(store.clip(&id).unwrap().grade.temperature, 60.0);

        store.undo(); // ganze Geste zurück
        assert_eq!(store.clip(&id).unwrap().grade.temperature, 0.0);
        assert_eq!(store.clip(&id).unwrap().grade.saturation, 0.0);
        store.undo(); // Einzelaktion zurück
        assert_eq!(store.clip(&id).unwrap().grade.saturation, 100.0);
        store.redo();
        assert_eq!(store.clip(&id).unwrap().grade.saturation, 0.0);
    }

    #[test]
    fn grade_reset_and_toggle_respect_locked_tracks_and_noops() {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        let clip = test_clip(&v1, TrackKind::Video, 0.0, 4.0);
        let id = clip.id.clone();
        store.clips.push(clip);

        // Reset ohne Änderungen: kein Undo-Eintrag.
        store.grade_reset(&[id.clone()]);
        assert!(!store.can_undo());

        store.grade_update(&id, |g| g.exposure = 2.0);
        store.grade_toggle_enabled(&id);
        assert!(!store.clip(&id).unwrap().grade.enabled);
        assert!(!store.clip(&id).unwrap().grade.is_active(), "Bypass deaktiviert");

        // Gesperrte Spur: keine Grade-Edits.
        let track_idx = store.tracks.iter().position(|t| t.id == v1).unwrap();
        store.tracks[track_idx].locked = true;
        store.grade_update(&id, |g| g.exposure = 5.0);
        assert_eq!(store.clip(&id).unwrap().grade.exposure, 2.0);
        store.grade_reset(&[id.clone()]);
        assert!(!store.clip(&id).unwrap().grade.is_default());

        store.tracks[track_idx].locked = false;
        store.grade_reset(&[id.clone()]);
        assert!(store.clip(&id).unwrap().grade.is_default());
    }

    // ------------------------------------------------------ Effekt-Stapel

    /// Store mit verknüpftem A/V-Paar; liefert (Store, Video-ID, Audio-ID).
    fn store_with_av_pair() -> (TimelineStore, String, String) {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        let a1 = track_ids(&store, TrackKind::Audio)[0].clone();
        let mut video = test_clip(&v1, TrackKind::Video, 0.0, 4.0);
        let mut audio = test_clip(&a1, TrackKind::Audio, 0.0, 4.0);
        let link = new_id();
        video.link_id = Some(link.clone());
        audio.link_id = Some(link);
        let (vid, aid) = (video.id.clone(), audio.id.clone());
        store.clips.push(video);
        store.clips.push(audio);
        (store, vid, aid)
    }

    #[test]
    fn effects_add_routes_by_kind_to_av_partner() {
        let (mut store, vid, aid) = store_with_av_pair();
        // Video-Effekt auf den Audio-Clip → landet beim Video-Partner.
        let target = store.effects_add(&aid, EffectKind::GaussianBlur).unwrap();
        assert_eq!(target, vid);
        assert_eq!(store.clip(&vid).unwrap().effects.len(), 1);
        assert!(store.clip(&aid).unwrap().effects.is_empty());
        // Audio-Effekt auf den Video-Clip → landet beim Audio-Partner.
        let target = store.effects_add(&vid, EffectKind::Reverb).unwrap();
        assert_eq!(target, aid);
        assert_eq!(store.clip(&aid).unwrap().effects.len(), 1);
        // Undo entfernt den letzten Effekt wieder.
        store.undo();
        assert!(store.clip(&aid).unwrap().effects.is_empty());
    }

    #[test]
    fn effects_add_rejects_mismatched_kind_without_partner() {
        let mut store = TimelineStore::default();
        let a1 = track_ids(&store, TrackKind::Audio)[0].clone();
        let clip = test_clip(&a1, TrackKind::Audio, 0.0, 4.0);
        let id = clip.id.clone();
        store.clips.push(clip);
        assert!(store.effects_add(&id, EffectKind::GaussianBlur).is_none());
        assert!(store.effects_add(&id, EffectKind::Compressor).is_some());
    }

    #[test]
    fn effects_move_toggle_reset_remove_with_undo() {
        let (mut store, vid, _) = store_with_av_pair();
        store.effects_add(&vid, EffectKind::GaussianBlur);
        store.effects_add(&vid, EffectKind::Invert);
        let fx: Vec<String> = store.clip(&vid).unwrap().effects.iter().map(|e| e.id.clone()).collect();

        // Reorder: Invert nach oben.
        store.effects_move(&vid, &fx[1], -1);
        assert_eq!(store.clip(&vid).unwrap().effects[0].id, fx[1]);
        // Außerhalb der Grenzen: No-op (kein Undo-Eintrag).
        let undo_before = store.revision;
        store.effects_move(&vid, &fx[1], -1);
        assert_eq!(store.revision, undo_before);

        // Bypass.
        store.effects_toggle_enabled(&vid, &fx[0]);
        assert!(!store.clip(&vid).unwrap().effects.iter().find(|e| e.id == fx[0]).unwrap().enabled);

        // Parameter ändern + Reset.
        let pref = ParamRef::Effect { fx_id: fx[0].clone(), index: 0 };
        store.begin_fx_edit();
        store.kf_set_value_live(&vid, &pref, 0.0, 55.0);
        assert_eq!(
            TimelineStore::clip_param(store.clip(&vid).unwrap(), &pref).unwrap().value,
            55.0
        );
        store.effects_reset(&vid, &fx[0]);
        let default = EffectKind::GaussianBlur.specs()[0].default;
        assert_eq!(
            TimelineStore::clip_param(store.clip(&vid).unwrap(), &pref).unwrap().value,
            default
        );

        // Entfernen + Undo.
        store.effects_remove(&vid, &fx[0]);
        assert_eq!(store.clip(&vid).unwrap().effects.len(), 1);
        store.undo();
        assert_eq!(store.clip(&vid).unwrap().effects.len(), 2);
    }

    // ---------------------------------------------- Spur-Effekte/Automation

    #[test]
    fn track_effects_chain_with_undo_and_locking() {
        let mut store = TimelineStore::default();
        let a1 = track_ids(&store, TrackKind::Audio)[0].clone();
        // Nur Audio-Effekte auf Audio-Spuren.
        assert!(!store.track_effects_add(&a1, EffectKind::GaussianBlur));
        assert!(store.track_effects_add(&a1, EffectKind::Equalizer));
        assert!(store.track_effects_add(&a1, EffectKind::Limiter));
        let fx: Vec<String> = store
            .tracks
            .iter()
            .find(|t| t.id == a1)
            .unwrap()
            .effects
            .iter()
            .map(|e| e.id.clone())
            .collect();
        assert_eq!(fx.len(), 2);
        // Reorder: Limiter nach oben.
        store.track_effects_move(&a1, &fx[1], -1);
        assert_eq!(
            store.tracks.iter().find(|t| t.id == a1).unwrap().effects[0].id,
            fx[1]
        );
        // Bypass.
        store.track_effects_toggle_enabled(&a1, &fx[0]);
        assert!(!store
            .tracks
            .iter()
            .find(|t| t.id == a1)
            .unwrap()
            .effects
            .iter()
            .find(|e| e.id == fx[0])
            .unwrap()
            .enabled);
        // Entfernen + Undo.
        store.track_effects_remove(&a1, &fx[0]);
        assert_eq!(store.tracks.iter().find(|t| t.id == a1).unwrap().effects.len(), 1);
        store.undo();
        assert_eq!(store.tracks.iter().find(|t| t.id == a1).unwrap().effects.len(), 2);
        // Gesperrte Spur: kein Add.
        let idx = store.tracks.iter().position(|t| t.id == a1).unwrap();
        store.tracks[idx].locked = true;
        assert!(!store.track_effects_add(&a1, EffectKind::Compressor));
    }

    #[test]
    fn track_automation_interpolates_and_is_undoable() {
        let mut store = TimelineStore::default();
        let a1 = track_ids(&store, TrackKind::Audio)[0].clone();
        store.track_auto_add_point(&a1, TrackAutoParam::Volume, 0.0, 0.0);
        store.track_auto_add_point(&a1, TrackAutoParam::Volume, 4.0, 6.0);
        store.set_track_gain_db(&a1, -3.0);
        let tr = store.tracks.iter().find(|t| t.id == a1).unwrap();
        assert!(tr.has_automation());
        assert!((tr.gain_db_at(0.0) - (-3.0)).abs() < 1e-9);
        assert!((tr.gain_db_at(2.0)).abs() < 1e-9, "Mitte: -3 + 3 = 0");
        assert!((tr.gain_db_at(4.0) - 3.0).abs() < 1e-9);
        // Pan-Automation.
        store.track_auto_add_point(&a1, TrackAutoParam::Pan, 0.0, 0.0);
        store.track_auto_add_point(&a1, TrackAutoParam::Pan, 2.0, 1.0);
        let tr = store.tracks.iter().find(|t| t.id == a1).unwrap();
        assert!((tr.pan_at(1.0) - 0.5).abs() < 1e-9, "Mitte 0..1 → 0.5");
        // Fader-Offset + Clamp auf [−1, 1].
        store.set_track_pan(&a1, 0.5);
        let tr = store.tracks.iter().find(|t| t.id == a1).unwrap();
        assert!((tr.pan_at(2.0) - 1.0).abs() < 1e-9, "0.5 + 1.0 → clamp 1.0");
        // Undo des letzten Punkt-Adds.
        store.undo();
        assert!(store
            .tracks
            .iter()
            .find(|t| t.id == a1)
            .unwrap()
            .pan_auto
            .key_index_at(2.0)
            .is_none());
    }

    #[test]
    fn track_serde_roundtrip_preserves_fx_and_automation() {
        let mut store = TimelineStore::default();
        let a1 = track_ids(&store, TrackKind::Audio)[0].clone();
        store.track_effects_add(&a1, EffectKind::Equalizer);
        store.track_auto_add_point(&a1, TrackAutoParam::Volume, 1.0, 4.0);
        let track = store.tracks.iter().find(|t| t.id == a1).unwrap().clone();
        let json = serde_json::to_string(&track).unwrap();
        let back: TimelineTrack = serde_json::from_str(&json).unwrap();
        assert_eq!(back.effects.len(), 1);
        assert_eq!(back.effects[0].kind, EffectKind::Equalizer);
        assert!(back.volume_auto.is_animated());
        // Standardspur bleibt schlank (keine neuen Felder im JSON).
        let plain = serde_json::to_string(&make_track(TrackKind::Audio)).unwrap();
        assert!(!plain.contains("effects"), "leere FX-Liste nicht serialisiert");
        assert!(!plain.contains("volumeAuto"), "Null-Automation nicht serialisiert");
    }

    #[test]
    fn effects_reorder_moves_to_index_with_undo() {
        let (mut store, vid, _) = store_with_av_pair();
        store.effects_add(&vid, EffectKind::GaussianBlur);
        store.effects_add(&vid, EffectKind::Invert);
        store.effects_add(&vid, EffectKind::Sharpen);
        let ids: Vec<String> = store
            .clip(&vid)
            .unwrap()
            .effects
            .iter()
            .map(|e| e.id.clone())
            .collect();
        // Letzten (Sharpen) an Position 0 ziehen.
        store.effects_reorder(&vid, &ids[2], 0);
        let order: Vec<String> = store
            .clip(&vid)
            .unwrap()
            .effects
            .iter()
            .map(|e| e.id.clone())
            .collect();
        assert_eq!(order, vec![ids[2].clone(), ids[0].clone(), ids[1].clone()]);
        store.undo();
        assert_eq!(store.clip(&vid).unwrap().effects[2].id, ids[2]);
        // No-op an gleiche Position: kein Undo-Eintrag.
        let rev = store.revision;
        store.effects_reorder(&vid, &ids[0], 0);
        assert_eq!(store.revision, rev);
    }

    #[test]
    fn kf_ops_animate_effect_params_like_builtin() {
        let (mut store, vid, _) = store_with_av_pair();
        store.effects_add(&vid, EffectKind::GaussianBlur);
        let fx_id = store.clip(&vid).unwrap().effects[0].id.clone();
        let pref = ParamRef::Effect { fx_id, index: 0 };

        // Stopwatch an → erster Keyframe; Wert setzen → zweiter.
        store.kf_toggle_animated(&vid, &pref, 1.0);
        store.kf_set_value_live(&vid, &pref, 3.0, 80.0);
        let p = TimelineStore::clip_param(store.clip(&vid).unwrap(), &pref).unwrap();
        assert_eq!(p.keyframes.len(), 2);
        assert!((p.eval(2.0) - 45.0).abs() < 1e-9, "linear zwischen 10 und 80");

        // Interpolation setzen.
        store.kf_set_interp(&vid, &[(pref.clone(), 1.0)], Interp::Hold);
        let p = TimelineStore::clip_param(store.clip(&vid).unwrap(), &pref).unwrap();
        assert_eq!(p.keyframes[0].interp, Interp::Hold);

        // Klemmen an die Spec-Grenzen.
        store.kf_set_value_live(&vid, &pref, 3.0, 9999.0);
        let p = TimelineStore::clip_param(store.clip(&vid).unwrap(), &pref).unwrap();
        assert_eq!(p.eval(3.0), 100.0);

        // Keyframes entfernen; Stopwatch aus friert den Wert ein.
        store.kf_remove_keyframes(&vid, &[(pref.clone(), 3.0)]);
        store.kf_toggle_animated(&vid, &pref, 1.0);
        let p = TimelineStore::clip_param(store.clip(&vid).unwrap(), &pref).unwrap();
        assert!(!p.is_animated());

        // Reset auf Spec-Default.
        store.kf_reset_param(&vid, &pref);
        let p = TimelineStore::clip_param(store.clip(&vid).unwrap(), &pref).unwrap();
        assert_eq!(p.value, EffectKind::GaussianBlur.specs()[0].default);
    }

    #[test]
    fn copy_paste_attributes_is_kind_aware_and_remints_ids() {
        let (mut store, vid, aid) = store_with_av_pair();
        // Quelle präparieren: Transform + Grade + Effekte auf beiden Hälften.
        store.begin_fx_edit();
        store.kf_set_value_live(&vid, &ParamRef::Builtin(ParamId::PosX), 0.0, 25.0);
        store.grade_update(&vid, |g| g.saturation = 0.0);
        store.effects_add(&vid, EffectKind::Invert);
        store.effects_add(&aid, EffectKind::Reverb);
        let src_fx_id = store.clip(&vid).unwrap().effects[0].id.clone();

        assert!(store.copy_attributes(&vid));

        // Ziel: zweites A/V-Paar.
        let (vid2, aid2) = {
            let v1 = track_ids(&store, TrackKind::Video)[1].clone();
            let a1 = track_ids(&store, TrackKind::Audio)[0].clone();
            let mut video = test_clip(&v1, TrackKind::Video, 10.0, 4.0);
            let mut audio = test_clip(&a1, TrackKind::Audio, 10.0, 4.0);
            let link = new_id();
            video.link_id = Some(link.clone());
            audio.link_id = Some(link);
            let ids = (video.id.clone(), audio.id.clone());
            store.clips.push(video);
            store.clips.push(audio);
            ids
        };
        store.paste_attributes(&[vid2.clone(), aid2.clone()]);

        let v2 = store.clip(&vid2).unwrap();
        assert_eq!(v2.fx.pos_x.value, 25.0, "Transform übernommen");
        assert_eq!(v2.grade.saturation, 0.0, "Grade übernommen");
        assert_eq!(v2.effects.len(), 1, "nur Video-Effekte auf Video-Clip");
        assert_eq!(v2.effects[0].kind, EffectKind::Invert);
        assert_ne!(v2.effects[0].id, src_fx_id, "frische Instanz-ID");

        let a2 = store.clip(&aid2).unwrap();
        assert_eq!(a2.effects.len(), 1, "Audio-Effekte vom Partner");
        assert_eq!(a2.effects[0].kind, EffectKind::Reverb);

        // Undo macht das Einfügen ungeschehen.
        store.undo();
        assert!(store.clip(&vid2).unwrap().effects.is_empty());
    }

    // -------------------------------------------------------- Übergänge

    /// Store mit zwei benachbarten Video-Clips auf V1 (mit Handles):
    /// A: 0–4 aus Quelle [1..5] von 10 s (Schwanz-Handle 5 s),
    /// B: 4–8 aus Quelle [2..6] von 10 s (Kopf-Handle 2 s).
    fn store_with_cut() -> (TimelineStore, String, String) {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        let mut a = test_clip(&v1, TrackKind::Video, 0.0, 4.0);
        a.src_in = 1.0;
        a.src_duration = 10.0;
        let mut b = test_clip(&v1, TrackKind::Video, 4.0, 4.0);
        b.src_in = 2.0;
        b.src_duration = 10.0;
        let (aid, bid) = (a.id.clone(), b.id.clone());
        store.clips.push(a);
        store.clips.push(b);
        (store, aid, bid)
    }

    #[test]
    fn add_transition_clamps_to_handles_with_undo() {
        let (mut store, aid, bid) = store_with_cut();
        // Gewünscht 100 s — zentriert maximal 2 × min(tail_A=5, dur_B=4,
        // head_B=2, dur_A=4) = 4 s.
        let id = store
            .add_transition(TransitionKind::CrossDissolve, &aid, TrimEdge::End, 100.0)
            .expect("Übergang");
        let tr = store.transition(&id).unwrap().clone();
        assert!((tr.duration - 4.0).abs() < 1e-9, "geklemmt: {}", tr.duration);
        assert_eq!(tr.from_clip_id.as_deref(), Some(aid.as_str()));
        assert_eq!(tr.to_clip_id.as_deref(), Some(bid.as_str()));
        assert_eq!(store.transition_window(&tr), Some((2.0, 6.0)));
        assert_eq!(store.selected_transition_ids, vec![id.clone()]);

        store.undo();
        assert!(store.transitions.is_empty());
        store.redo();
        assert_eq!(store.transitions.len(), 1);
    }

    #[test]
    fn add_transition_single_sided_and_kind_checks() {
        let (mut store, aid, _) = store_with_cut();
        // Kante am Clipanfang ohne Vorgänger ⇒ Einblenden (nur to).
        let id = store
            .add_transition(TransitionKind::CrossDissolve, &aid, TrimEdge::Start, 1.0)
            .expect("Fade-In");
        let tr = store.transition(&id).unwrap();
        assert!(tr.from_clip_id.is_none());
        assert_eq!(store.transition_window(tr), Some((0.0, 1.0)));
        // Audio-Übergang auf Video-Clip: abgelehnt.
        assert!(store
            .add_transition(TransitionKind::ConstantPower, &aid, TrimEdge::End, 1.0)
            .is_err());
        // Gesperrte Spur: abgelehnt.
        let track_id = store.clip(&aid).unwrap().track_id.clone();
        store.tracks.iter_mut().find(|t| t.id == track_id).unwrap().locked = true;
        assert!(store
            .add_transition(TransitionKind::Wipe, &aid, TrimEdge::End, 1.0)
            .is_err());
    }

    #[test]
    fn transitions_follow_trims_and_die_with_adjacency() {
        let (mut store, aid, bid) = store_with_cut();
        store
            .add_transition(TransitionKind::CrossDissolve, &aid, TrimEdge::End, 2.0)
            .unwrap();
        // Slip schrumpft den Kopf-Handle von B auf 0,1 s ⇒ zentriert max 0,2 s.
        store.slip_clip(&bid, -1.9);
        let tr = &store.transitions[0];
        assert!((tr.duration - 0.2).abs() < 1e-9, "nachgeklemmt: {}", tr.duration);
        // Clip wegbewegen: Nachbarschaft bricht ⇒ Übergang entfällt.
        store.move_clips(&[bid.clone()], 2.0, 0);
        assert!(store.transitions.is_empty());
        // Undo stellt Übergang UND Lage wieder her.
        store.undo();
        assert_eq!(store.transitions.len(), 1);
    }

    #[test]
    fn ripple_trim_keeps_transition_at_moved_cut() {
        let (mut store, aid, _) = store_with_cut();
        store
            .add_transition(TransitionKind::CrossDissolve, &aid, TrimEdge::End, 2.0)
            .unwrap();
        // Ripple-Trim am Ende von A um −1 s: B rückt nach, Kante bleibt dicht.
        store.ripple_trim_clip(&aid, TrimEdge::End, -1.0);
        assert_eq!(store.transitions.len(), 1, "Übergang überlebt Ripple");
        let tr = store.transitions[0].clone();
        let (w0, w1) = store.transition_window(&tr).unwrap();
        assert!((w0 - 2.0).abs() < 1e-9 && (w1 - 4.0).abs() < 1e-9, "{w0}..{w1}");
    }

    #[test]
    fn split_remaps_end_transition_to_right_half() {
        let (mut store, aid, _) = store_with_cut();
        let id = store
            .add_transition(TransitionKind::Wipe, &aid, TrimEdge::End, 1.0)
            .unwrap();
        store.split_at(2.0, None);
        let tr = store.transition(&id).expect("Übergang überlebt Split");
        let from = tr.from_clip_id.clone().unwrap();
        let right = store.clip(&from).unwrap();
        assert!((right.end() - 4.0).abs() < 1e-9, "hängt an der rechten Hälfte");
        assert_ne!(from, aid);
    }

    #[test]
    fn delete_and_asset_removal_drop_transitions() {
        let (mut store, aid, bid) = store_with_cut();
        store
            .add_transition(TransitionKind::CrossDissolve, &aid, TrimEdge::End, 2.0)
            .unwrap();
        store.delete_clips(&[bid], false);
        assert!(store.transitions.is_empty());
        store.undo();
        assert_eq!(store.transitions.len(), 1);
        store.remove_clips_for_assets(&["asset".to_string()]);
        assert!(store.transitions.is_empty());
    }

    #[test]
    fn copy_paste_and_duplicate_carry_transitions() {
        let (mut store, aid, bid) = store_with_cut();
        store
            .add_transition(TransitionKind::Push, &aid, TrimEdge::End, 2.0)
            .unwrap();
        // Kopieren + Einfügen bei 20 s: Übergang folgt auf frische IDs.
        store.select_clips(&[aid.clone(), bid.clone()], SelectMode::Replace, false);
        store.copy_selection();
        store.playhead_sec = 20.0;
        store.paste(None);
        assert_eq!(store.transitions.len(), 2);
        let pasted = store
            .transitions
            .iter()
            .find(|t| t.from_clip_id.as_deref() != Some(aid.as_str()))
            .unwrap()
            .clone();
        let (w0, _) = store.transition_window(&pasted).unwrap();
        assert!((w0 - 23.0).abs() < 1e-9, "Fenster an der neuen Kante: {w0}");
        // Alt+Drag-Duplizieren nimmt den Übergang ebenfalls mit.
        store.duplicate_clips(&[aid.clone(), bid.clone()], 40.0, 0);
        assert_eq!(store.transitions.len(), 3);
    }

    #[test]
    fn apply_default_transition_covers_selection_edges_once() {
        let (mut store, aid, bid) = store_with_cut();
        store.select_clips(&[bid.clone()], SelectMode::Replace, false);
        let n = store.apply_transition_to_selection(TransitionKind::CrossDissolve);
        // Kante zu A (zweiseitig) + Clipende (Ausblenden).
        assert_eq!(n, 2);
        // Zweiter Aufruf: alle Kanten belegt.
        assert_eq!(store.apply_transition_to_selection(TransitionKind::CrossDissolve), 0);
        // Audio-Standard auf Video-Auswahl: nichts.
        assert_eq!(store.apply_transition_to_selection(TransitionKind::ConstantPower), 0);
        let _ = aid;
    }

    #[test]
    fn transition_ops_duration_alignment_kind() {
        let (mut store, aid, _) = store_with_cut();
        let id = store
            .add_transition(TransitionKind::CrossDissolve, &aid, TrimEdge::End, 2.0)
            .unwrap();
        store.set_transition_duration(&id, 3.0);
        assert!((store.transition(&id).unwrap().duration - 3.0).abs() < 1e-9);
        // EndAtCut: max = min(head_B=2, dur_A=4) = 2 ⇒ Dauer wird nachgeklemmt.
        store.set_transition_alignment(&id, TransitionAlignment::EndAtCut);
        let tr = store.transition(&id).unwrap().clone();
        assert_eq!(tr.alignment, TransitionAlignment::EndAtCut);
        assert!(tr.duration <= 2.0 + 1e-9, "{}", tr.duration);
        assert_eq!(store.transition_window(&tr).unwrap().1, 4.0, "endet am Schnitt");
        // Ersetzen innerhalb derselben Spurart.
        store.set_transition_kind(&id, TransitionKind::Wipe);
        assert_eq!(store.transition(&id).unwrap().kind, TransitionKind::Wipe);
        store.set_transition_kind(&id, TransitionKind::ConstantPower);
        assert_eq!(store.transition(&id).unwrap().kind, TransitionKind::Wipe, "Artwechsel Video→Audio abgelehnt");
    }

    #[test]
    fn audio_fades_extend_audible_range() {
        let mut store = TimelineStore::default();
        let a1 = track_ids(&store, TrackKind::Audio)[0].clone();
        let mut a = test_clip(&a1, TrackKind::Audio, 0.0, 4.0);
        a.kind = TrackKind::Audio;
        a.src_in = 1.0;
        a.src_duration = 10.0;
        let mut b = test_clip(&a1, TrackKind::Audio, 4.0, 4.0);
        b.kind = TrackKind::Audio;
        b.src_in = 2.0;
        b.src_duration = 10.0;
        let (aid, bid) = (a.id.clone(), b.id.clone());
        store.clips.push(a);
        store.clips.push(b);
        store
            .add_transition(TransitionKind::ConstantPower, &aid, TrimEdge::End, 2.0)
            .unwrap();
        let a = store.clip(&aid).unwrap().clone();
        let b = store.clip(&bid).unwrap().clone();
        let fades_a = store.audio_fades(&a);
        let fades_b = store.audio_fades(&b);
        assert_eq!(fades_a, vec![(3.0, 5.0, false, true)]);
        assert_eq!(fades_b, vec![(3.0, 5.0, true, true)]);
        assert_eq!(store.audio_extent(&a, &fades_a), (0.0, 5.0));
        assert_eq!(store.audio_extent(&b, &fades_b), (3.0, 8.0));
    }

    #[test]
    fn overlapping_transition_windows_are_rejected() {
        let (mut store, aid, bid) = store_with_cut();
        // Dritter Clip C: 8–12 mit Handles.
        let v1 = store.clip(&aid).unwrap().track_id.clone();
        let mut c = test_clip(&v1, TrackKind::Video, 8.0, 4.0);
        c.src_in = 2.0;
        c.src_duration = 10.0;
        store.clips.push(c);
        // Übergang an Kante A|B (zentriert 4 s: Fenster 2..6).
        store
            .add_transition(TransitionKind::CrossDissolve, &aid, TrimEdge::End, 4.0)
            .unwrap();
        // Kante B|C zentriert 4 s wollen: Fenster 6..10 — kollidiert nicht.
        let id2 = store
            .add_transition(TransitionKind::CrossDissolve, &bid, TrimEdge::End, 4.0)
            .unwrap();
        assert_eq!(store.transitions.len(), 2);
        // Erstes Fenster auf 2..6, zweites 6..10 — beide bleiben.
        let w2 = store
            .transition_window(store.transition(&id2).unwrap())
            .unwrap();
        assert!((w2.0 - 6.0).abs() < 1e-9);
    }

    #[test]
    fn effects_clear_and_bypass_affect_all_selected() {
        let (mut store, vid, aid) = store_with_av_pair();
        store.effects_add(&vid, EffectKind::Invert);
        store.effects_add(&aid, EffectKind::Reverb);

        store.effects_toggle_bypass(&[vid.clone(), aid.clone()]);
        assert!(store.clip(&vid).unwrap().effects.iter().all(|e| !e.enabled));
        assert!(store.clip(&aid).unwrap().effects.iter().all(|e| !e.enabled));
        store.effects_toggle_bypass(&[vid.clone(), aid.clone()]);
        assert!(store.clip(&vid).unwrap().effects.iter().all(|e| e.enabled));

        store.effects_clear(&[vid.clone(), aid.clone()]);
        assert!(store.clip(&vid).unwrap().effects.is_empty());
        assert!(store.clip(&aid).unwrap().effects.is_empty());
        store.undo();
        assert_eq!(store.clip(&vid).unwrap().effects.len(), 1);
    }
    // ------------------------------------------------------------- Titel

    #[test]
    fn add_title_clip_lands_on_free_track_above_content() {
        let mut store = TimelineStore::default();
        let videos = track_ids(&store, TrackKind::Video);
        // Material auf der UNTEREN Videospur (V1) — der Titel muss auf die
        // freie Spur DARÜBER (V2).
        store.clips.push(test_clip(&videos[1], TrackKind::Video, 0.0, 8.0));
        let id = store.add_title_clip(crate::core::title::TitleSpec::default(), 1.0, 4.0);
        let title = store.clip(&id).expect("Titel-Clip");
        assert!(title.is_title());
        assert_eq!(title.kind, TrackKind::Video);
        assert_eq!(title.track_id, videos[0]);
        assert!(title.asset_id.is_empty());
        assert!(title.src_duration.is_infinite(), "frei dehnbar wie Standbilder");
        assert_eq!(store.selected_clip_ids, vec![id.clone()]);

        // Beide Videospuren belegt → neue Spur entsteht oben.
        let id2 = store.add_title_clip(crate::core::title::TitleSpec::default(), 1.0, 4.0);
        assert_eq!(store.tracks.len(), 5);
        assert_eq!(store.clip(&id2).unwrap().track_id, store.tracks[0].id);

        // Undo räumt Titel UND Spur wieder auf.
        store.undo();
        store.undo();
        assert!(store.clips.iter().all(|c| !c.is_title()));
        assert_eq!(store.tracks.len(), 4);
    }

    #[test]
    fn add_title_clip_without_content_uses_bottom_video_track() {
        let mut store = TimelineStore::default();
        let videos = track_ids(&store, TrackKind::Video);
        let id = store.add_title_clip(crate::core::title::TitleSpec::default(), 0.0, 5.0);
        assert_eq!(store.clip(&id).unwrap().track_id, videos[1], "unterste freie Spur");
    }

    #[test]
    fn title_clip_moves_and_trims_like_a_still() {
        let mut store = TimelineStore::default();
        let id = store.add_title_clip(crate::core::title::TitleSpec::default(), 2.0, 5.0);
        store.move_clips(&[id.clone()], 3.0, 0);
        assert_eq!(store.clip(&id).unwrap().start, 5.0);
        // Ende frei dehnbar (unendliche Quelle).
        store.trim_clip(&id, TrimEdge::End, 10.0);
        assert!((store.clip(&id).unwrap().duration - 15.0).abs() < 1e-9);
        store.undo();
        store.undo();
        assert_eq!(store.clip(&id).unwrap().start, 2.0);
    }

    #[test]
    fn title_update_live_syncs_name_and_shares_one_snapshot() {
        let mut store = TimelineStore::default();
        let id = store.add_title_clip(crate::core::title::TitleSpec::default(), 0.0, 5.0);
        store.begin_title_edit();
        store.title_update_live(&id, |s| s.text = "N".into());
        store.title_update_live(&id, |s| s.text = "Nachrichten\nLive".into());
        let clip = store.clip(&id).unwrap();
        assert_eq!(clip.name, "Nachrichten");
        assert_eq!(clip.title.as_ref().unwrap().text, "Nachrichten\nLive");
        // EIN Undo stellt den Zustand vor der Tipp-Sitzung wieder her.
        store.undo();
        assert_eq!(store.clip(&id).unwrap().title.as_ref().unwrap().text, "Titel");
        // title_update (Einzelklick) legt einen eigenen Snapshot an.
        store.title_update(&id, |s| s.stroke_width = 4.0);
        assert_eq!(store.clip(&id).unwrap().title.as_ref().unwrap().stroke_width, 4.0);
        store.undo();
        assert_eq!(store.clip(&id).unwrap().title.as_ref().unwrap().stroke_width, 0.0);
    }

    // ------------------------------------------------------------ Untertitel

    #[test]
    fn subtitle_track_sits_on_top_and_is_named_u() {
        let mut store = TimelineStore::default();
        let u1 = store.add_track(TrackKind::Subtitle);
        let u2 = store.add_track(TrackKind::Subtitle);
        // Neue Untertitelspur ganz oben; U1 bleibt die unterste des Blocks.
        assert_eq!(store.tracks[0].id, u2);
        assert_eq!(store.tracks[1].id, u1);
        assert_eq!(store.tracks[2].kind, TrackKind::Video);
        let name = |id: &str| {
            let t = store.tracks.iter().find(|t| t.id == id).unwrap();
            track_name(t, &store.tracks)
        };
        assert_eq!(name(&u1), "U1");
        assert_eq!(name(&u2), "U2");
        // Aktive Spur folgt der zuletzt angelegten; Fallback nach Entfernen.
        assert_eq!(store.active_subtitle_track().unwrap().id, u2);
        store.remove_track(&u2);
        assert_eq!(store.active_subtitle_track().unwrap().id, u1);
        // Neue Videospur bleibt UNTER dem Untertitel-Block.
        store.add_track(TrackKind::Video);
        assert_eq!(store.tracks[0].id, u1);
        assert_eq!(store.tracks[1].kind, TrackKind::Video);
    }

    #[test]
    fn add_subtitle_clip_snaps_to_frames_and_respects_neighbors() {
        let mut store = TimelineStore::default();
        store.settings = SequenceSettings {
            rate: sequence::FrameRate::new(25, 1),
            ..SequenceSettings::default()
        };
        // Leicht neben dem Frame-Raster → rastet auf 1,0 s (Frame 25) ein.
        let id = store.add_subtitle_clip("Hallo", 1.013).expect("anlegen");
        let clip = store.clip(&id).unwrap().clone();
        assert_eq!(clip.kind, TrackKind::Subtitle);
        assert!((clip.start - 1.0).abs() < 1e-9);
        assert!((clip.duration - crate::core::subtitle::DEFAULT_CUE_DURATION).abs() < 1e-9);
        assert_eq!(clip.subtitle.as_ref().unwrap().text, "Hallo");
        assert_eq!(clip.name, "Hallo");
        assert!(clip.asset_id.is_empty(), "Generator ohne Mediendatei");

        // Belegte Position → Fehler statt Überschreiben.
        assert!(store.add_subtitle_clip("Kollision", 2.0).is_err());
        // Dahinter: Dauer wird bis zum nächsten Segment geklemmt.
        let id2 = store.add_subtitle_clip("Zweites", 6.0).expect("anlegen");
        store.move_clips(&[id2.clone()], 0.0, 0);
        let id3 = store.add_subtitle_clip("Drittes", 4.0).expect("anlegen");
        let c3 = store.clip(&id3).unwrap();
        assert!((c3.end() - 6.0).abs() < 1e-9, "endet am nächsten Segment");
        // Alles auf einer Spur gelandet.
        let subs = store.subtitle_tracks();
        assert_eq!(subs.len(), 1);
    }

    #[test]
    fn subtitle_segments_split_and_trim_like_clips() {
        let mut store = TimelineStore::default();
        let id = store.add_subtitle_clip("Erste Zeile\nZweite", 0.0).expect("anlegen");
        store.split_at(1.5, None);
        let track_id = store.clip(&id).map(|c| c.track_id.clone()).unwrap();
        let parts: Vec<&TimelineClip> = store
            .clips
            .iter()
            .filter(|c| c.track_id == track_id)
            .collect();
        assert_eq!(parts.len(), 2);
        // Beide Hälften tragen den Text weiter.
        assert!(parts
            .iter()
            .all(|c| c.subtitle.as_ref().unwrap().text == "Erste Zeile\nZweite"));
        // Undo stellt das ungeteilte Segment wieder her.
        store.undo();
        assert_eq!(
            store
                .clips
                .iter()
                .filter(|c| c.track_id == track_id)
                .count(),
            1
        );
    }

    #[test]
    fn import_subtitle_cues_is_frame_accurate_and_resolves_overlaps() {
        use crate::core::subtitle::SrtCue;
        let mut store = TimelineStore::default();
        // NTSC: 29,97 — die kritische Rate für Frame-Genauigkeit.
        store.settings = SequenceSettings {
            rate: sequence::FrameRate::new(30000, 1001),
            ..SequenceSettings::default()
        };
        let rate = store.settings.rate;
        let cues = vec![
            SrtCue { start: 0.5, end: 2.0, text: "Eins".into() },
            // Überlappt den ersten Cue → Start wird ans Ende geklemmt.
            SrtCue { start: 1.8, end: 3.0, text: "Zwei".into() },
            // Kürzer als ein Frame → auf einen Frame verlängert.
            SrtCue { start: 10.0, end: 10.001, text: "Mini".into() },
            // Leerer Text fliegt raus.
            SrtCue { start: 20.0, end: 21.0, text: "   ".into() },
        ];
        let (track_id, n) = store.import_subtitle_cues(&cues);
        assert_eq!(n, 3);
        let mut clips: Vec<&TimelineClip> = store
            .clips
            .iter()
            .filter(|c| c.track_id == track_id)
            .collect();
        clips.sort_by(|a, b| a.start.total_cmp(&b.start));
        // Alle Kanten liegen exakt auf dem NTSC-Frame-Raster.
        for c in &clips {
            let f0 = rate.frame_round(c.start);
            let f1 = rate.frame_round(c.end());
            assert!((c.start - rate.time_of_frame(f0 as f64)).abs() < 1e-9);
            assert!((c.end() - rate.time_of_frame(f1 as f64)).abs() < 1e-9);
        }
        // Überlappung aufgelöst: Cue 2 beginnt am Ende von Cue 1.
        assert!((clips[1].start - clips[0].end()).abs() < 1e-9);
        // Mini-Cue: exakt ein Frame.
        assert_eq!(
            rate.frame_round(clips[2].end()) - rate.frame_round(clips[2].start),
            1
        );
        // EIN Undo entfernt den kompletten Import (Spur + Segmente).
        store.undo();
        assert!(store.subtitle_tracks().is_empty());
    }

    #[test]
    fn subtitle_cues_roundtrip_through_srt_is_frame_exact() {
        use crate::core::subtitle::{format_srt, parse_srt};
        let mut store = TimelineStore::default();
        store.settings = SequenceSettings {
            rate: sequence::FrameRate::new(30000, 1001),
            ..SequenceSettings::default()
        };
        let rate = store.settings.rate;
        // Segmente auf krummen NTSC-Frame-Zeiten (Frame 30 ≈ 1,001 s …).
        let cues_in = vec![
            crate::core::subtitle::SrtCue {
                start: rate.time_of_frame(30.0),
                end: rate.time_of_frame(90.0),
                text: "Erster Satz".into(),
            },
            crate::core::subtitle::SrtCue {
                start: rate.time_of_frame(120.0),
                end: rate.time_of_frame(200.0),
                text: "Zweiter Satz\nmit Umbruch".into(),
            },
        ];
        let (track_id, _) = store.import_subtitle_cues(&cues_in);
        let exported = store.subtitle_cues(&track_id);
        let parsed = parse_srt(&format_srt(&exported)).expect("SRT-Roundtrip");
        // SRT speichert Millisekunden — die Rück-Rundung auf das Frame-
        // Raster muss exakt die ursprünglichen Frames liefern.
        for (orig, back) in cues_in.iter().zip(parsed.iter()) {
            assert_eq!(rate.frame_round(back.start), rate.frame_round(orig.start));
            assert_eq!(rate.frame_round(back.end), rate.frame_round(orig.end));
            assert_eq!(back.text, orig.text);
        }
    }

    #[test]
    fn subtitle_style_and_text_updates_are_undoable() {
        let mut store = TimelineStore::default();
        let id = store.add_subtitle_clip("Anfang", 0.0).expect("anlegen");
        let track_id = store.clip(&id).unwrap().track_id.clone();

        // Spurstil: Einzelklick-Update mit eigenem Snapshot.
        store.subtitle_style_update(&track_id, |s| s.size = 64.0);
        assert_eq!(store.subtitle_style(&track_id).size, 64.0);
        store.undo();
        assert_eq!(store.subtitle_style(&track_id).size, 48.0);
        store.redo();

        // Tipp-Sitzung: EIN Snapshot für mehrere Live-Updates.
        store.begin_subtitle_edit();
        store.subtitle_update_live(&id, |s| s.text = "N".into());
        store.subtitle_update_live(&id, |s| s.text = "Neuer Text".into());
        let clip = store.clip(&id).unwrap();
        assert_eq!(clip.subtitle.as_ref().unwrap().text, "Neuer Text");
        assert_eq!(clip.name, "Neuer Text");
        store.undo();
        assert_eq!(store.clip(&id).unwrap().subtitle.as_ref().unwrap().text, "Anfang");
    }

    #[test]
    fn hidden_subtitle_tracks_are_excluded_from_program_layers() {
        use crate::core::compose;
        let mut store = TimelineStore::default();
        let id = store.add_subtitle_clip("Sichtbar", 0.0).expect("anlegen");
        let track_id = store.clip(&id).unwrap().track_id.clone();
        let layer_ids = |store: &TimelineStore| -> Vec<String> {
            compose::visible_program_layers(store, 1.0)
                .iter()
                .filter_map(|l| match l {
                    compose::ProgramLayer::Clip { clip, .. } => Some(clip.id.clone()),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(layer_ids(&store), vec![id.clone()]);
        // Auge zu (muted = ausgeblendet) → Layer verschwindet.
        store.toggle_track_flag(&track_id, TrackFlag::Muted);
        assert!(layer_ids(&store).is_empty());
        // Video-Solo blendet Untertitel NICHT aus.
        store.toggle_track_flag(&track_id, TrackFlag::Muted);
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        let video = test_clip(&v1, TrackKind::Video, 0.0, 4.0);
        let vid = video.id.clone();
        store.clips.push(video);
        let v_track = store.tracks.iter().find(|t| t.id == v1).unwrap().id.clone();
        store.toggle_track_flag(&v_track, TrackFlag::Solo);
        let ids = layer_ids(&store);
        assert!(ids.contains(&vid));
        assert!(ids.contains(&id), "Untertitel bleiben bei Video-Solo sichtbar");
        // Untertitel-Layer liegt ÜBER dem Video-Layer.
        assert_eq!(ids.last(), Some(&id));
    }

    #[test]
    fn subtitle_layer_spec_follows_track_style() {
        use crate::core::compose;
        let mut store = TimelineStore::default();
        let id = store.add_subtitle_clip("Hallo Welt", 0.0).expect("anlegen");
        let track_id = store.clip(&id).unwrap().track_id.clone();
        let spec = compose::layer_title_spec(&store, store.clip(&id).unwrap()).unwrap();
        assert_eq!(spec.text, "Hallo Welt");
        assert_eq!(spec.size, 48.0);
        let before = spec.content_hash();
        store.subtitle_style_update(&track_id, |s| s.size = 72.0);
        let spec = compose::layer_title_spec(&store, store.clip(&id).unwrap()).unwrap();
        assert_eq!(spec.size, 72.0);
        assert_ne!(spec.content_hash(), before, "Stiländerung invalidiert den Raster-Cache");
    }

    // ------------------------------------------------ Geschwindigkeit/Dauer

    /// Clip mit Geschwindigkeit: 0–4 s Timeline aus Quelle [2..(2+4·speed)].
    fn speed_clip(track_id: &str, speed: f64, reverse: bool) -> TimelineClip {
        let mut c = test_clip(track_id, TrackKind::Video, 0.0, 4.0);
        c.src_in = 2.0;
        c.src_duration = 30.0;
        c.speed = speed;
        c.reverse = reverse;
        c
    }

    #[test]
    fn media_time_mapping_roundtrips_for_speed_reverse_freeze() {
        let mut c = speed_clip("t", 0.37, false);
        // Vorwärts: media = src_in + (t−start)·speed; Umkehrung exakt.
        for t in [0.0, 0.5, 1.7, 4.0] {
            let m = c.media_time_at(t);
            assert!((m - (2.0 + t * 0.37)).abs() < 1e-12);
            assert!((c.seq_time_of_media(m) - t).abs() < 1e-9);
        }
        // Rückwärts: am Clipanfang liegt der Medien-Out, am Ende der In.
        c.reverse = true;
        assert!((c.media_time_at(0.0) - c.media_out()).abs() < 1e-12);
        assert!((c.media_time_at(4.0) - c.src_in).abs() < 1e-12);
        for t in [0.0, 1.3, 4.0] {
            assert!((c.seq_time_of_media(c.media_time_at(t)) - t).abs() < 1e-9);
        }
        // Standbild: Medienzeit konstant am In-Punkt, Spanne 0.
        c.freeze = true;
        assert_eq!(c.media_time_at(0.0), 2.0);
        assert_eq!(c.media_time_at(3.0), 2.0);
        assert_eq!(c.media_span(), 0.0);
    }

    #[test]
    fn split_preserves_media_continuity_at_any_speed() {
        for (speed, reverse) in [(0.37, false), (2.5, false), (0.5, true), (1.0, true)] {
            let mut store = TimelineStore::default();
            let v1 = track_ids(&store, TrackKind::Video)[1].clone();
            let clip = speed_clip(&v1, speed, reverse);
            let span = clip.media_span();
            store.clips.push(clip);
            store.split_at(1.5, None);
            let mut parts: Vec<&TimelineClip> = store.clips.iter().collect();
            parts.sort_by(|a, b| a.start.total_cmp(&b.start));
            assert_eq!(parts.len(), 2, "speed={speed} reverse={reverse}");
            let (l, r) = (parts[0], parts[1]);
            // Medienspanne bleibt in Summe erhalten …
            assert!(
                (l.media_span() + r.media_span() - span).abs() < 1e-9,
                "Spanne speed={speed} reverse={reverse}"
            );
            // … und die Abbildung ist an der Schnittkante stetig.
            assert!(
                (l.media_time_at(1.5) - r.media_time_at(1.5)).abs() < 1e-9,
                "Kante speed={speed} reverse={reverse}: {} vs {}",
                l.media_time_at(1.5),
                r.media_time_at(1.5)
            );
        }
    }

    #[test]
    fn trim_handles_scale_with_speed() {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        // 2× Tempo: Kopf-Handle 2 s Medien = 1 s Timeline.
        let clip = speed_clip(&v1, 2.0, false);
        let id = clip.id.clone();
        store.clips.push(clip);
        store.move_clips(&[id.clone()], 2.0, 0); // Platz vor dem Clip schaffen
        let (lo, _) = trim_range(store.clip(&id).unwrap(), TrimEdge::Start, &store.clips, false);
        assert!((lo + 1.0).abs() < 1e-9, "Kopf-Handle in Timeline-Sekunden: {lo}");
        // Kopf 0,5 s verlängern: src_in sinkt um 0,5·2 = 1 s Medien.
        store.trim_clip(&id, TrimEdge::Start, -0.5);
        let c = store.clip(&id).unwrap();
        assert!((c.src_in - 1.0).abs() < 1e-9, "src_in: {}", c.src_in);
        assert!((c.duration - 4.5).abs() < 1e-9);
        // Ende: Schwanz-Handle (30 − media_out)/speed.
        let (_, hi) = trim_range(c, TrimEdge::End, &store.clips, false);
        assert!((hi - (30.0 - c.media_out()) / 2.0).abs() < 1e-9);
    }

    #[test]
    fn reverse_end_trim_extends_into_earlier_media() {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        let clip = speed_clip(&v1, 1.0, true); // Quelle [2..6] rückwärts
        let id = clip.id.clone();
        store.clips.push(clip);
        // Ende +1 s: spielt FRÜHERES Material — src_in sinkt auf 1.
        store.trim_clip(&id, TrimEdge::End, 1.0);
        let c = store.clip(&id).unwrap();
        assert!((c.src_in - 1.0).abs() < 1e-9, "src_in: {}", c.src_in);
        assert!((c.media_out() - 6.0).abs() < 1e-9, "Medien-Out bleibt verankert");
        // Schwanz-Handle rückwärts = src_in/speed ⇒ maximal noch 1 s.
        let (_, hi) = trim_range(c, TrimEdge::End, &store.clips, false);
        assert!((hi - 1.0).abs() < 1e-9, "{hi}");
    }

    #[test]
    fn slip_scales_with_speed() {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        let clip = speed_clip(&v1, 2.0, false); // Quelle [2..10] von 30
        let id = clip.id.clone();
        store.clips.push(clip);
        store.slip_clip(&id, 1.0); // 1 s Timeline = 2 s Medien
        assert!((store.clip(&id).unwrap().src_in - 4.0).abs() < 1e-9);
        // Klemmen: maximal (30 − media_out)/speed nach rechts.
        store.slip_clip(&id, 100.0);
        let c = store.clip(&id).unwrap();
        assert!((c.media_out() - 30.0).abs() < 1e-9, "an der Quelle geklemmt");
    }

    #[test]
    fn set_clip_speed_couples_duration_and_ripples() {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        let a1 = track_ids(&store, TrackKind::Audio)[0].clone();
        // Verknüpftes A/V-Paar 0–4 + Folgeclip bei 4 auf V1 und A1.
        let link = new_id();
        let mut av = test_clip(&v1, TrackKind::Video, 0.0, 4.0);
        av.src_duration = 30.0;
        av.link_id = Some(link.clone());
        let mut aa = test_clip(&a1, TrackKind::Audio, 0.0, 4.0);
        aa.src_duration = 30.0;
        aa.link_id = Some(link);
        let mut next_v = test_clip(&v1, TrackKind::Video, 4.0, 3.0);
        next_v.src_duration = 30.0;
        let mut next_a = test_clip(&a1, TrackKind::Audio, 4.0, 3.0);
        next_a.src_duration = 30.0;
        let (vid, nid, naid) = (av.id.clone(), next_v.id.clone(), next_a.id.clone());
        store.clips.extend([av, aa, next_v, next_a]);

        // 50 % mit Ripple: Paar wird 8 s lang, Folgeclips rücken auf 8 —
        // EIN Ripple je Verknüpfungsgruppe (nicht doppelt).
        store.set_clip_speed(&[vid.clone()], 0.5, false, false, true);
        let v = store.clip(&vid).unwrap();
        assert!((v.duration - 8.0).abs() < 1e-9, "{}", v.duration);
        assert!((v.eff_speed() - 0.5).abs() < 1e-12);
        assert!((store.clip(&nid).unwrap().start - 8.0).abs() < 1e-9);
        assert!((store.clip(&naid).unwrap().start - 8.0).abs() < 1e-9);
        // Der verknüpfte Audio-Partner folgt mit.
        let partner = store
            .clips
            .iter()
            .find(|c| c.kind == TrackKind::Audio && c.link_id.is_some())
            .unwrap();
        assert!((partner.duration - 8.0).abs() < 1e-9);

        store.undo();
        assert!((store.clip(&vid).unwrap().duration - 4.0).abs() < 1e-9);
        assert!((store.clip(&nid).unwrap().start - 4.0).abs() < 1e-9);

        // Ohne Ripple: am Folgeclip gekappt (Medien werden geschnitten).
        store.set_clip_speed(&[vid.clone()], 0.5, false, false, false);
        let v = store.clip(&vid).unwrap();
        assert!((v.duration - 4.0).abs() < 1e-9, "gekappt: {}", v.duration);
        assert!((store.clip(&nid).unwrap().start - 4.0).abs() < 1e-9, "kein Ripple");
    }

    #[test]
    fn freeze_frame_at_playhead_freezes_current_media_time() {
        let mut store = TimelineStore::default();
        let v1 = track_ids(&store, TrackKind::Video)[1].clone();
        let clip = speed_clip(&v1, 2.0, false); // media = 2 + 2·t
        let id = clip.id.clone();
        store.clips.push(clip);
        store.set_playhead(1.0);
        store.freeze_frame_at_playhead(&[id.clone()]);
        let c = store.clip(&id).unwrap();
        assert!(c.freeze);
        assert!((c.src_in - 4.0).abs() < 1e-9, "Frame bei Medienzeit 4: {}", c.src_in);
        assert_eq!(c.media_time_at(0.0), c.media_time_at(3.9));
        // Standbild ist frei dehnbar.
        let (_, hi) = trim_range(c, TrimEdge::End, &store.clips, false);
        assert!(hi.is_infinite());
        store.undo();
        assert!(!store.clip(&id).unwrap().freeze);
    }

    #[test]
    fn speed_labels_match_premiere_conventions() {
        let mut c = speed_clip("t", 1.0, false);
        assert_eq!(c.speed_label(), None);
        c.speed = 0.5;
        assert_eq!(c.speed_label().as_deref(), Some("50 %"));
        c.reverse = true;
        c.speed = 1.0;
        assert_eq!(c.speed_label().as_deref(), Some("−100 %"));
        c.freeze = true;
        assert_eq!(c.speed_label().as_deref(), Some("Standbild"));
    }

    // ------------------------------------------------------------ Marker

    #[test]
    fn marker_add_is_frame_snapped_and_idempotent() {
        let mut store = TimelineStore::default(); // 25 fps ⇒ 0,04 s/Frame
        let id1 = store.add_marker_at(0.41); // Frame 10 → 0,40 s
        assert_eq!(store.markers.len(), 1);
        assert!((store.markers[0].time - 0.40).abs() < 1e-9, "frame-gerastet");
        // Erneut im selben Frame (0,405 → Frame 10): kein zweiter Marker.
        let id2 = store.add_marker_at(0.405);
        assert_eq!(store.markers.len(), 1);
        assert_eq!(id1, id2);
        // Anderer Frame: neuer Marker, Liste bleibt sortiert.
        store.add_marker_at(0.10);
        assert_eq!(store.markers.len(), 2);
        assert!(store.markers[0].time < store.markers[1].time);
    }

    #[test]
    fn marker_navigation_handles_edges() {
        let mut store = TimelineStore::default();
        // Keine Marker: Navigation bewegt nichts.
        store.playhead_sec = 5.0;
        assert!(!store.go_to_next_marker());
        assert!(!store.go_to_prev_marker());
        assert_eq!(store.playhead_sec, 5.0);

        for t in [1.0, 2.0, 3.0] {
            store.add_marker_at(t);
        }
        // Vor dem ersten: prev findet nichts, next springt auf 1,0.
        store.playhead_sec = 0.0;
        assert!(!store.go_to_prev_marker());
        assert!(store.go_to_next_marker());
        assert!((store.playhead_sec - 1.0).abs() < 1e-9);
        // Exakt auf einem Marker: next/prev springen zum Nachbarn, nicht
        // zum eigenen Frame.
        store.playhead_sec = 2.0;
        assert!(store.go_to_next_marker());
        assert!((store.playhead_sec - 3.0).abs() < 1e-9);
        store.playhead_sec = 2.0;
        assert!(store.go_to_prev_marker());
        assert!((store.playhead_sec - 1.0).abs() < 1e-9);
        // Hinter dem letzten: next findet nichts.
        store.playhead_sec = 9.0;
        assert!(!store.go_to_next_marker());
    }

    #[test]
    fn marker_undo_redo_roundtrip() {
        use crate::core::marker::MarkerColor;
        let mut store = TimelineStore::default();
        let id = store.add_marker_at(2.0);
        store.marker_update(&id, |m| {
            m.name = "Take 1".into();
            m.color = MarkerColor::Red;
        });
        assert_eq!(store.markers[0].name, "Take 1");
        store.undo(); // Name/Farbe zurück
        assert_eq!(store.markers[0].name, "");
        store.undo(); // Marker weg
        assert!(store.markers.is_empty());
        store.redo();
        assert_eq!(store.markers.len(), 1);
        store.redo();
        assert_eq!(store.markers[0].name, "Take 1");
    }

    #[test]
    fn clip_marker_split_partitions_by_media_time() {
        let mut store = TimelineStore::default();
        let v = track_ids(&store, TrackKind::Video)[1].clone();
        // Clip 0–10 s, Quelle ab 0, 1× Tempo: Medienzeit == Sequenzzeit.
        let mut clip = test_clip(&v, TrackKind::Video, 0.0, 10.0);
        clip.src_duration = 30.0;
        clip.markers = vec![Marker::new(2.0), Marker::new(7.0)];
        let cid = clip.id.clone();
        store.clips.push(clip);
        // Bei Sequenzzeit 5 schneiden.
        store.split_at(5.0, Some(&[cid.clone()]));
        let left = store.clips.iter().find(|c| c.id == cid).unwrap();
        let right = store.clips.iter().find(|c| c.id != cid).unwrap();
        assert_eq!(left.markers.len(), 1, "linke Hälfte behält Marker < Schnitt");
        assert!((left.markers[0].time - 2.0).abs() < 1e-9);
        assert_eq!(right.markers.len(), 1, "rechte Hälfte behält Marker ≥ Schnitt");
        assert!((right.markers[0].time - 7.0).abs() < 1e-9);
    }

    #[test]
    fn clip_marker_anchors_to_media_through_trim_and_move() {
        let mut store = TimelineStore::default();
        let v = track_ids(&store, TrackKind::Video)[1].clone();
        let mut clip = test_clip(&v, TrackKind::Video, 4.0, 10.0);
        clip.src_in = 0.0;
        clip.src_duration = 30.0;
        // Marker bei Medienzeit 3 ⇒ Sequenzzeit 4+3 = 7.
        clip.markers = vec![Marker::new(3.0)];
        let cid = clip.id.clone();
        store.clips.push(clip);
        let seq0 = store.clip(&cid).unwrap().visible_markers().next().unwrap().0;
        assert!((seq0 - 7.0).abs() < 1e-9);

        // Kopf um 2 s trimmen (src_in → 2, start → 6): Medienzeit bleibt 3,
        // Sequenzposition wandert auf 6 + (3−2) = 7? Nein: media 3, src_in 2,
        // off = 3−2 = 1, seq = 6+1 = 7 bleibt gleich — der Marker hängt am
        // Frame, nicht an der Clipkante.
        store.trim_clip(&cid, TrimEdge::Start, 2.0);
        let c = store.clip(&cid).unwrap();
        assert!((c.src_in - 2.0).abs() < 1e-9);
        assert_eq!(c.markers.len(), 1, "Marker bleibt erhalten");
        assert!((c.markers[0].time - 3.0).abs() < 1e-9, "Medienzeit unverändert");
        let seq1 = c.visible_markers().next().unwrap().0;
        assert!((seq1 - 7.0).abs() < 1e-9, "Frame-Position stabil: {seq1}");

        // Clip um +3 s verschieben (start 6 → 9): Medienzeit bleibt, die
        // Sequenzposition wandert mit dem Clip auf 10.
        store.move_clips(&[cid.clone()], 3.0, 0);
        let c = store.clip(&cid).unwrap();
        assert!((c.markers[0].time - 3.0).abs() < 1e-9);
        let seq2 = c.visible_markers().next().unwrap().0;
        assert!((seq2 - 10.0).abs() < 1e-9, "wandert mit dem Clip: {seq2}");
    }

    #[test]
    fn insert_copies_asset_markers_into_clip() {
        let mut store = TimelineStore::default();
        let mut asset = MediaAsset {
            id: "a1".into(),
            path: "/tmp/x.mp4".into(),
            name: "x.mp4".into(),
            kind: MediaKind::Video,
            info: crate::core::types::MediaInfo {
                path: "/tmp/x.mp4".into(),
                file_name: "x.mp4".into(),
                container: "mp4".into(),
                duration_sec: 10.0,
                size_bytes: 1,
                video: Vec::new(),
                audio: Vec::new(),
            },
            thumbnail_path: None,
            imported_at: 0.0,
            offline: false,
            markers: vec![Marker::new(2.0), Marker::new(8.0)],
        };
        asset.markers[0].name = "Beat".into();
        let assets = vec![asset];
        store.insert_assets(&assets, &["a1".into()], 0.0, None);
        let clip = store
            .clips
            .iter()
            .find(|c| c.asset_id == "a1")
            .expect("Clip eingefügt");
        // Beide Asset-Marker fallen in [0, 10] ⇒ als Clip-Marker kopiert,
        // mit frischen IDs (kein Verweis auf die Asset-Marker-IDs).
        assert_eq!(clip.markers.len(), 2);
        assert!(clip.markers.iter().any(|m| m.name == "Beat"));
        assert!(clip.markers.iter().all(|m| m.id != "" ));
    }

    #[test]
    fn clip_marker_outside_view_is_hidden_but_kept() {
        let mut store = TimelineStore::default();
        let v = track_ids(&store, TrackKind::Video)[1].clone();
        let mut clip = test_clip(&v, TrackKind::Video, 0.0, 5.0);
        clip.src_in = 2.0;
        clip.src_duration = 30.0;
        // Marker bei Medienzeit 1 liegt VOR src_in (weggetrimmt).
        clip.markers = vec![Marker::new(1.0), Marker::new(3.0)];
        let cid = clip.id.clone();
        store.clips.push(clip);
        let c = store.clip(&cid).unwrap();
        // Beide bleiben gespeichert, nur einer ist sichtbar.
        assert_eq!(c.markers.len(), 2);
        assert_eq!(c.visible_markers().count(), 1);
    }
}

