//! Timeline-Austauschformate (Interop): OpenTimelineIO, CMX-3600-EDL und
//! Final Cut Pro XML.
//!
//! Profi-Schnittprogramme tauschen Schnitte über diese Formate aus — typisch
//! reicht ein Editor seine Schnittfassung an DaVinci Resolve (Grading) weiter
//! und bekommt sie zurück. Interop ist **binär**: entweder der Schnitt kommt
//! frame-genau drüben an oder das Feature ist wertlos. Darum:
//!
//! * Alle Zeiten laufen über die **rationale Sequenzrate** ([`FrameRate`],
//!   z. B. 24000/1001) und werden als ganzzahlige Frame-Zahlen geführt —
//!   driftfrei auch an krummen NTSC-Raten und damit Editron→Format→Editron
//!   **verlustfrei**.
//! * Es gibt eine gemeinsame, format-neutrale Zwischendarstellung
//!   ([`InteropTimeline`]). Der Export baut sie aus der aktiven Sequenz; jeder
//!   Serializer (OTIO/EDL/FCPXML) verbraucht dieselbe IR — die Frame-Mathematik
//!   existiert genau einmal. Der Import dreht das um: ein Parser liefert die IR,
//!   [`build_import`] baut daraus Spuren/Clips/Assets.
//! * **Keine stillen Datenverluste.** Was Editron in ein Format nicht abbilden
//!   kann (fremde Effekte, Generatoren, Geschwindigkeit, höhere Spuren im EDL),
//!   wird übersprungen UND als Warnung gesammelt, die der Nutzer zu sehen
//!   bekommt — nie kommentarlos weggelassen.

pub mod edl;
pub mod fcpxml;
pub mod otio;

#[cfg(test)]
mod roundtrip;

use crate::core::marker::{Marker, MarkerColor};
use crate::core::sequence::{FrameRate, SequenceSettings, MAX_DIMENSION, MIN_DIMENSION};
use crate::core::timeline::{
    new_track, TimelineClip, TimelineStore, TimelineTrack, TrackKind, MIN_CLIP_DURATION,
};
use crate::core::transitions::{
    alignment_split_frames, Transition, TransitionAlignment, TransitionKind,
};
use crate::core::types::{
    new_id, AudioStreamInfo, MediaAsset, MediaInfo, MediaKind, VideoStreamInfo,
};
use std::path::Path;

/// Die unterstützten Austauschformate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InteropFormat {
    /// OpenTimelineIO (JSON) — das primäre, verlustärmste Format.
    Otio,
    /// CMX-3600-EDL (Text) — eine Video-Spur, Quell-/Record-Timecode.
    Edl,
    /// Final Cut Pro XML 1.11 — Resolve & viele Tools importieren es zuverlässig.
    Fcpxml,
}

impl InteropFormat {
    pub const ALL: [InteropFormat; 3] =
        [InteropFormat::Otio, InteropFormat::Edl, InteropFormat::Fcpxml];

    /// Dateiendung (ohne Punkt).
    pub fn extension(self) -> &'static str {
        match self {
            InteropFormat::Otio => "otio",
            InteropFormat::Edl => "edl",
            InteropFormat::Fcpxml => "fcpxml",
        }
    }

    /// Deutscher Anzeigename inkl. Endung (Menü, Dialog).
    pub fn label(self) -> &'static str {
        match self {
            InteropFormat::Otio => "OpenTimelineIO (.otio)",
            InteropFormat::Edl => "CMX-3600-EDL (.edl)",
            InteropFormat::Fcpxml => "Final Cut Pro XML (.fcpxml)",
        }
    }

    /// Stabiler Schlüssel für Command-IDs/Argumente.
    pub fn key(self) -> &'static str {
        match self {
            InteropFormat::Otio => "otio",
            InteropFormat::Edl => "edl",
            InteropFormat::Fcpxml => "fcpxml",
        }
    }

    pub fn from_key(key: &str) -> Option<InteropFormat> {
        InteropFormat::ALL.into_iter().find(|f| f.key() == key)
    }

    /// Kann dieses Format importiert werden? (FCPXML ist vorerst Export-only.)
    pub fn can_import(self) -> bool {
        matches!(self, InteropFormat::Otio | InteropFormat::Edl)
    }
}

// =====================================================================
//  Format-neutrale Zwischendarstellung (IR)
// =====================================================================

/// Eine Mediendatei-Referenz (dedupliziert: ein Eintrag je Quelle).
#[derive(Clone, Debug)]
pub struct InteropMedia {
    /// Anzeigename (i. d. R. Dateiname ohne '(Audio)'-Suffix).
    pub name: String,
    /// Absoluter Pfad. Leer, wenn die Quelle nur über den Namen bekannt ist.
    pub path: String,
    /// Reel-/Tape-Name für EDL (max. 8 Zeichen, A–Z/0–9) — aus dem Dateinamen.
    pub reel: String,
    /// Eigene Bildrate der Quelle (für FCPXML-Asset/OTIO available_range).
    pub rate: Option<FrameRate>,
    /// Gesamtlänge der Quelle in eigenen Frames (available_range), falls bekannt.
    pub total_frames: Option<i64>,
    pub has_video: bool,
    pub has_audio: bool,
}

/// Ein Übergang an einer Schnittkante (zwischen zwei Clips derselben Spur).
#[derive(Clone, Copy, Debug)]
pub struct InteropTransition {
    pub kind: TransitionKind,
    /// Gesamtdauer in Sequenz-Frames.
    pub frames: i64,
    /// Frames vor der Schnittkante (OTIO in_offset).
    pub pre: i64,
    /// Frames nach der Schnittkante (OTIO out_offset).
    pub post: i64,
}

/// Ein Element einer Spur in Abspielreihenfolge.
#[derive(Clone, Debug)]
pub enum InteropItem {
    /// Lücke (Gap) der angegebenen Frame-Länge.
    Gap { frames: i64 },
    /// Ein platzierter Clip.
    Clip(InteropClip),
    /// Ein Übergang zwischen dem vorigen und dem nächsten Clip.
    Transition(InteropTransition),
}

/// Ein platzierter Clip in Sequenz-Frame-Zeit (alles an der Sequenzrate).
#[derive(Clone, Debug)]
pub struct InteropClip {
    pub name: String,
    /// Index in [`InteropTimeline::media`].
    pub media: usize,
    /// Record-Start in Sequenz-Frames (absolut, ab Sequenzanfang).
    pub rec_start: i64,
    /// Quell-In in Frames bei der **Sequenzrate** (verlustfrei, siehe Modul-Doku).
    pub src_start: i64,
    /// Länge in Sequenz-Frames (Record == Source bei normaler Geschwindigkeit).
    pub frames: i64,
    pub enabled: bool,
}

impl InteropClip {
    pub fn rec_end(&self) -> i64 {
        self.rec_start + self.frames
    }
}

/// Eine Spur (Video oder Audio) als Folge von Elementen.
#[derive(Clone, Debug)]
pub struct InteropTrack {
    pub kind: TrackKind,
    /// Anzeigename (V1, A1 …).
    pub name: String,
    pub items: Vec<InteropItem>,
}

impl InteropTrack {
    /// Nur die Clips (ohne Gaps/Übergänge).
    pub fn clips(&self) -> impl Iterator<Item = &InteropClip> {
        self.items.iter().filter_map(|i| match i {
            InteropItem::Clip(c) => Some(c),
            _ => None,
        })
    }
}

/// Ein Sequenz-Marker in Sequenz-Frames.
#[derive(Clone, Debug)]
pub struct InteropMarker {
    pub frame: i64,
    pub duration: i64,
    pub name: String,
    pub note: String,
    pub color: MarkerColor,
}

/// Die vollständige, format-neutrale Timeline.
#[derive(Clone, Debug)]
pub struct InteropTimeline {
    pub name: String,
    pub rate: FrameRate,
    pub drop_frame: bool,
    pub width: u32,
    pub height: u32,
    /// Record-Start der Sequenz in Frames (Editron: 0; viele Tools: 1 h).
    pub global_start: i64,
    pub media: Vec<InteropMedia>,
    /// Video-Spuren von V1 (unten/Basis) bis Vn (oben).
    pub video_tracks: Vec<InteropTrack>,
    /// Audio-Spuren von A1 bis An.
    pub audio_tracks: Vec<InteropTrack>,
    pub markers: Vec<InteropMarker>,
}

// =====================================================================
//  Export: aktive Sequenz -> IR
// =====================================================================

/// Baut die IR aus einer Sequenz auf und sammelt dabei alle Auslassungen
/// (fremd-unabbildbare Dinge), die der Aufrufer dem Nutzer melden MUSS.
pub fn build_export(
    timeline: &TimelineStore,
    assets: &[MediaAsset],
    seq_name: &str,
) -> (InteropTimeline, Vec<String>) {
    let rate = timeline.settings.rate;
    let mut warnings: Vec<String> = Vec::new();

    // Mediendatei-Tabelle (dedupliziert nach asset_id).
    let mut media: Vec<InteropMedia> = Vec::new();
    let mut media_index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut media_of = |asset_id: &str, fallback_name: &str| -> Option<usize> {
        if let Some(&i) = media_index.get(asset_id) {
            return Some(i);
        }
        let asset = assets.iter().find(|a| a.id == asset_id)?;
        let mrate = asset
            .info
            .video
            .first()
            .and_then(|v| FrameRate::from_fps(v.fps));
        let total_frames = mrate.map(|r| r.frame_round(asset.info.duration_sec).max(0));
        let entry = InteropMedia {
            name: if asset.name.trim().is_empty() {
                fallback_name.to_string()
            } else {
                asset.name.clone()
            },
            path: asset.path.clone(),
            reel: reel_from_name(&asset.info.file_name),
            rate: mrate,
            total_frames,
            has_video: !asset.info.video.is_empty() || asset.kind == MediaKind::Image,
            has_audio: !asset.info.audio.is_empty(),
        };
        let idx = media.len();
        media.push(entry);
        media_index.insert(asset_id.to_string(), idx);
        Some(idx)
    };

    // Spuren nach Art aufteilen (Untertitel-Spuren werden gemeldet).
    let video_src: Vec<&TimelineTrack> = timeline
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Video)
        .collect();
    let audio_src: Vec<&TimelineTrack> = timeline
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio)
        .collect();
    let subtitle_count = timeline
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Subtitle)
        .count();
    if subtitle_count > 0 {
        warnings.push(format!(
            "{subtitle_count} Untertitel-Spur(en) werden nicht übertragen (eigener SRT-Export)."
        ));
    }

    // Video-Spuren von V1 (unten = Speicherende) bis Vn (oben).
    let mut video_tracks: Vec<InteropTrack> = Vec::new();
    for (rev_idx, track) in video_src.iter().rev().enumerate() {
        let name = format!("V{}", rev_idx + 1);
        video_tracks.push(build_track(
            timeline,
            track,
            &name,
            rate,
            &mut media_of,
            &mut warnings,
        ));
    }
    // Audio-Spuren A1.. in Speicherreihenfolge.
    let mut audio_tracks: Vec<InteropTrack> = Vec::new();
    for (idx, track) in audio_src.iter().enumerate() {
        let name = format!("A{}", idx + 1);
        audio_tracks.push(build_track(
            timeline,
            track,
            &name,
            rate,
            &mut media_of,
            &mut warnings,
        ));
    }

    let markers = timeline
        .markers
        .iter()
        .map(|m| InteropMarker {
            frame: rate.frame_round(m.time).max(0),
            duration: rate.frame_round(m.duration).max(0),
            name: m.name.clone(),
            note: m.note.clone(),
            color: m.color,
        })
        .collect();

    let ir = InteropTimeline {
        name: seq_name.to_string(),
        rate,
        drop_frame: timeline.settings.drop_frame && rate.supports_drop_frame(),
        width: timeline.settings.width,
        height: timeline.settings.height,
        global_start: 0,
        media,
        video_tracks,
        audio_tracks,
        markers,
    };
    (ir, warnings)
}

/// Eine Editron-Spur in eine IR-Spur übersetzen: Clips sortieren, Lücken als
/// Gaps einfügen, Übergänge als Übergangs-Elemente an die Kanten setzen.
fn build_track(
    timeline: &TimelineStore,
    track: &TimelineTrack,
    name: &str,
    rate: FrameRate,
    media_of: &mut impl FnMut(&str, &str) -> Option<usize>,
    warnings: &mut Vec<String>,
) -> InteropTrack {
    // Clips der Spur in Record-Reihenfolge.
    let mut clips: Vec<&TimelineClip> = timeline
        .clips
        .iter()
        .filter(|c| c.track_id == track.id)
        .collect();
    clips.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));

    let mut items: Vec<InteropItem> = Vec::new();
    let mut cursor = 0i64; // Record-Frame
    let mut prev_id: Option<&str> = None;

    for clip in clips {
        let rec_start = rate.frame_round(clip.start).max(0);
        let frames = rate.frame_round(clip.duration).max(1);

        // Lücke vor dem Clip.
        if rec_start > cursor {
            items.push(InteropItem::Gap { frames: rec_start - cursor });
        }

        // Übergang an der Eingangskante (zweiseitig: from == prev, to == clip).
        if let Some(tr) = two_sided_transition_into(timeline, prev_id, &clip.id) {
            items.push(InteropItem::Transition(transition_ir(&tr, rate, warnings)));
        }

        // Nicht abbildbare Clips als gleichlange Lücke erhalten (Timing bleibt
        // frame-genau) und melden.
        if let Some(reason) = unsupported_clip_reason(clip) {
            warnings.push(format!(
                "Clip '{}' ({name}) wird als Lücke exportiert: {reason}.",
                display_name(clip)
            ));
            items.push(InteropItem::Gap { frames });
            cursor = rec_start + frames;
            prev_id = Some(&clip.id);
            continue;
        }

        let Some(media_idx) = media_of(&clip.asset_id, &clip.name) else {
            warnings.push(format!(
                "Clip '{}' ({name}) ohne auffindbares Medium — als Lücke exportiert.",
                display_name(clip)
            ));
            items.push(InteropItem::Gap { frames });
            cursor = rec_start + frames;
            prev_id = Some(&clip.id);
            continue;
        };

        if has_unexportable_attrs(clip) {
            warnings.push(format!(
                "Effekte/Farbkorrektur an '{}' ({name}) werden nicht übertragen — nur der Schnitt.",
                display_name(clip)
            ));
        }
        if clip.reverse || clip.freeze || (clip.speed - 1.0).abs() > 1e-6 {
            warnings.push(format!(
                "Geschwindigkeit/Standbild an '{}' ({name}) wird nicht übertragen — Clip läuft normal.",
                display_name(clip)
            ));
        }

        items.push(InteropItem::Clip(InteropClip {
            name: display_name(clip).to_string(),
            media: media_idx,
            rec_start,
            src_start: rate.frame_round(clip.src_in).max(0),
            frames,
            enabled: clip.enabled,
        }));
        cursor = rec_start + frames;
        prev_id = Some(&clip.id);
    }

    InteropTrack {
        kind: track.kind,
        name: name.to_string(),
        items,
    }
}

/// Den zweiseitigen Übergang an der Kante prev→clip finden (falls vorhanden).
fn two_sided_transition_into(
    timeline: &TimelineStore,
    prev_id: Option<&str>,
    clip_id: &str,
) -> Option<Transition> {
    let prev = prev_id?;
    timeline
        .transitions
        .iter()
        .find(|t| {
            t.from_clip_id.as_deref() == Some(prev) && t.to_clip_id.as_deref() == Some(clip_id)
        })
        .cloned()
}

fn transition_ir(tr: &Transition, rate: FrameRate, warnings: &mut Vec<String>) -> InteropTransition {
    let frames = rate.frame_round(tr.duration).max(1);
    let (pre, post) = alignment_split_frames(tr.alignment, frames);
    if tr.kind != TransitionKind::CrossDissolve && !tr.kind.is_audio() {
        warnings.push(format!(
            "Übergang '{}' wird als Überblendung exportiert (Zielformate kennen nur Dissolves).",
            tr.kind.label()
        ));
    }
    InteropTransition {
        kind: tr.kind,
        frames,
        pre,
        post,
    }
}

/// Warum ein Clip nicht als Medien-Clip exportierbar ist (None = exportierbar).
fn unsupported_clip_reason(clip: &TimelineClip) -> Option<&'static str> {
    if clip.is_title() {
        Some("Titel-Generator")
    } else if clip.is_subtitle() {
        Some("Untertitel-Segment")
    } else if clip.is_nest() {
        Some("verschachtelte Sequenz")
    } else if clip.asset_id.trim().is_empty() {
        Some("ohne Medium")
    } else {
        None
    }
}

fn has_unexportable_attrs(clip: &TimelineClip) -> bool {
    !clip.effects.is_empty()
        || !crate::core::grade::ColorGrade::is_default(&clip.grade)
        || !crate::core::animation::ClipFx::is_default(&clip.fx)
}

fn display_name(clip: &TimelineClip) -> &str {
    if clip.name.trim().is_empty() {
        "Clip"
    } else {
        &clip.name
    }
}

// =====================================================================
//  Import: IR -> Spuren/Clips/Assets
// =====================================================================

/// Ergebnis des IR-Aufbaus für den Import — vom Aufrufer in den App-Zustand
/// übernommen (Assets anlegen, Sequenz einsetzen, Relink anstoßen).
pub struct ImportBuild {
    pub sequence_name: String,
    pub settings: SequenceSettings,
    pub tracks: Vec<TimelineTrack>,
    pub clips: Vec<TimelineClip>,
    pub transitions: Vec<Transition>,
    pub markers: Vec<Marker>,
    /// Neu anzulegende Assets (online wie offline).
    pub new_assets: Vec<MediaAsset>,
    /// (asset_id, pfad) für Assets, deren Datei existiert und nachträglich
    /// per ffprobe verifiziert werden soll.
    pub probe: Vec<(String, String)>,
    pub summary: InteropImportSummary,
}

/// Ergebnis-Bericht eines Import-/Export-Vorgangs für den Ergebnis-Dialog.
/// Trägt eine Titelzeile, Kennzahlen-Paare, ALLE Auslassungs-Warnungen und die
/// Zahl fehlender Medien (>0 ⇒ der Dialog bietet „Medien verknüpfen" an).
#[derive(Clone, Debug)]
pub struct InteropReport {
    pub title: String,
    pub summary: Vec<(String, String)>,
    pub warnings: Vec<String>,
    pub offline: usize,
    pub is_import: bool,
}

impl InteropReport {
    pub fn from_import(s: &InteropImportSummary) -> Self {
        let media = if s.media_offline > 0 {
            format!("{} ({} offline)", s.media_total, s.media_offline)
        } else {
            format!("{}", s.media_total)
        };
        InteropReport {
            title: format!("{} importiert", s.format.extension().to_uppercase()),
            summary: vec![
                ("Format".into(), s.format.label().to_string()),
                ("Sequenz".into(), s.sequence_name.clone()),
                (
                    "Spuren".into(),
                    format!("{} Video · {} Audio", s.video_tracks, s.audio_tracks),
                ),
                ("Clips".into(), format!("{}", s.clips)),
                ("Medien".into(), media),
            ],
            warnings: s.warnings.clone(),
            offline: s.media_offline,
            is_import: true,
        }
    }

    pub fn from_export(format: InteropFormat, path: &str, warnings: Vec<String>) -> Self {
        InteropReport {
            title: format!("{} exportiert", format.extension().to_uppercase()),
            summary: vec![
                ("Format".into(), format.label().to_string()),
                ("Datei".into(), path.to_string()),
            ],
            warnings,
            offline: 0,
            is_import: false,
        }
    }
}

/// Was der Import ergeben hat — für den Ergebnis-Dialog.
pub struct InteropImportSummary {
    pub format: InteropFormat,
    pub sequence_name: String,
    pub video_tracks: usize,
    pub audio_tracks: usize,
    pub clips: usize,
    pub media_total: usize,
    pub media_offline: usize,
    /// Auslassungen/Hinweise (Parser- und Aufbau-Warnungen zusammengeführt).
    pub warnings: Vec<String>,
}

/// Baut aus der geparsten IR Spuren, Clips und Assets. `existing` sind die
/// bereits vorhandenen Assets (Pfad/Name-Match zur Wiederverwendung).
pub fn build_import(
    ir: &InteropTimeline,
    existing: &[MediaAsset],
    format: InteropFormat,
    parse_warnings: Vec<String>,
) -> ImportBuild {
    let rate = ir.rate;
    let warnings = parse_warnings;

    // Mediendatei → asset_id auflösen: bestehendes Asset wiederverwenden, sonst
    // neues (offline, falls Datei fehlt) anlegen.
    let mut media_asset_id: Vec<String> = Vec::with_capacity(ir.media.len());
    let mut new_assets: Vec<MediaAsset> = Vec::new();
    let mut probe: Vec<(String, String)> = Vec::new();
    let mut offline = 0usize;
    let now = unix_now();
    for m in &ir.media {
        if let Some(found) = find_existing_asset(existing, m) {
            media_asset_id.push(found);
            continue;
        }
        let exists = !m.path.is_empty() && Path::new(&m.path).exists();
        let asset = make_import_asset(m, rate, exists, now);
        media_asset_id.push(asset.id.clone());
        if exists {
            probe.push((asset.id.clone(), m.path.clone()));
        } else {
            offline += 1;
        }
        new_assets.push(asset);
    }

    // Spuren in Editron-Speicherreihenfolge aufbauen: erst Video von OBEN nach
    // unten (Vn..V1 → V1 ist die letzte gespeicherte), dann Audio A1..An.
    let mut tracks: Vec<TimelineTrack> = Vec::new();
    let mut clips: Vec<TimelineClip> = Vec::new();
    let mut transitions: Vec<Transition> = Vec::new();

    for vt in ir.video_tracks.iter().rev() {
        let track = new_track(TrackKind::Video);
        emit_track_clips(
            vt,
            &track,
            rate,
            &media_asset_id,
            &mut clips,
            &mut transitions,
        );
        tracks.push(track);
    }
    for at in &ir.audio_tracks {
        let track = new_track(TrackKind::Audio);
        emit_track_clips(
            at,
            &track,
            rate,
            &media_asset_id,
            &mut clips,
            &mut transitions,
        );
        tracks.push(track);
    }

    link_av_pairs(&mut clips);

    let markers = ir
        .markers
        .iter()
        .map(|m| Marker {
            id: new_id(),
            time: rate.time_of_frame(m.frame as f64).max(0.0),
            duration: rate.time_of_frame(m.duration as f64).max(0.0),
            name: m.name.clone(),
            note: m.note.clone(),
            color: m.color,
        })
        .collect();

    let settings = SequenceSettings {
        rate,
        width: ir.width.clamp(MIN_DIMENSION, MAX_DIMENSION),
        height: ir.height.clamp(MIN_DIMENSION, MAX_DIMENSION),
        drop_frame: ir.drop_frame && rate.supports_drop_frame(),
    }
    .sanitized();

    let media_total = ir.media.len();
    let clip_count = clips.iter().filter(|c| c.kind == TrackKind::Video || c.kind == TrackKind::Audio).count();
    let summary = InteropImportSummary {
        format,
        sequence_name: ir.name.clone(),
        video_tracks: ir.video_tracks.len(),
        audio_tracks: ir.audio_tracks.len(),
        clips: clip_count,
        media_total,
        media_offline: offline,
        warnings,
    };

    ImportBuild {
        sequence_name: if ir.name.trim().is_empty() {
            format!("Import ({})", format.extension().to_uppercase())
        } else {
            ir.name.clone()
        },
        settings,
        tracks,
        clips,
        transitions,
        markers,
        new_assets,
        probe,
        summary,
    }
}

/// Clips + Übergänge einer IR-Spur in konkrete Timeline-Objekte gießen.
fn emit_track_clips(
    ir_track: &InteropTrack,
    track: &TimelineTrack,
    rate: FrameRate,
    media_asset_id: &[String],
    clips: &mut Vec<TimelineClip>,
    transitions: &mut Vec<Transition>,
) {
    let mut last_clip_id: Option<String> = None;
    let mut pending_transition: Option<InteropTransition> = None;

    for item in &ir_track.items {
        match item {
            InteropItem::Gap { .. } => {
                // Gaps sind in Editron implizit (Clips tragen ihren rec_start) —
                // ein offener Übergang verfällt an einer Lücke.
                pending_transition = None;
            }
            InteropItem::Transition(tr) => {
                pending_transition = Some(*tr);
            }
            InteropItem::Clip(c) => {
                let asset_id = media_asset_id.get(c.media).cloned().unwrap_or_default();
                let clip = TimelineClip {
                    extra: Default::default(),
                    id: new_id(),
                    track_id: track.id.clone(),
                    asset_id,
                    name: c.name.clone(),
                    kind: track.kind,
                    start: rate.time_of_frame(c.rec_start as f64).max(0.0),
                    duration: rate.time_of_frame(c.frames as f64).max(MIN_CLIP_DURATION),
                    src_in: rate.time_of_frame(c.src_start as f64).max(0.0),
                    src_duration: f64::INFINITY, // wird beim Relink/Probe gesetzt
                    link_id: None,
                    enabled: c.enabled,
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
                };
                let clip_id = clip.id.clone();
                if let (Some(tr), Some(prev)) = (pending_transition.take(), last_clip_id.clone()) {
                    let mut t = Transition::new(
                        tr.kind,
                        Some(prev),
                        Some(clip_id.clone()),
                        rate.time_of_frame(tr.frames as f64).max(MIN_CLIP_DURATION),
                    );
                    t.alignment = alignment_from_offsets(tr.pre, tr.post);
                    transitions.push(t);
                }
                clips.push(clip);
                last_clip_id = Some(clip_id);
            }
        }
    }
}

/// Verknüpfung von Video- und Audio-Clips desselben Assets an gleicher
/// Record-Position (heuristisch — A/V-Paare bleiben so gemeinsam editierbar).
fn link_av_pairs(clips: &mut [TimelineClip]) {
    use std::collections::HashMap;
    // Schlüssel: (asset_id, rec_start gerundet auf µs).
    let mut video_at: HashMap<(String, i64), usize> = HashMap::new();
    for (i, c) in clips.iter().enumerate() {
        if c.kind == TrackKind::Video && !c.asset_id.is_empty() {
            video_at.insert((c.asset_id.clone(), (c.start * 1000.0).round() as i64), i);
        }
    }
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    for (j, c) in clips.iter().enumerate() {
        if c.kind == TrackKind::Audio && !c.asset_id.is_empty() {
            if let Some(&i) = video_at.get(&(c.asset_id.clone(), (c.start * 1000.0).round() as i64)) {
                pairs.push((i, j));
            }
        }
    }
    for (i, j) in pairs {
        let link = new_id();
        clips[i].link_id = Some(link.clone());
        clips[j].link_id = Some(link);
    }
}

/// Vorhandenes Asset per absolutem Pfad (exakt) oder Dateiname (case-insensitiv)
/// finden — vermeidet Asset-Dubletten beim Import.
fn find_existing_asset(existing: &[MediaAsset], m: &InteropMedia) -> Option<String> {
    if !m.path.is_empty() {
        if let Some(a) = existing.iter().find(|a| a.path == m.path) {
            return Some(a.id.clone());
        }
    }
    let file = Path::new(&m.path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| m.name.clone());
    existing
        .iter()
        .find(|a| a.info.file_name.eq_ignore_ascii_case(&file) || a.name.eq_ignore_ascii_case(&file))
        .map(|a| a.id.clone())
}

/// Ein (i. d. R. offline) Asset aus einer IR-Medienreferenz bauen.
fn make_import_asset(m: &InteropMedia, seq_rate: FrameRate, exists: bool, now: f64) -> MediaAsset {
    let file_name = Path::new(&m.path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| m.name.clone());
    let mrate = m.rate.unwrap_or(seq_rate);
    let duration_sec = m
        .total_frames
        .map(|f| mrate.time_of_frame(f as f64))
        .unwrap_or(0.0);
    let kind = if m.has_video {
        MediaKind::Video
    } else if m.has_audio {
        MediaKind::Audio
    } else {
        MediaKind::Video
    };
    let video = if m.has_video {
        vec![VideoStreamInfo {
            index: 0,
            codec: String::new(),
            width: 0,
            height: 0,
            fps: mrate.fps(),
            pix_fmt: None,
            bitrate: None,
            bit_depth: 8,
            color_transfer: None,
            color_primaries: None,
            color_space: None,
            color_range: None,
        }]
    } else {
        Vec::new()
    };
    let audio = if m.has_audio {
        vec![AudioStreamInfo {
            index: if m.has_video { 1 } else { 0 },
            codec: String::new(),
            channels: 2,
            sample_rate: 48_000,
            bitrate: None,
        }]
    } else {
        Vec::new()
    };
    MediaAsset {
        extra: Default::default(),
        id: new_id(),
        path: m.path.clone(),
        name: if m.name.trim().is_empty() {
            file_name.clone()
        } else {
            m.name.clone()
        },
        kind,
        info: MediaInfo {
            path: m.path.clone(),
            file_name,
            container: String::new(),
            duration_sec,
            size_bytes: 0,
            video,
            audio,
            recorded_at: None,
        },
        thumbnail_path: None,
        imported_at: now,
        bin_id: crate::core::bin::ROOT_BIN_ID.to_string(),
        label: None,
        offline: !exists,
        markers: Vec::new(),
        proxy_path: None,
        proxy_src_mtime: None,
        proxy_offline: false,
    }
}

fn alignment_from_offsets(pre: i64, post: i64) -> TransitionAlignment {
    if pre <= 0 && post > 0 {
        TransitionAlignment::StartAtCut
    } else if post <= 0 && pre > 0 {
        TransitionAlignment::EndAtCut
    } else {
        TransitionAlignment::Center
    }
}

// =====================================================================
//  Top-Level-Dispatcher (von der UI genutzt)
// =====================================================================

/// Aktive Sequenz in das gewählte Format serialisieren. Liefert den Dateiinhalt
/// und ALLE Auslassungs-Warnungen (vom IR-Aufbau und vom jeweiligen Serializer).
pub fn export_text(
    format: InteropFormat,
    timeline: &TimelineStore,
    assets: &[MediaAsset],
    seq_name: &str,
) -> (String, Vec<String>) {
    let (ir, mut warnings) = build_export(timeline, assets, seq_name);
    let body = match format {
        InteropFormat::Otio => otio::export(&ir),
        InteropFormat::Edl => {
            let (t, w) = edl::export(&ir);
            warnings.extend(w);
            t
        }
        InteropFormat::Fcpxml => {
            let (t, w) = fcpxml::export(&ir);
            warnings.extend(w);
            t
        }
    };
    (body, warnings)
}

/// Dateiinhalt eines Formats parsen → IR + Parser-Warnungen.
pub fn parse_text(
    format: InteropFormat,
    text: &str,
) -> Result<(InteropTimeline, Vec<String>), String> {
    match format {
        InteropFormat::Otio => otio::parse(text),
        InteropFormat::Edl => edl::parse(text),
        InteropFormat::Fcpxml => {
            Err("FCPXML-Import wird (noch) nicht unterstützt — bitte OTIO oder EDL verwenden.".to_string())
        }
    }
}

/// Vollständiger Import: parsen + Spuren/Clips/Assets bauen.
pub fn import_text(
    format: InteropFormat,
    text: &str,
    existing: &[MediaAsset],
) -> Result<ImportBuild, String> {
    let (ir, warnings) = parse_text(format, text)?;
    Ok(build_import(&ir, existing, format, warnings))
}

// =====================================================================
//  Gemeinsame Helfer
// =====================================================================

fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Reel-/Tape-Name für EDL aus einem Dateinamen: Großbuchstaben/Ziffern, max.
/// 8 Zeichen, Rest entfernt. Leerer Rest ⇒ 'AX' (CMX-Aux-Reel).
pub fn reel_from_name(file_name: &str) -> String {
    let stem = Path::new(file_name)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| file_name.to_string());
    let cleaned: String = stem
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .take(8)
        .collect();
    if cleaned.is_empty() {
        "AX".to_string()
    } else {
        cleaned
    }
}

/// Absoluten Pfad in eine `file://`-URL mit Prozent-Kodierung wandeln.
pub fn path_to_file_url(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    // Pfad-Trenner bleiben erhalten; alles andere RFC-3986-kodieren.
    let mut out = String::from("file://");
    // Auf POSIX beginnt ein absoluter Pfad mit '/', die erste Komponente ist leer.
    for (i, seg) in path.split('/').enumerate() {
        if i > 0 {
            out.push('/');
        }
        out.push_str(&percent_encode(seg));
    }
    out
}

/// `file://`-URL (oder nackten Pfad) zurück in einen Dateipfad wandeln.
pub fn file_url_to_path(url: &str) -> String {
    let trimmed = url.trim();
    let body = if let Some(rest) = trimmed.strip_prefix("file://") {
        // Optionaler Host (file://host/pfad) — leeren Host (localhost) entfernen.
        match rest.find('/') {
            Some(0) => rest,
            Some(pos) => &rest[pos..],
            None => rest,
        }
    } else {
        trimmed
    };
    percent_decode(body)
}

fn percent_encode(seg: &str) -> String {
    let mut out = String::with_capacity(seg.len());
    for b in seg.bytes() {
        let keep = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~');
        if keep {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(hex_digit(b >> 4));
            out.push(hex_digit(b & 0x0f));
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_digit(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        _ => (b'A' + (n - 10)) as char,
    }
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_url_roundtrips_with_spaces_and_unicode() {
        let p = "/Volumes/Media/B-Roll/clip 01 (öä).mov";
        let url = path_to_file_url(p);
        assert!(url.starts_with("file:///Volumes/"));
        assert!(!url.contains(' '), "Leerzeichen müssen kodiert sein");
        assert_eq!(file_url_to_path(&url), p);
    }

    #[test]
    fn file_url_accepts_plain_path() {
        assert_eq!(file_url_to_path("/abs/path.mov"), "/abs/path.mov");
    }

    #[test]
    fn reel_name_is_uppercase_alnum_max_8() {
        assert_eq!(reel_from_name("my-clip_02.mov"), "MYCLIP02");
        assert_eq!(reel_from_name("A very long name.mov"), "AVERYLON");
        assert_eq!(reel_from_name("___.mov"), "AX");
    }
}
