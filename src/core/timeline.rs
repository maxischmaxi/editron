//! Sequenz-Modell + Store: Tracks/Clips mit verknüpften A/V-Paaren,
//! Snapshot-History (Undo/Redo) und allen Editier-Operationen.

use crate::core::animation::{AnimatedParam, ClipFx, Interp, Keyframe, ParamId, ParamRef};
use crate::core::compose::BlendMode;
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

// Die `impl TimelineStore`-Methoden sind thematisch in Submodule zerlegt (jedes
// hängt nur Methoden an den hier definierten Typ an — daher kein Re-Export).
// Reihenfolge spiegelt die ursprünglichen Datei-Abschnitte wider.
mod clipgen;
mod edit;
mod effects;
mod history;
mod multicam;
mod selection;
mod store;

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

/// Grenzen der manuell verstellten Spurhöhe (logische Pixel, Sash-Drag am
/// Spurkopf). Die Untergrenze lässt Label + Toggles der kompaktesten Spurart
/// (Untertitel) noch zu; die Obergrenze gibt Platz für Waveforms/Keyframes.
pub const MIN_TRACK_HEIGHT: f32 = 28.0;
pub const MAX_TRACK_HEIGHT: f32 = 320.0;

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
    /// Manuell verstellte Spurhöhe in logischen Pixeln (Sash-Drag am Spurkopf,
    /// auf [`MIN_TRACK_HEIGHT`]..[`MAX_TRACK_HEIGHT`] geklemmt). `None` ⇒ die
    /// kompakte Standardhöhe der Spurart (Formatversion 13).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<f32>,
    /// Felder einer NEUEREN Editron-Version, die dieser Build noch nicht kennt.
    /// Werden beim Speichern unverändert zurückgeschrieben (Vorwärtskompatibilität,
    /// siehe `core::project::ProjectFile::extra`).
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
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

/// Frische Spur einer Art mit Standardwerten (für Interop-Import u. Ä.).
pub fn new_track(kind: TrackKind) -> TimelineTrack {
    make_track(kind)
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
        height: None,
        extra: Default::default(),
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
    /// Verschachtelte Sequenz (Nesting): ist dies gesetzt, ist der Clip kein
    /// Mediendatei-Clip, sondern bildet eine andere Sequenz ab (`asset_id`
    /// leer). Die Quelle wird zur Wiedergabe/zum Export rekursiv aus der
    /// referenzierten Sequenz gerendert. Medienzeit-Achse = Sequenzzeit der
    /// inneren Sequenz; `src_in`/`src_duration` trimmen in sie hinein
    /// (Formatversion 11).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nest_seq: Option<String>,
    /// Multicam-Clip: verweist auf eine Multicam-Quelle (Sequenz mit
    /// `timeline.multicam`) und trägt den aktiven Winkel. `asset_id` leer;
    /// `src_in`/`duration` rechnen in gemeinsamer Multicam-Zeit. Der aktive
    /// Winkel wird zur Wiedergabe/zum Export zu einem normalen Medien-Blatt
    /// aufgelöst (Formatversion 12).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multicam: Option<crate::core::multicam::MulticamClip>,
    /// Ebenen-Mischmodus des Clips (Normal = klassisches Src-over). Wirkt auf
    /// das Compositing im Programmmonitor und Export; der GPU-Blend-Shader
    /// zieht formelgleich nach. Formatversion 17.
    #[serde(default)]
    pub blend_mode: BlendMode,
    /// Felder einer NEUEREN Editron-Version, die dieser Build noch nicht kennt.
    /// Werden beim Speichern unverändert zurückgeschrieben (Vorwärtskompatibilität,
    /// siehe `core::project::ProjectFile::extra`). `Clone` erhält sie, sodass
    /// Schnitt-Operationen (Teilen/Duplizieren/Ripple) sie automatisch mittragen.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
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

    /// Nest-Clip: bildet eine andere Sequenz ab (ohne eigene Mediendatei).
    /// Wird wie ein Medien-Clip dekodiert/komponiert (NICHT wie ein Generator),
    /// ist aber von der Verwaisten-Bereinigung ausgenommen (kein `asset_id`).
    pub fn is_nest(&self) -> bool {
        self.nest_seq.is_some()
    }

    /// Multicam-Clip: bildet den aktiven Winkel einer Multicam-Quelle ab
    /// (ohne eigenes `asset_id`). Wie ein Nest von der Verwaisten-Bereinigung
    /// ausgenommen; der aktive Winkel wird an den Render-Pfaden aufgelöst.
    pub fn is_multicam(&self) -> bool {
        self.multicam.is_some()
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

/// Einen einfachen Medien-Clip mit Standardwerten bauen (für die innere
/// Timeline einer Multicam-Quelle, Interop o. Ä.).
#[allow(clippy::too_many_arguments)]
pub fn new_media_clip(
    track_id: &str,
    asset_id: &str,
    name: impl Into<String>,
    kind: TrackKind,
    start: f64,
    duration: f64,
    src_in: f64,
    src_duration: f64,
) -> TimelineClip {
    TimelineClip {
        extra: Default::default(),
        id: new_id(),
        track_id: track_id.to_string(),
        asset_id: asset_id.to_string(),
        name: name.into(),
        kind,
        start,
        duration,
        src_in,
        src_duration,
        link_id: None,
        enabled: true,
        gain_db: 0.0,
        fx: Default::default(),
        grade: Default::default(),
        effects: Vec::new(),
        title: None,
        subtitle: None,
        speed: 1.0,
        reverse: false,
        freeze: false,
        markers: Vec::new(),
        nest_seq: None,
        multicam: None,
        blend_mode: BlendMode::default(),
    }
}

/// Einen Multicam-Clip bauen: verweist auf eine Multicam-Quelle (`source`,
/// eine Sequenz-ID) und trägt den aktiven `angle`. `asset_id` leer;
/// `src_in`/`duration` rechnen in gemeinsamer Multicam-Zeit.
#[allow(clippy::too_many_arguments)]
pub fn new_multicam_clip(
    track_id: &str,
    source: &str,
    angle: u32,
    name: impl Into<String>,
    kind: TrackKind,
    start: f64,
    duration: f64,
    src_in: f64,
    src_duration: f64,
) -> TimelineClip {
    let mut c = new_media_clip(track_id, "", name, kind, start, duration, src_in, src_duration);
    c.multicam = Some(crate::core::multicam::MulticamClip {
        source: source.to_string(),
        angle,
    });
    c
}

/// Minimaler, gültiger Test-Clip auf einer Spur (für modulübergreifende Tests).
#[cfg(test)]
pub fn test_clip(track_id: &str) -> TimelineClip {
    TimelineClip {
        id: new_id(),
        track_id: track_id.to_string(),
        asset_id: "asset".into(),
        name: "clip".into(),
        kind: TrackKind::Video,
        start: 0.0,
        duration: 2.0,
        src_in: 0.0,
        src_duration: 10.0,
        link_id: None,
        enabled: true,
        gain_db: 0.0,
        fx: Default::default(),
        grade: Default::default(),
        effects: Vec::new(),
        title: None,
        subtitle: None,
        speed: 1.0,
        reverse: false,
        freeze: false,
        markers: Vec::new(),
        nest_seq: None,
        multicam: None,
        extra: Default::default(),
        blend_mode: BlendMode::default(),
    }
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
pub(crate) struct Snapshot {
    tracks: Vec<TimelineTrack>,
    clips: Vec<TimelineClip>,
    transitions: Vec<Transition>,
    markers: Vec<Marker>,
    master_gain_db: f64,
    /// Globale Operationssequenz (siehe `core::next_op_seq`) — ordnet diesen
    /// Snapshot gegen die Medien-Undo-History für die `edit.undo`-Koordination.
    seq: u64,
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
    pub(crate) clipboard_transitions: Vec<Transition>,
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
    pub(crate) attr_clipboard: Option<ClipAttributes>,
    /// Multicam-Quelle: ist dies gesetzt, ist die Sequenz eine Multicam-Quelle
    /// (je Winkel ein Video-/Audio-Clip in der Timeline; die Winkel-Metadaten
    /// + Sync-Offsets stehen hier). Multicam-Clips anderer Sequenzen verweisen
    /// über `MulticamClip::source` auf die ID dieser Sequenz (Formatversion 12).
    pub multicam: Option<crate::core::multicam::MulticamSource>,
    pub(crate) past: Vec<Snapshot>,
    pub(crate) future: Vec<Snapshot>,
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
            multicam: None,
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
    // total_cmp statt partial_cmp().unwrap() — paniced sonst bei NaN-Clipzeiten
    // (korruptes Projekt); NaN sortiert konsistent ans Ende.
    points.sort_by(|a, b| a.total_cmp(b));
    points.dedup_by(|a, b| (*a - *b).abs() < EPS);
    points
}

#[cfg(test)]
mod tests;
