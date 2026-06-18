//! Mehrere Sequenzen pro Projekt + Tab-/Aktiv-Verwaltung.
//!
//! Ein Projekt hält eine Liste von [`Sequence`]n (je eine eigene
//! [`TimelineStore`] mit eigener Undo-History, Sequenz-Einstellungen, Playhead
//! und Zoom), von denen genau eine *aktiv* ist. Der [`SequenceStore`] derefe-
//! renziert transparent auf die aktive Sequenz, damit der gesamte Bestandscode
//! (`state.timeline.clips`, `state.timeline.set_playhead(..)`, …) unverändert
//! auf die jeweils aktive Sequenz wirkt.
//!
//! Verschachtelte Sequenzen (Nesting) referenzieren über
//! [`TimelineClip::nest_seq`](crate::core::timeline::TimelineClip) die ID einer
//! anderen Sequenz. Der Rekursionsschutz ([`SequenceStore::would_create_cycle`])
//! verhindert, dass eine Sequenz sich (auch transitiv) selbst enthält.

use crate::core::sequence::SequenceSettings;
use crate::core::timeline::TimelineStore;
use crate::core::types::new_id;
use std::collections::HashSet;
use std::ops::{Deref, DerefMut};

/// Eine benannte Sequenz: Identität + eigene Timeline.
pub struct Sequence {
    pub id: String,
    pub name: String,
    /// Bin (Ordner) im Medien-Browser, in dem die Sequenz liegt.
    pub bin_id: String,
    pub timeline: TimelineStore,
    /// Unbekannte Felder einer neueren Version auf Sequenz-Ebene (siehe
    /// [`crate::core::project::ProjectFile::extra`]). Verlustfrei beim Speichern
    /// zurückgeschrieben.
    pub extra: serde_json::Map<String, serde_json::Value>,
    /// Unbekannte Felder einer neueren Version innerhalb des Timeline-Objekts
    /// dieser Sequenz.
    pub timeline_extra: serde_json::Map<String, serde_json::Value>,
}

impl Sequence {
    pub fn new(name: impl Into<String>, bin_id: impl Into<String>, timeline: TimelineStore) -> Self {
        Sequence {
            id: new_id(),
            name: name.into(),
            bin_id: bin_id.into(),
            timeline,
            extra: serde_json::Map::new(),
            timeline_extra: serde_json::Map::new(),
        }
    }

    /// IDs der direkt in dieser Sequenz verschachtelten Sequenzen.
    pub fn nested_ids(&self) -> impl Iterator<Item = &str> {
        self.timeline
            .clips
            .iter()
            .filter_map(|c| c.nest_seq.as_deref())
    }
}

/// Liste aller Sequenzen eines Projekts + aktive Auswahl + offene Tabs.
pub struct SequenceStore {
    /// Mindestens ein Eintrag — ein Projekt hat immer eine Sequenz.
    sequences: Vec<Sequence>,
    /// Index der aktiven Sequenz in `sequences` (stets gültig).
    active: usize,
    /// Offene Tabs (Sequenz-IDs in Anzeigereihenfolge). Die aktive Sequenz ist
    /// stets offen; nicht Teil einer Undo-History (reiner Ansichts-Zustand).
    open_tabs: Vec<String>,
}

impl Default for SequenceStore {
    fn default() -> Self {
        let seq = Sequence::new(
            "Sequenz 01",
            crate::core::bin::ROOT_BIN_ID,
            TimelineStore::default(),
        );
        let tab = seq.id.clone();
        SequenceStore {
            sequences: vec![seq],
            active: 0,
            open_tabs: vec![tab],
        }
    }
}

impl Deref for SequenceStore {
    type Target = TimelineStore;
    fn deref(&self) -> &TimelineStore {
        &self.sequences[self.active].timeline
    }
}

impl crate::core::compose::NestResolver for SequenceStore {
    fn nested_timeline(&self, seq_id: &str) -> Option<&TimelineStore> {
        self.timeline_of(seq_id)
    }
}

impl DerefMut for SequenceStore {
    fn deref_mut(&mut self) -> &mut TimelineStore {
        &mut self.sequences[self.active].timeline
    }
}

impl SequenceStore {
    /// Aus einer geladenen Sequenzliste aufbauen (Projektladen). Leere Listen
    /// fallen auf eine Default-Sequenz zurück; der aktive Index wird geklemmt.
    pub fn from_sequences(sequences: Vec<Sequence>, active_id: Option<&str>) -> Self {
        if sequences.is_empty() {
            return SequenceStore::default();
        }
        let active = active_id
            .and_then(|id| sequences.iter().position(|s| s.id == id))
            .unwrap_or(0);
        // Offene Tabs: die aktive Sequenz mindestens; weitere bleiben beim
        // Laden geschlossen (Premiere öffnet nur die zuletzt aktive).
        let open_tabs = vec![sequences[active].id.clone()];
        SequenceStore {
            sequences,
            active,
            open_tabs,
        }
    }

    // -------------------------------------------------------------- Abfragen

    pub fn all(&self) -> &[Sequence] {
        &self.sequences
    }

    pub fn iter(&self) -> impl Iterator<Item = &Sequence> {
        self.sequences.iter()
    }

    /// Veränderbarer Durchlauf über ALLE Sequenzen — z. B. für die
    /// Konsolidierung, die Medienpfade/`src_in` projektweit anpasst.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Sequence> {
        self.sequences.iter_mut()
    }

    pub fn len(&self) -> usize {
        self.sequences.len()
    }

    pub fn is_empty(&self) -> bool {
        // Ein Projekt hat immer mindestens eine Sequenz.
        self.sequences.is_empty()
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn active_id(&self) -> &str {
        &self.sequences[self.active].id
    }

    pub fn active_name(&self) -> &str {
        &self.sequences[self.active].name
    }

    pub fn active_sequence(&self) -> &Sequence {
        &self.sequences[self.active]
    }

    pub fn get(&self, id: &str) -> Option<&Sequence> {
        self.sequences.iter().find(|s| s.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Sequence> {
        self.sequences.iter_mut().find(|s| s.id == id)
    }

    /// Timeline einer (auch nicht-aktiven) Sequenz — für die Nest-Auflösung.
    pub fn timeline_of(&self, id: &str) -> Option<&TimelineStore> {
        self.get(id).map(|s| &s.timeline)
    }

    pub fn name_of(&self, id: &str) -> Option<&str> {
        self.get(id).map(|s| s.name.as_str())
    }

    pub fn open_tabs(&self) -> &[String] {
        &self.open_tabs
    }

    pub fn is_tab_open(&self, id: &str) -> bool {
        self.open_tabs.iter().any(|t| t == id)
    }

    // -------------------------------------------------------- Dirty-Tracking

    /// Aggregierte Revision über ALLE Sequenzen — Basis des projektweiten
    /// Dirty-Trackings (eine Änderung in irgendeiner Sequenz zählt).
    pub fn aggregate_revision(&self) -> u64 {
        self.sequences.iter().map(|s| s.timeline.revision).sum()
    }

    // ----------------------------------------------------------- Tab-Wechsel

    /// Sequenz aktivieren (öffnet ihren Tab, falls nötig). Liefert true, wenn
    /// sich die aktive Sequenz geändert hat.
    pub fn set_active(&mut self, id: &str) -> bool {
        let Some(idx) = self.sequences.iter().position(|s| s.id == id) else {
            return false;
        };
        if !self.open_tabs.iter().any(|t| t == id) {
            self.open_tabs.push(id.to_string());
        }
        if self.active == idx {
            return false;
        }
        self.active = idx;
        true
    }

    /// Tab schließen (Sequenz bleibt im Projekt). Wechselt die aktive Sequenz,
    /// falls der aktive Tab geschlossen wird. Der letzte offene Tab bleibt
    /// erhalten (es muss immer eine sichtbare Sequenz geben).
    pub fn close_tab(&mut self, id: &str) {
        if self.open_tabs.len() <= 1 {
            return;
        }
        let Some(pos) = self.open_tabs.iter().position(|t| t == id) else {
            return;
        };
        let was_active = self.active_id() == id;
        self.open_tabs.remove(pos);
        if was_active {
            // Nachbar-Tab aktivieren (bevorzugt der vormals rechte).
            let next = self.open_tabs.get(pos).or_else(|| self.open_tabs.last());
            if let Some(next_id) = next.cloned() {
                if let Some(idx) = self.sequences.iter().position(|s| s.id == next_id) {
                    self.active = idx;
                }
            }
        }
    }

    /// Tab an eine neue Position verschieben (Drag-Reihenfolge).
    pub fn reorder_tab(&mut self, id: &str, to_index: usize) {
        let Some(from) = self.open_tabs.iter().position(|t| t == id) else {
            return;
        };
        let tab = self.open_tabs.remove(from);
        let to = to_index.min(self.open_tabs.len());
        self.open_tabs.insert(to, tab);
    }

    // ------------------------------------------------------------ Mutationen

    /// Neue, leere Sequenz anlegen, aktivieren und öffnen. Liefert ihre ID.
    pub fn add(&mut self, name: Option<String>, settings: SequenceSettings, bin_id: &str) -> String {
        let name = self.unique_name(name.unwrap_or_else(|| self.next_default_name()));
        let mut timeline = TimelineStore::default();
        timeline.set_sequence_settings(settings);
        let seq = Sequence::new(name, bin_id, timeline);
        let id = seq.id.clone();
        self.sequences.push(seq);
        self.open_tabs.push(id.clone());
        self.active = self.sequences.len() - 1;
        id
    }

    /// Eine fertig aufgebaute Sequenz einsetzen, aktivieren und ihren Tab
    /// öffnen (Interop-Import). Der Name wird bei Kollision eindeutig gemacht.
    /// Liefert ihre (ggf. neu vergebene, eindeutige) ID.
    pub fn add_sequence(&mut self, mut seq: Sequence) -> String {
        seq.name = self.unique_name(seq.name.clone());
        // ID-Kollisionen mit bestehenden Sequenzen ausschließen.
        if seq.id.trim().is_empty() || self.sequences.iter().any(|s| s.id == seq.id) {
            seq.id = new_id();
        }
        let id = seq.id.clone();
        self.sequences.push(seq);
        self.open_tabs.push(id.clone());
        self.active = self.sequences.len() - 1;
        id
    }

    /// Eine fertig aufgebaute Sequenz im Hintergrund einsetzen, OHNE die aktive
    /// Sequenz zu wechseln oder einen Tab zu öffnen (Multicam-Quelle: erscheint
    /// im Browser, der Nutzer bleibt im Schnitt). Liefert die (ggf. neu
    /// vergebene) ID.
    pub fn add_background(&mut self, mut seq: Sequence) -> String {
        seq.name = self.unique_name(seq.name.clone());
        if seq.id.trim().is_empty() || self.sequences.iter().any(|s| s.id == seq.id) {
            seq.id = new_id();
        }
        let id = seq.id.clone();
        self.sequences.push(seq);
        id
    }

    /// Multicam-Quelle dieser (Quell-)Sequenz, falls die Sequenz eine ist.
    pub fn multicam_source(&self, seq_id: &str) -> Option<&crate::core::multicam::MulticamSource> {
        self.get(seq_id).and_then(|s| s.timeline.multicam.as_ref())
    }

    /// Ist die Sequenz eine Multicam-Quelle?
    pub fn is_multicam_source(&self, seq_id: &str) -> bool {
        self.get(seq_id)
            .is_some_and(|s| s.timeline.multicam.is_some())
    }

    /// Multicam-Clips der AKTIVEN Sequenz „auf einzelne Clips reduzieren":
    /// jeden Multicam-Clip durch einen normalen Clip seines aktiven Winkels
    /// ersetzen (Asset = Winkel-Original, `src_in` = gemeinsame Zeit − pos).
    /// `clip_ids = None` ⇒ alle Multicam-Clips der Sequenz. Liefert die Anzahl
    /// reduzierter Clips.
    pub fn flatten_multicam(&mut self, clip_ids: Option<&[String]>) -> usize {
        use crate::core::timeline::TrackKind;
        let active = self.active;
        // 1. Betroffene Multicam-Clips der aktiven Sequenz.
        let targets: Vec<(String, String, u32, bool)> = self.sequences[active]
            .timeline
            .clips
            .iter()
            .filter(|c| c.is_multicam())
            .filter(|c| clip_ids.is_none_or(|ids| ids.contains(&c.id)))
            .filter_map(|c| {
                c.multicam
                    .as_ref()
                    .map(|mc| (c.id.clone(), mc.source.clone(), mc.angle, c.kind == TrackKind::Audio))
            })
            .collect();
        if targets.is_empty() {
            return 0;
        }
        // 2. Winkeldaten aus den Quell-Sequenzen auflösen. AUDIO-Clips folgen dem
        // festen Audio-Winkel (audio_angle_idx), nicht dem Video-Winkel.
        let mut resolved: std::collections::HashMap<String, (String, f64, f64, String)> =
            std::collections::HashMap::new();
        for (clip_id, source, angle, is_audio) in &targets {
            if let Some(src) = self.get(source).and_then(|s| s.timeline.multicam.as_ref()) {
                let idx = if *is_audio {
                    src.audio_angle_idx(*angle) as u32
                } else {
                    *angle
                };
                if let Some(a) = src.angle(idx) {
                    resolved.insert(
                        clip_id.clone(),
                        (a.asset_id.clone(), a.pos, a.duration, a.name.clone()),
                    );
                }
            }
        }
        if resolved.is_empty() {
            return 0;
        }
        // 3. Aktive Timeline mutieren (ein History-Eintrag).
        let tl = &mut self.sequences[active].timeline;
        tl.push_history();
        let mut count = 0usize;
        for c in tl.clips.iter_mut() {
            if let Some((asset_id, pos, duration, name)) = resolved.get(&c.id) {
                c.asset_id = asset_id.clone();
                c.src_in = (c.src_in - pos).max(0.0);
                c.src_duration = *duration;
                c.name = if c.kind == TrackKind::Audio {
                    format!("{name} (Audio)")
                } else {
                    name.clone()
                };
                c.multicam = None;
                count += 1;
            }
        }
        count
    }

    /// Sequenz duplizieren (gleicher Inhalt, frische History + ID). Liefert
    /// die ID des Duplikats und aktiviert es.
    pub fn duplicate(&mut self, id: &str) -> Option<String> {
        let src = self.get(id)?;
        let name = self.unique_name(format!("{} Kopie", src.name));
        let bin_id = src.bin_id.clone();
        let timeline = src.timeline.duplicate_content();
        let seq = Sequence::new(name, bin_id, timeline);
        let new_id = seq.id.clone();
        self.sequences.push(seq);
        self.open_tabs.push(new_id.clone());
        self.active = self.sequences.len() - 1;
        Some(new_id)
    }

    /// Sequenz umbenennen (eindeutiger Name).
    pub fn rename(&mut self, id: &str, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() {
            return false;
        }
        if self.get(id).is_some_and(|s| s.name == name) {
            return false;
        }
        let unique = self.unique_name_excluding(name.to_string(), id);
        if let Some(s) = self.get_mut(id) {
            s.name = unique;
            true
        } else {
            false
        }
    }

    /// Sequenz löschen. Schutz: die letzte Sequenz bleibt erhalten. Entfernt
    /// zugleich verwaiste Nest-Clips in anderen Sequenzen, die sie referenzieren
    /// (samt zugehöriger Übergänge). Liefert true bei Erfolg.
    pub fn remove(&mut self, id: &str) -> bool {
        if self.sequences.len() <= 1 {
            return false;
        }
        let Some(pos) = self.sequences.iter().position(|s| s.id == id) else {
            return false;
        };
        // Multicam-Clips, die diese Sequenz als Quelle nutzen, vor dem Löschen
        // auf ihren aktiven Winkel flachklopfen → Material verwaist nicht.
        self.flatten_multicam_clips_of(id);
        // Verwaiste Nest-Clips in den verbleibenden Sequenzen aufräumen.
        for seq in self.sequences.iter_mut() {
            if seq.id == id {
                continue;
            }
            seq.timeline.remove_nest_clips_of(id);
        }
        self.sequences.remove(pos);
        self.open_tabs.retain(|t| t != id);
        // Aktiven Index reparieren.
        if self.active >= self.sequences.len() {
            self.active = self.sequences.len() - 1;
        } else if pos <= self.active && self.active > 0 {
            self.active -= 1;
        }
        // Sicherstellen, dass mindestens ein Tab offen ist und die aktive
        // Sequenz darin liegt.
        let active_id = self.sequences[self.active].id.clone();
        if !self.open_tabs.iter().any(|t| t == &active_id) {
            self.open_tabs.insert(0, active_id);
        }
        true
    }

    // ------------------------------------------------------------ Nesting

    /// Sequenzen als Nest-Clips in die AKTIVE Sequenz einsetzen (Drop aus dem
    /// Browser). Sequenzen, die einen Zyklus erzeugen würden (auch transitiv),
    /// werden übersprungen. Liefert (eingefügt, wegen_zyklus_abgelehnt).
    pub fn insert_nests(&mut self, seq_ids: &[String], at: f64, track_id: Option<&str>) -> (usize, usize) {
        let host = self.active_id().to_string();
        let mut planned: Vec<(String, String, f64, bool)> = Vec::new();
        let mut rejected = 0usize;
        for sid in seq_ids {
            let Some(seq) = self.get(sid) else { continue };
            if self.would_create_cycle(&host, sid) {
                rejected += 1;
                continue;
            }
            let name = seq.name.clone();
            let len = crate::core::timeline::sequence_end(&seq.timeline.clips);
            let len = if len > 0.0 {
                len
            } else {
                crate::core::timeline::IMAGE_DEFAULT_DURATION
            };
            let has_audio = seq
                .timeline
                .clips
                .iter()
                .any(|c| c.kind == crate::core::timeline::TrackKind::Audio);
            planned.push((sid.clone(), name, len, has_audio));
        }
        if planned.is_empty() {
            return (0, rejected);
        }
        // Auto-Deref auf die aktive Timeline.
        let inserted = self.insert_nest_clips(&planned, at, track_id);
        (inserted, rejected)
    }

    // ------------------------------------------------------- Rekursionsschutz

    /// Würde das Einfügen von `nested_id` als Nest in `host_id` einen Zyklus
    /// erzeugen? Wahr, wenn `nested_id == host_id` oder `host_id` (transitiv)
    /// bereits aus `nested_id` heraus erreichbar ist (dann enthielte `host`
    /// eine Sequenz, die `host` enthält).
    pub fn would_create_cycle(&self, host_id: &str, nested_id: &str) -> bool {
        if host_id == nested_id {
            return true;
        }
        self.reaches(nested_id, host_id)
    }

    /// Ist `target` von `start` aus über Nest-Kanten erreichbar?
    fn reaches(&self, start: &str, target: &str) -> bool {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut stack: Vec<&str> = vec![start];
        while let Some(cur) = stack.pop() {
            if cur == target {
                return true;
            }
            if !seen.insert(cur) {
                continue;
            }
            if let Some(seq) = self.get(cur) {
                for n in seq.nested_ids() {
                    stack.push(n);
                }
            }
        }
        false
    }

    /// Sequenzen, die `id` (direkt) als Nest verwenden — für die Lösch-Warnung.
    pub fn nest_users(&self, id: &str) -> Vec<&Sequence> {
        self.sequences
            .iter()
            .filter(|s| s.id != id && s.nested_ids().any(|n| n == id))
            .collect()
    }

    /// Anzahl Nest-Clips über alle Sequenzen, die `id` referenzieren.
    pub fn nest_usage_count(&self, id: &str) -> usize {
        self.sequences
            .iter()
            .flat_map(|s| s.timeline.clips.iter())
            .filter(|c| c.nest_seq.as_deref() == Some(id))
            .count()
    }

    /// Anzahl Multicam-Clips über alle Sequenzen, die `id` als Multicam-Quelle
    /// nutzen — für die Lösch-Warnung (analog zu `nest_usage_count`).
    pub fn multicam_usage_count(&self, id: &str) -> usize {
        self.sequences
            .iter()
            .flat_map(|s| s.timeline.clips.iter())
            .filter(|c| c.multicam.as_ref().is_some_and(|m| m.source == id))
            .count()
    }

    /// Alle Multicam-Clips (in ALLEN Sequenzen), die `source_id` als Quelle
    /// nutzen, auf ihren aktiven Winkel flachklopfen — wie `flatten_multicam`,
    /// aber gezielt vor dem Löschen der Quell-Sequenz, damit das Material
    /// erhalten bleibt statt zu verwaisen. Liefert die Anzahl. Bewusst OHNE
    /// History-Eintrag: die Sequenz-Löschung selbst ist nicht undobar, sonst
    /// würde ein Undo den Clip wieder auf die gelöschte Quelle zeigen lassen.
    fn flatten_multicam_clips_of(&mut self, source_id: &str) -> usize {
        // 1. Plan bauen (Winkeldaten der Quelle pro betroffenem Clip auflösen).
        let mut plan: Vec<(usize, String, bool, (String, f64, f64, String))> = Vec::new();
        {
            let Some(src) = self.get(source_id).and_then(|s| s.timeline.multicam.as_ref()) else {
                return 0;
            };
            for (si, seq) in self.sequences.iter().enumerate() {
                for c in seq.timeline.clips.iter() {
                    let Some(mc) = c.multicam.as_ref() else { continue };
                    if mc.source != source_id {
                        continue;
                    }
                    // AUDIO-Clips folgen dem festen Audio-Winkel.
                    let is_audio = c.kind == crate::core::timeline::TrackKind::Audio;
                    let idx = if is_audio {
                        src.audio_angle_idx(mc.angle) as u32
                    } else {
                        mc.angle
                    };
                    if let Some(a) = src.angle(idx) {
                        plan.push((
                            si,
                            c.id.clone(),
                            is_audio,
                            (a.asset_id.clone(), a.pos, a.duration, a.name.clone()),
                        ));
                    }
                }
            }
        }
        // 2. Anwenden.
        let mut count = 0usize;
        for (si, clip_id, is_audio, (asset_id, pos, duration, name)) in &plan {
            if let Some(c) = self.sequences[*si]
                .timeline
                .clips
                .iter_mut()
                .find(|c| &c.id == clip_id)
            {
                c.asset_id = asset_id.clone();
                c.src_in = (c.src_in - pos).max(0.0);
                c.src_duration = *duration;
                c.name = if *is_audio {
                    format!("{name} (Audio)")
                } else {
                    name.clone()
                };
                c.multicam = None;
                count += 1;
            }
        }
        count
    }

    // --------------------------------------------------------------- Helfer

    fn next_default_name(&self) -> String {
        for n in 1..1000 {
            let candidate = format!("Sequenz {n:02}");
            if !self.sequences.iter().any(|s| s.name == candidate) {
                return candidate;
            }
        }
        format!("Sequenz {}", self.sequences.len() + 1)
    }

    fn unique_name(&self, name: String) -> String {
        self.unique_name_excluding(name, "")
    }

    fn unique_name_excluding(&self, name: String, exclude_id: &str) -> String {
        let base = if name.trim().is_empty() {
            "Sequenz".to_string()
        } else {
            name.trim().to_string()
        };
        let taken = |candidate: &str| {
            self.sequences
                .iter()
                .any(|s| s.id != exclude_id && s.name.eq_ignore_ascii_case(candidate))
        };
        if !taken(&base) {
            return base;
        }
        for n in 2..1000 {
            let candidate = format!("{base} {n}");
            if !taken(&candidate) {
                return candidate;
            }
        }
        base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with(names: &[&str]) -> SequenceStore {
        let mut s = SequenceStore::default();
        s.rename(&s.active_id().to_string(), names[0]);
        for n in &names[1..] {
            s.add(Some(n.to_string()), SequenceSettings::default(), crate::core::bin::ROOT_BIN_ID);
        }
        s
    }

    #[test]
    fn default_has_one_active_sequence() {
        let s = SequenceStore::default();
        assert_eq!(s.len(), 1);
        assert_eq!(s.active_index(), 0);
        assert_eq!(s.open_tabs().len(), 1);
        assert_eq!(s.active_name(), "Sequenz 01");
    }

    #[test]
    fn add_activates_and_opens_tab() {
        let mut s = SequenceStore::default();
        let id = s.add(None, SequenceSettings::default(), crate::core::bin::ROOT_BIN_ID);
        assert_eq!(s.len(), 2);
        assert_eq!(s.active_id(), id);
        assert!(s.is_tab_open(&id));
        assert_eq!(s.active_name(), "Sequenz 02");
    }

    #[test]
    fn unique_names_on_add_and_rename() {
        let mut s = store_with(&["Doku", "Doku"]);
        // Zweites "Doku" wird automatisch eindeutig.
        assert_ne!(s.all()[0].name, s.all()[1].name);
        let id1 = s.all()[1].id.clone();
        // Umbenennen auf einen vergebenen Namen erzwingt einen eindeutigen.
        s.rename(&id1, "Doku");
        assert_ne!(s.get(&id1).unwrap().name, "Doku");
    }

    #[test]
    fn close_tab_keeps_sequence_and_last_tab() {
        let mut s = SequenceStore::default();
        let a = s.active_id().to_string();
        let b = s.add(None, SequenceSettings::default(), crate::core::bin::ROOT_BIN_ID);
        assert_eq!(s.open_tabs().len(), 2);
        s.close_tab(&b);
        assert_eq!(s.open_tabs().len(), 1);
        assert_eq!(s.len(), 2, "Sequenz bleibt im Projekt");
        assert_eq!(s.active_id(), a, "aktive Sequenz wechselt auf Nachbarn");
        // Letzten Tab kann man nicht schließen.
        s.close_tab(&a);
        assert_eq!(s.open_tabs().len(), 1);
    }

    #[test]
    fn remove_refuses_last_and_repairs_active() {
        let mut s = SequenceStore::default();
        let a = s.active_id().to_string();
        let b = s.add(None, SequenceSettings::default(), crate::core::bin::ROOT_BIN_ID);
        assert_eq!(s.active_id(), b);
        assert!(s.remove(&b));
        assert_eq!(s.len(), 1);
        assert_eq!(s.active_id(), a);
        assert!(!s.remove(&a), "letzte Sequenz bleibt erhalten");
    }

    #[test]
    fn cycle_guard_direct_and_transitive() {
        // A, B, C. A nest B, B nest C. C darf A/B nicht enthalten.
        let mut s = store_with(&["A", "B", "C"]);
        let a = s.all()[0].id.clone();
        let b = s.all()[1].id.clone();
        let c = s.all()[2].id.clone();
        // Nest-Kanten direkt in den Timelines setzen (Testaufbau).
        push_nest(&mut s, &a, &b);
        push_nest(&mut s, &b, &c);

        assert!(s.would_create_cycle(&a, &a), "Selbst-Nest verboten");
        assert!(s.would_create_cycle(&c, &a), "C enthält A → A enthielte sich selbst");
        assert!(s.would_create_cycle(&c, &b), "transitiv: C→B→C");
        assert!(!s.would_create_cycle(&a, &c), "A darf C verschachteln (kein Zyklus)");
    }

    #[test]
    fn nest_users_and_count() {
        let mut s = store_with(&["A", "B"]);
        let a = s.all()[0].id.clone();
        let b = s.all()[1].id.clone();
        push_nest(&mut s, &a, &b);
        push_nest(&mut s, &a, &b);
        assert_eq!(s.nest_usage_count(&b), 2);
        assert_eq!(s.nest_users(&b).len(), 1);
    }

    #[test]
    fn undo_history_is_isolated_per_sequence() {
        let mut s = SequenceStore::default();
        let a = s.active_id().to_string();
        let b = s.add(None, SequenceSettings::default(), crate::core::bin::ROOT_BIN_ID);
        // In A einen Marker setzen (eigene History), dann in B.
        s.set_active(&a);
        s.add_marker_at(1.0);
        assert!(s.can_undo());
        s.set_active(&b);
        assert!(!s.can_undo(), "frische Sequenz B hat keine History");
        s.add_marker_at(2.0);
        // Undo in B betrifft nur B.
        s.undo();
        assert_eq!(s.timeline_of(&b).unwrap().markers.len(), 0);
        assert_eq!(s.timeline_of(&a).unwrap().markers.len(), 1, "A unberührt");
        // A kann weiterhin eigenständig rückgängig gemacht werden.
        s.set_active(&a);
        s.undo();
        assert_eq!(s.timeline_of(&a).unwrap().markers.len(), 0);
    }

    /// Test-Helfer: einen Nest-Clip in die Timeline `host` einsetzen.
    fn push_nest(s: &mut SequenceStore, host: &str, nested: &str) {
        let seq = s.get_mut(host).unwrap();
        let track_id = seq.timeline.tracks[0].id.clone();
        let mut clip = crate::core::timeline::test_clip(&track_id);
        clip.nest_seq = Some(nested.to_string());
        seq.timeline.clips.push(clip);
    }
}
