//! Zentraler App-Zustand: App-, Medien-, Wiedergabe- und Monitor-Store
//! plus Kontextwerte für when-Klauseln.

use crate::core::bin::{Bin, MediaLabel, MediaViewState, ROOT_BIN_ID};
use crate::core::proxy::{proxy_is_valid, ProxySettings};
use crate::core::types::{FfmpegInfo, MediaAsset};

/// Anzeigename der impliziten Wurzel (kein echter Bin-Eintrag).
pub const ROOT_BIN_NAME: &str = "Projekt";

/// Ziel einer anstehenden Inline-Umbenennung im Medien-Browser.
#[derive(Clone, Debug, PartialEq)]
pub enum RenameTarget {
    Asset(String),
    Bin(String),
}

/// Undo-Snapshot der Medien-Organisation (Assets + Bins). `seq` ordnet die
/// Operation global gegen die Timeline-History ein (siehe `core::next_op_seq`).
#[derive(Clone)]
struct MediaSnapshot {
    assets: Vec<MediaAsset>,
    bins: Vec<Bin>,
    seq: u64,
}

/// Obergrenze der Medien-Undo-History (wie die Timeline).
const MEDIA_HISTORY_LIMIT: usize = 100;

pub const WORKSPACE_IDS: [&str; 6] = ["media", "edit", "color", "effects", "audio", "graphics"];

pub fn workspace_name(id: &str) -> &'static str {
    match id {
        "media" => "Medien",
        "edit" => "Schnitt",
        "color" => "Farbe",
        "effects" => "Effekte",
        "audio" => "Audio",
        "graphics" => "Grafik",
        _ => "?",
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DialogId {
    Shortcuts,
    Export,
    /// App-Einstellungen (Allgemein/Autosave/Wiedergabe/Medien/Erscheinungsbild).
    Settings,
    /// Autosave-Versionen des aktuellen Projekts auflisten/wiederherstellen.
    /// Auch der Absturz-Wiederherstellungs-Hinweis nutzt diesen Dialog
    /// (`AppStore::autosave_recover_hint`).
    AutosaveVersions,
    /// Wizard zum Wiederverknüpfen fehlender Medien.
    Relink,
    /// Sequenzeinstellungen (Auflösung, Framerate, Drop-Frame-Timecode).
    SequenceSettings,
    /// Prompt „Sequenz an Medien anpassen?“ nach dem ersten Clip-Drop.
    MatchMedia,
    /// „Geschwindigkeit/Dauer“ der ausgewählten Clips (Mod+R).
    ClipSpeed,
    /// Marker bearbeiten (Name/Notiz/Farbe/Timecode) — Ziel in
    /// `AppStore::marker_editor`.
    Marker,
    /// Bin löschen — Inhalt-Behandlung abfragen. Ziel in
    /// `AppStore::bin_delete_target`.
    DeleteBin,
    /// Entfernen verwendeter Medien bestätigen (Assets sind in der Auswahl).
    ConfirmRemoveMedia,
    /// Proxy-Einstellungen (Codec + Auflösung) wählen.
    ProxySettings,
    /// Beenden bestätigen, während noch Render-Jobs laufen/warten.
    ConfirmQuitRender,
    /// Sequenz löschen, die als Nest verwendet wird (Warnung). Ziel in
    /// `AppStore::sequence_delete_target`.
    ConfirmDeleteSequence,
    /// Ergebnis eines Interop-Import/-Exports (Kennzahlen + Auslassungen).
    /// Inhalt in `AppStore::interop_report`.
    InteropReport,
}

/// Ziel des Marker-Bearbeiten-Dialogs: welche Sammlung + welcher Marker.
#[derive(Clone, Debug, PartialEq)]
pub struct MarkerEditTarget {
    pub scope: crate::core::marker::MarkerScope,
    pub marker_id: String,
}

pub type TimelineTool = &'static str;

pub const TOOLS: [TimelineTool; 8] = [
    "select", "razor", "ripple", "rolling", "slip", "slide", "hand", "zoom",
];

/// Kanonische deutsche Werkzeug-Bezeichnungen (StatusBar, Tooltips).
pub fn tool_label(tool: &str) -> &'static str {
    match tool {
        "select" => "Auswahl",
        "razor" => "Rasierklinge",
        "ripple" => "Ripple-Trimmen",
        "rolling" => "Rollen-Trimmen",
        "slip" => "Slip",
        "slide" => "Slide",
        "hand" => "Hand",
        "zoom" => "Zoom",
        _ => tool_command_title(tool),
    }
}

/// Command-Titel der Werkzeuge für Registry und Befehlspalette.
pub fn tool_command_title(tool: &str) -> &'static str {
    match tool {
        "select" => "Auswahlwerkzeug",
        "razor" => "Rasierklingen-Werkzeug",
        "ripple" => "Ripple-Trimmen-Werkzeug",
        "rolling" => "Rollen-Trimmen-Werkzeug",
        "slip" => "Slip-Werkzeug",
        "slide" => "Slide-Werkzeug",
        "hand" => "Hand-Werkzeug",
        "zoom" => "Zoom-Werkzeug",
        _ => "?",
    }
}

/// Anzeigedauer transienter Statusmeldungen.
const STATUS_MESSAGE_TIMEOUT: f64 = 6.0;

pub struct AppStore {
    pub active_workspace: String,
    pub open_dialog: Option<DialogId>,
    pub command_palette_open: bool,
    pub active_tool: TimelineTool,
    pub status_message: Option<String>,
    status_deadline: f64,
    pub ffmpeg: Option<FfmpegInfo>,
    /// Verfügbare ffmpeg-Encoder (None = noch nicht erfragt).
    pub encoders: Option<std::collections::HashSet<String>>,
    /// Fokussiertes Panel (Kontext-Key `panel`) — zuletzt angeklicktes Panel.
    pub focused_panel: String,
    /// Aktive Farbpipette (Chroma-Key): Zielparameter, die der nächste Klick
    /// in den Programmmonitor mit der angeklickten Quellfarbe füllt.
    pub color_pick: Option<ColorPickRequest>,
    /// Übergangs-ID, deren Dauer-Eingabe die Timeline öffnen soll
    /// (gesetzt vom Kontextmenü, gelesen + geleert vom Timeline-Panel).
    pub edit_transition_duration: Option<String>,
    /// Ziel des Marker-Bearbeiten-Dialogs (gesetzt beim Öffnen via
    /// Shift+M / Doppelklick / Panel; gelesen vom MarkerDialog).
    pub marker_editor: Option<MarkerEditTarget>,
    /// Bin-ID, deren Löschung der DeleteBin-Dialog bestätigt.
    pub bin_delete_target: Option<String>,
    /// Vom Statusleisten-Klick / Command „Warteschlange öffnen" gesetzt:
    /// der Export-Dialog springt beim nächsten Frame auf den Queue-Tab.
    pub export_open_queue: bool,
    /// Vom Beenden-Bestätigungsdialog gesetzt: die Hauptschleife beendet die
    /// App trotz laufender Render-Jobs.
    pub quit_requested: bool,
    /// Sequenz, deren Tab/Browser-Eintrag inline umbenannt werden soll
    /// (vom Command gesetzt, von der Tab-Leiste/Browser konsumiert).
    pub rename_sequence: Option<String>,
    /// Ziel des „Sequenz löschen“-Bestätigungsdialogs (als Nest verwendet).
    pub sequence_delete_target: Option<String>,
    /// Ergebnis-Bericht des letzten Interop-Import/-Exports (vom Vorgang
    /// gesetzt, vom Ergebnis-Dialog gelesen). Nicht persistiert.
    pub interop_report: Option<crate::core::interop::InteropReport>,
    /// Aktiver, aufgelöster UI-Scale (HiDPI-Faktor). Wird jeden Frame vom
    /// Mainloop aus DPI/Override gesetzt; Scale-Commands lesen ihn als
    /// Ausgangswert.
    pub ui_scale: f32,
    /// Vom Autosave-Versionen-Dialog gesetzt: diese Versionsdatei beim
    /// nächsten Frame als ungespeicherte Kopie öffnen (Original unberührt).
    /// Der Mainloop konsumiert die Anfrage.
    pub autosave_open_request: Option<std::path::PathBuf>,
    /// Beim Start gesetzt, wenn eine Autosave-Version neuer ist als die
    /// Projektdatei (mutmaßlicher Absturz). Der Autosave-Dialog zeigt dann
    /// einen Wiederherstellungs-Hinweis auf diese Datei.
    pub autosave_recover_hint: Option<std::path::PathBuf>,
}

/// Ziel einer Farbaufnahme: drei aufeinanderfolgende Effekt-Parameter
/// (R, G, B) einer Effekt-Instanz.
#[derive(Clone, Debug, PartialEq)]
pub struct ColorPickRequest {
    pub clip_id: String,
    pub fx_id: String,
    pub p_idx: usize,
}

impl Default for AppStore {
    fn default() -> Self {
        AppStore {
            active_workspace: "edit".into(),
            open_dialog: None,
            command_palette_open: false,
            active_tool: "select",
            status_message: None,
            status_deadline: 0.0,
            ffmpeg: None,
            encoders: None,
            focused_panel: String::new(),
            color_pick: None,
            edit_transition_duration: None,
            marker_editor: None,
            bin_delete_target: None,
            export_open_queue: false,
            quit_requested: false,
            rename_sequence: None,
            sequence_delete_target: None,
            interop_report: None,
            ui_scale: 1.0,
            autosave_open_request: None,
            autosave_recover_hint: None,
        }
    }
}

impl AppStore {
    pub fn set_status_message(&mut self, message: Option<String>, now: f64) {
        self.status_deadline = now + STATUS_MESSAGE_TIMEOUT;
        self.status_message = message;
    }

    /// Pro Frame: transiente Meldung nach 6 s wieder auf „Bereit“.
    pub fn tick_status(&mut self, now: f64) {
        if self.status_message.is_some() && now >= self.status_deadline {
            self.status_message = None;
        }
    }
}

/// Laufzeit-Status eines Proxy-Transcodes (nicht persistiert; der fertige
/// Proxy-Pfad landet direkt am [`MediaAsset`]).
#[derive(Clone, Debug)]
pub enum ProxyJobStatus {
    /// In der Warteschlange oder läuft — Fortschritt 0..1.
    Building(f32),
    /// Fehlgeschlagen (Fehlertext); Retry über „Proxies erstellen“.
    Failed(String),
}

#[derive(Default)]
pub struct MediaStore {
    pub assets: Vec<MediaAsset>,
    pub selected_asset_ids: Vec<String>,
    pub importing: bool,
    /// Waveform-Peaks je Asset (None = Extraktion fehlgeschlagen).
    pub waveforms: std::collections::HashMap<String, Option<Vec<f32>>>,
    /// Zählt Bestandsänderungen (Import/Entfernen/Relink) fürs Dirty-Tracking.
    pub revision: u64,
    /// Bins (Ordner) als Baum; die Wurzel ([`ROOT_BIN_ID`]) ist implizit und
    /// nicht in dieser Liste enthalten. Assets tragen ihr `bin_id` selbst.
    pub bins: Vec<Bin>,
    /// Persistierter Ansichts-Zustand des Browsers (Modus, Sortierung,
    /// Spaltenbreiten, geöffneter Bin).
    pub view: MediaViewState,
    /// Anstehende Inline-Umbenennung (vom Command gesetzt, vom Panel
    /// konsumiert und geleert). Nicht persistiert.
    pub rename_request: Option<RenameTarget>,
    /// Scrub-Vorschaubilder (Hover-Scrub): asset_id → (bucket → Cache-Pfad).
    /// Lazy vom Browser angefordert, vom Mainloop gefüllt. Nicht persistiert.
    pub scrub_thumbs: std::collections::HashMap<String, std::collections::HashMap<u32, String>>,
    /// Undo-History der Bin-/Metadaten-Operationen (getrennt von der Timeline;
    /// `edit.undo`/`edit.redo` koordinieren beide über die op-Sequenz).
    past: Vec<MediaSnapshot>,
    future: Vec<MediaSnapshot>,
    /// Globaler „Proxies verwenden“-Schalter (Premiere-Pendant): bei aktiv
    /// dekodieren Player/Scrub/Waveform aus der Proxy-Datei. Der EXPORT nutzt
    /// IMMER die Originale. Persistiert ab Formatversion 10.
    pub use_proxies: bool,
    /// Proxy-Format/-Auflösung für neue Transcodes (persistiert).
    pub proxy_settings: ProxySettings,
    /// Laufende/fehlgeschlagene Proxy-Jobs je Asset (Laufzeit, nicht persistiert).
    pub proxy_jobs: std::collections::HashMap<String, ProxyJobStatus>,
}

impl MediaStore {
    pub fn asset(&self, id: &str) -> Option<&MediaAsset> {
        self.assets.iter().find(|a| a.id == id)
    }

    pub fn select(&mut self, ids: Vec<String>) {
        self.selected_asset_ids = ids;
    }

    pub fn add_asset(&mut self, mut asset: MediaAsset) {
        // Frische Importe landen im aktuell geöffneten Bin (Premiere-Verhalten);
        // existiert er nicht (mehr), fällt das Asset in die Wurzel zurück.
        let target = self.view.current_bin.clone();
        asset.bin_id = if self.bin_exists(&target) { target } else { ROOT_BIN_ID.to_string() };
        self.assets.push(asset);
        self.revision += 1;
    }

    pub fn remove_assets(&mut self, ids: &[String]) {
        self.assets.retain(|a| !ids.contains(&a.id));
        self.selected_asset_ids.retain(|id| !ids.contains(id));
        self.revision += 1;
    }

    /// Anzahl der Assets, deren Quelldatei fehlt.
    pub fn offline_count(&self) -> usize {
        self.assets.iter().filter(|a| a.offline).count()
    }

    // -------------------------------------------------------------- Proxys
    // Proxys sind leichtgewichtige Transcodes für die Vorschau. Der globale
    // Schalter `use_proxies` lenkt Player/Scrub/Waveform auf die Proxy-Datei;
    // der EXPORT nimmt unverändert die Originale (siehe core::export).

    /// „Proxies verwenden“ setzen. Bei Aktivierung werden alle Proxys frisch
    /// validiert (Existenz/Staleness); Scrub-Vorschauen werden verworfen, damit
    /// sie aus der jeweils aktiven Quelle (Proxy/Original) neu entstehen.
    pub fn set_use_proxies(&mut self, on: bool) {
        if self.use_proxies == on {
            return;
        }
        self.use_proxies = on;
        if on {
            self.revalidate_proxies();
        }
        self.scrub_thumbs.clear();
        self.revision += 1;
    }

    /// Proxy-Status eines Assets (für Badges/Info-Panel).
    pub fn proxy_status(&self, asset_id: &str) -> Option<&ProxyJobStatus> {
        self.proxy_jobs.get(asset_id)
    }

    /// Proxy-Job als laufend markieren (Fortschritt 0..1).
    pub fn set_proxy_building(&mut self, asset_id: &str, pct: f32) {
        self.proxy_jobs
            .insert(asset_id.to_string(), ProxyJobStatus::Building(pct.clamp(0.0, 1.0)));
    }

    /// Proxy-Job als fehlgeschlagen markieren (Fehler-Badge + Retry). Reiner
    /// Laufzeit-Zustand (nicht persistiert) — kein Dirty-Flag.
    pub fn set_proxy_failed(&mut self, asset_id: &str, error: String) {
        self.proxy_jobs
            .insert(asset_id.to_string(), ProxyJobStatus::Failed(error));
    }

    /// Laufenden/fehlgeschlagenen Proxy-Job vergessen (Abbruch/Quittieren).
    pub fn clear_proxy_job(&mut self, asset_id: &str) {
        self.proxy_jobs.remove(asset_id);
    }

    /// Fertigen Proxy am Asset eintragen (Pfad + Quell-mtime), Job beenden.
    pub fn apply_proxy_result(&mut self, asset_id: &str, proxy_path: String, src_mtime: Option<f64>) {
        self.proxy_jobs.remove(asset_id);
        self.scrub_thumbs.remove(asset_id);
        if let Some(asset) = self.assets.iter_mut().find(|a| a.id == asset_id) {
            asset.proxy_path = Some(proxy_path);
            asset.proxy_src_mtime = src_mtime;
            asset.proxy_offline = false;
            self.revision += 1;
        }
    }

    /// Proxy-Zuordnung eines Assets entfernen (Proxy verwerfen). Liefert den
    /// bisherigen Proxy-Pfad (zum Löschen der Datei durch den Aufrufer).
    pub fn detach_proxy(&mut self, asset_id: &str) -> Option<String> {
        self.proxy_jobs.remove(asset_id);
        let asset = self.assets.iter_mut().find(|a| a.id == asset_id)?;
        let prev = asset.proxy_path.take();
        asset.proxy_src_mtime = None;
        asset.proxy_offline = false;
        if prev.is_some() {
            self.scrub_thumbs.remove(asset_id);
            self.revision += 1;
        }
        prev
    }

    /// Alle Proxys neu validieren (Datei vorhanden + nicht veraltet). Setzt das
    /// Laufzeit-Flag `proxy_offline`. Liefert true, wenn sich ein Status
    /// geändert hat (z. B. Proxy-Datei zwischenzeitlich gelöscht → Fallback).
    pub fn revalidate_proxies(&mut self) -> bool {
        let mut changed = false;
        for asset in self.assets.iter_mut() {
            let Some(proxy) = asset.proxy_path.clone() else {
                continue;
            };
            let valid = proxy_is_valid(&proxy, &asset.path, asset.proxy_src_mtime);
            if asset.proxy_offline == valid {
                asset.proxy_offline = !valid;
                changed = true;
            }
        }
        if changed {
            self.revision += 1;
        }
        changed
    }

    /// Anzahl Assets mit gültigem Proxy.
    pub fn proxy_count(&self) -> usize {
        self.assets.iter().filter(|a| a.has_valid_proxy()).count()
    }

    // ----------------------------------------------------------- Marker
    // Asset-/Quell-Marker (Quellmonitor) liegen außerhalb der Timeline-
    // Undo-History; sie zählen wie Import/Entfernen zum Dirty-Tracking.

    /// Asset-Marker an der Quellzeit `t` setzen (idempotent pro Sekunde —
    /// einfacher Toleranzvergleich, da Asset-fps hier nicht bekannt sind).
    /// Liefert die ID des (neuen oder bestehenden) Markers.
    pub fn add_asset_marker(&mut self, asset_id: &str, t: f64) -> Option<String> {
        let asset = self.assets.iter_mut().find(|a| a.id == asset_id)?;
        let t = t.max(0.0);
        if let Some(existing) = asset
            .markers
            .iter()
            .find(|m| (m.time - t).abs() < 1e-3)
            .map(|m| m.id.clone())
        {
            return Some(existing);
        }
        let marker = crate::core::marker::Marker::new(t);
        let id = marker.id.clone();
        asset.markers.push(marker);
        crate::core::timeline::sort_markers(&mut asset.markers);
        self.revision += 1;
        Some(id)
    }

    /// Asset-Marker ändern.
    pub fn asset_marker_update(
        &mut self,
        asset_id: &str,
        marker_id: &str,
        f: impl FnOnce(&mut crate::core::marker::Marker),
    ) {
        if let Some(asset) = self.assets.iter_mut().find(|a| a.id == asset_id) {
            if let Some(m) = asset.markers.iter_mut().find(|m| m.id == marker_id) {
                f(m);
                m.sanitize();
                crate::core::timeline::sort_markers(&mut asset.markers);
                self.revision += 1;
            }
        }
    }

    /// Asset-Marker entfernen.
    pub fn remove_asset_marker(&mut self, asset_id: &str, marker_id: &str) {
        if let Some(asset) = self.assets.iter_mut().find(|a| a.id == asset_id) {
            let before = asset.markers.len();
            asset.markers.retain(|m| m.id != marker_id);
            if asset.markers.len() != before {
                self.revision += 1;
            }
        }
    }

    /// Den zur Quellzeit `t` nächstgelegenen Asset-Marker (für „löschen").
    pub fn asset_marker_at(&self, asset_id: &str, t: f64) -> Option<String> {
        let asset = self.asset(asset_id)?;
        asset
            .markers
            .iter()
            .find(|m| (m.time - t).abs() < 1e-3 || (m.duration > 0.0 && t >= m.time && t <= m.end()))
            .map(|m| m.id.clone())
    }

    /// Neue Quelldatei für ein Asset übernehmen (Relink): Pfad/Metadaten
    /// ersetzen, Anzeigename bleibt erhalten, Waveform wird neu extrahiert.
    pub fn relink_asset(
        &mut self,
        asset_id: &str,
        path: String,
        info: crate::core::types::MediaInfo,
        thumbnail_path: Option<String>,
    ) -> bool {
        let Some(asset) = self.assets.iter_mut().find(|a| a.id == asset_id) else {
            return false;
        };
        asset.path = path;
        asset.info = info;
        if thumbnail_path.is_some() {
            asset.thumbnail_path = thumbnail_path;
        }
        asset.offline = false;
        self.waveforms.remove(asset_id);
        self.revision += 1;
        true
    }

    // ----------------------------------------------------------- Bins / Undo

    /// Schreibt den aktuellen Stand (Assets + Bins) in die Undo-History und
    /// markiert die Operation global. Vor jeder Bin-/Metadaten-Mutation rufen.
    fn push_media_history(&mut self) {
        self.past.push(MediaSnapshot {
            assets: self.assets.clone(),
            bins: self.bins.clone(),
            seq: crate::core::next_op_seq(),
        });
        if self.past.len() > MEDIA_HISTORY_LIMIT {
            self.past.remove(0);
        }
        self.future.clear();
        self.revision += 1;
    }

    pub fn can_undo(&self) -> bool {
        !self.past.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.future.is_empty()
    }

    /// Sequenz der zuletzt rückgängig machbaren Medien-Operation (für die
    /// `edit.undo`-Koordination; höchste Sequenz = jüngste Operation).
    pub fn undo_seq(&self) -> Option<u64> {
        self.past.last().map(|s| s.seq)
    }

    /// Sequenz der als Nächstes wiederherstellbaren Medien-Operation.
    pub fn redo_seq(&self) -> Option<u64> {
        self.future.first().map(|s| s.seq)
    }

    pub fn undo(&mut self) {
        let Some(prev) = self.past.pop() else { return };
        self.future.insert(
            0,
            MediaSnapshot {
                assets: std::mem::replace(&mut self.assets, prev.assets),
                bins: std::mem::replace(&mut self.bins, prev.bins),
                seq: prev.seq,
            },
        );
        self.reconcile_after_history();
    }

    pub fn redo(&mut self) {
        if self.future.is_empty() {
            return;
        }
        let next = self.future.remove(0);
        self.past.push(MediaSnapshot {
            assets: std::mem::replace(&mut self.assets, next.assets),
            bins: std::mem::replace(&mut self.bins, next.bins),
            seq: next.seq,
        });
        self.reconcile_after_history();
    }

    /// History löschen (Projektwechsel/Laden).
    pub fn clear_history(&mut self) {
        self.past.clear();
        self.future.clear();
    }

    /// Nach Undo/Redo: Auswahl/Navigation auf existierende Einträge stutzen.
    fn reconcile_after_history(&mut self) {
        let asset_ids: std::collections::HashSet<&str> =
            self.assets.iter().map(|a| a.id.as_str()).collect();
        self.selected_asset_ids.retain(|id| asset_ids.contains(id.as_str()));
        if !self.bin_exists(&self.view.current_bin.clone()) {
            self.view.current_bin = ROOT_BIN_ID.to_string();
        }
        self.revision += 1;
    }

    // --------------------------------------------------------------- Abfragen

    pub fn bin(&self, id: &str) -> Option<&Bin> {
        self.bins.iter().find(|b| b.id == id)
    }

    /// true für die implizite Wurzel und jeden existierenden Bin.
    pub fn bin_exists(&self, id: &str) -> bool {
        id == ROOT_BIN_ID || self.bins.iter().any(|b| b.id == id)
    }

    pub fn bin_name(&self, id: &str) -> String {
        if id == ROOT_BIN_ID {
            return ROOT_BIN_NAME.to_string();
        }
        self.bin(id).map(|b| b.name.clone()).unwrap_or_else(|| ROOT_BIN_NAME.to_string())
    }

    /// Direkte Unter-Bins von `parent`, alphabetisch sortiert.
    pub fn bin_children(&self, parent: &str) -> Vec<&Bin> {
        let mut out: Vec<&Bin> = self.bins.iter().filter(|b| b.parent == parent).collect();
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        out
    }

    /// Assets direkt in `bin_id` (Assets mit unbekanntem Bin gelten als Wurzel).
    pub fn assets_in_bin(&self, bin_id: &str) -> Vec<&MediaAsset> {
        self.assets
            .iter()
            .filter(|a| self.effective_bin(a) == bin_id)
            .collect()
    }

    /// Bin eines Assets, auf die Wurzel normalisiert, falls das Ziel fehlt.
    pub fn effective_bin<'a>(&self, asset: &'a MediaAsset) -> &'a str {
        if self.bin_exists(&asset.bin_id) {
            &asset.bin_id
        } else {
            ROOT_BIN_ID
        }
    }

    /// Breadcrumb von der Wurzel bis `id` (jeweils (id, name)).
    pub fn bin_path(&self, id: &str) -> Vec<(String, String)> {
        let mut chain: Vec<(String, String)> = Vec::new();
        let mut cur = id.to_string();
        // Schutz gegen Zyklen in fremden Dateien.
        let mut guard = 0;
        while cur != ROOT_BIN_ID && guard < 256 {
            let Some(b) = self.bin(&cur) else { break };
            chain.push((b.id.clone(), b.name.clone()));
            cur = b.parent.clone();
            guard += 1;
        }
        chain.push((ROOT_BIN_ID.to_string(), ROOT_BIN_NAME.to_string()));
        chain.reverse();
        chain
    }

    /// Lesbarer Pfad „Projekt / Footage / B-Roll“ (Suchtreffer-Anzeige).
    pub fn bin_path_label(&self, id: &str) -> String {
        self.bin_path(id)
            .into_iter()
            .map(|(_, name)| name)
            .collect::<Vec<_>>()
            .join(" / ")
    }

    /// Alle Bin-IDs des Teilbaums unter `id` (inklusive `id`).
    pub fn bin_subtree(&self, id: &str) -> Vec<String> {
        let mut out = vec![id.to_string()];
        let mut i = 0;
        while i < out.len() {
            let parent = out[i].clone();
            for child in self.bins.iter().filter(|b| b.parent == parent) {
                if !out.contains(&child.id) {
                    out.push(child.id.clone());
                }
            }
            i += 1;
        }
        out
    }

    /// true, wenn `maybe_child` im Teilbaum von `ancestor` liegt (für die
    /// Zyklus-Prüfung beim Verschieben eines Bins).
    pub fn is_descendant(&self, maybe_child: &str, ancestor: &str) -> bool {
        if maybe_child == ancestor {
            return true;
        }
        self.bin_subtree(ancestor).iter().any(|id| id == maybe_child)
    }

    /// Anzahl Assets im Teilbaum eines Bins (für „Löschen“-Bestätigung).
    pub fn count_assets_in_subtree(&self, id: &str) -> usize {
        let subtree: std::collections::HashSet<String> =
            self.bin_subtree(id).into_iter().collect();
        self.assets
            .iter()
            .filter(|a| subtree.contains(self.effective_bin(a)))
            .count()
    }

    // -------------------------------------------------------------- Mutationen

    /// Neuen Bin unter `parent` anlegen; liefert dessen ID. Undobar.
    pub fn create_bin(&mut self, parent: &str, name: &str) -> String {
        let parent = if self.bin_exists(parent) { parent.to_string() } else { ROOT_BIN_ID.to_string() };
        self.push_media_history();
        let bin = Bin::new(unique_bin_name(&self.bins, &parent, name), parent);
        let id = bin.id.clone();
        self.bins.push(bin);
        id
    }

    /// Bin umbenennen. Undobar.
    pub fn rename_bin(&mut self, id: &str, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let Some(parent) = self.bin(id).map(|b| b.parent.clone()) else { return };
        if self.bin(id).is_some_and(|b| b.name == name) {
            return;
        }
        let unique = unique_bin_name_excluding(&self.bins, &parent, name, id);
        self.push_media_history();
        if let Some(b) = self.bins.iter_mut().find(|b| b.id == id) {
            b.name = unique;
        }
    }

    /// Asset-Anzeigename ändern (unabhängig vom Dateinamen). Undobar.
    pub fn rename_asset(&mut self, id: &str, name: &str) {
        let name = name.trim();
        if name.is_empty() || self.asset(id).is_some_and(|a| a.name == name) {
            return;
        }
        if self.asset(id).is_none() {
            return;
        }
        self.push_media_history();
        if let Some(a) = self.assets.iter_mut().find(|a| a.id == id) {
            a.name = name.to_string();
        }
    }

    /// Farbetikett auf mehrere Assets setzen/löschen (None = kein Etikett).
    /// Undobar.
    pub fn set_label(&mut self, ids: &[String], label: Option<MediaLabel>) {
        let any = self
            .assets
            .iter()
            .any(|a| ids.contains(&a.id) && a.label != label);
        if !any {
            return;
        }
        self.push_media_history();
        for a in self.assets.iter_mut().filter(|a| ids.contains(&a.id)) {
            a.label = label;
        }
    }

    /// Assets in einen Bin verschieben (Drag&Drop). Undobar.
    pub fn move_assets_to_bin(&mut self, ids: &[String], bin_id: &str) {
        let bin_id = if self.bin_exists(bin_id) { bin_id.to_string() } else { ROOT_BIN_ID.to_string() };
        let any = self
            .assets
            .iter()
            .any(|a| ids.contains(&a.id) && a.bin_id != bin_id);
        if !any {
            return;
        }
        self.push_media_history();
        for a in self.assets.iter_mut().filter(|a| ids.contains(&a.id)) {
            a.bin_id = bin_id.clone();
        }
    }

    /// Bin in einen anderen Bin verschieben (Drag&Drop). Verhindert Zyklen
    /// (kein Verschieben in den eigenen Teilbaum). Undobar.
    pub fn move_bin(&mut self, bin_id: &str, new_parent: &str) {
        if bin_id == ROOT_BIN_ID {
            return;
        }
        let new_parent = if self.bin_exists(new_parent) { new_parent.to_string() } else { ROOT_BIN_ID.to_string() };
        if self.is_descendant(&new_parent, bin_id) {
            return; // Würde einen Zyklus erzeugen.
        }
        if self.bin(bin_id).is_some_and(|b| b.parent == new_parent) {
            return;
        }
        if self.bin(bin_id).is_none() {
            return;
        }
        let unique = {
            let name = self.bin(bin_id).map(|b| b.name.clone()).unwrap_or_default();
            unique_bin_name_excluding(&self.bins, &new_parent, &name, bin_id)
        };
        self.push_media_history();
        if let Some(b) = self.bins.iter_mut().find(|b| b.id == bin_id) {
            b.parent = new_parent;
            b.name = unique;
        }
    }

    /// Bin löschen. `keep_contents`: Inhalt (Unter-Bins + Assets) in den
    /// Eltern-Bin heben; sonst den ganzen Teilbaum samt Assets entfernen.
    /// Liefert die IDs entfernter Assets (Aufräumen von Clips/Quellmonitor).
    /// Undobar.
    pub fn delete_bin(&mut self, id: &str, keep_contents: bool) -> Vec<String> {
        if id == ROOT_BIN_ID || self.bin(id).is_none() {
            return Vec::new();
        }
        let parent = self.bin(id).map(|b| b.parent.clone()).unwrap_or_else(|| ROOT_BIN_ID.to_string());
        self.push_media_history();
        let mut removed: Vec<String> = Vec::new();
        if keep_contents {
            // Direkte Unter-Bins + Assets in den Eltern-Bin heben.
            for b in self.bins.iter_mut() {
                if b.parent == id {
                    b.parent = parent.clone();
                }
            }
            for a in self.assets.iter_mut() {
                if a.bin_id == id {
                    a.bin_id = parent.clone();
                }
            }
            self.bins.retain(|b| b.id != id);
        } else {
            let subtree: std::collections::HashSet<String> =
                self.bin_subtree(id).into_iter().collect();
            removed = self
                .assets
                .iter()
                .filter(|a| subtree.contains(&a.bin_id))
                .map(|a| a.id.clone())
                .collect();
            self.assets.retain(|a| !subtree.contains(&a.bin_id));
            self.bins.retain(|b| !subtree.contains(&b.id));
            self.selected_asset_ids.retain(|aid| !removed.contains(aid));
        }
        if !self.bin_exists(&self.view.current_bin.clone()) {
            self.view.current_bin = parent;
        }
        removed
    }

    /// Geöffneten Bin wechseln (Navigation). Auswahl wird zurückgesetzt.
    pub fn set_current_bin(&mut self, id: &str) {
        let id = if self.bin_exists(id) { id } else { ROOT_BIN_ID };
        if self.view.current_bin != id {
            self.view.current_bin = id.to_string();
            self.selected_asset_ids.clear();
        }
    }

    pub fn current_bin(&self) -> &str {
        &self.view.current_bin
    }

    /// Bins/Assets nach dem Laden konsolidieren: Asset-Bins und Bin-Eltern auf
    /// existierende Ziele normalisieren, Zyklen auflösen (Reparenting zur
    /// Wurzel). History/Auswahl bleiben unberührt (Aufrufer regelt das).
    pub fn reconcile_bins(&mut self) {
        let existing: std::collections::HashSet<String> =
            self.bins.iter().map(|b| b.id.clone()).collect();
        // Eltern-Verweise auf existierende Bins normalisieren.
        for b in self.bins.iter_mut() {
            if b.parent != ROOT_BIN_ID && !existing.contains(&b.parent) {
                b.parent = ROOT_BIN_ID.to_string();
            }
        }
        // Zyklen auflösen: Bins, die sich (über Eltern) selbst erreichen, an
        // die Wurzel hängen.
        let ids: Vec<String> = self.bins.iter().map(|b| b.id.clone()).collect();
        for id in ids {
            let mut cur = self.bin(&id).map(|b| b.parent.clone()).unwrap_or_default();
            let mut guard = 0;
            let mut cyclic = false;
            while cur != ROOT_BIN_ID && guard < 1024 {
                if cur == id {
                    cyclic = true;
                    break;
                }
                cur = match self.bin(&cur) {
                    Some(b) => b.parent.clone(),
                    None => break,
                };
                guard += 1;
            }
            if cyclic {
                if let Some(b) = self.bins.iter_mut().find(|b| b.id == id) {
                    b.parent = ROOT_BIN_ID.to_string();
                }
            }
        }
        // Asset-Bins auf existierende Bins normalisieren.
        for a in self.assets.iter_mut() {
            if a.bin_id != ROOT_BIN_ID && !existing.contains(&a.bin_id) {
                a.bin_id = ROOT_BIN_ID.to_string();
            }
        }
        if !self.bin_exists(&self.view.current_bin.clone()) {
            self.view.current_bin = ROOT_BIN_ID.to_string();
        }
    }
}

/// Eindeutigen Bin-Namen innerhalb eines Eltern-Bins erzeugen (hängt bei
/// Kollision „ 2“, „ 3“ … an).
fn unique_bin_name(bins: &[Bin], parent: &str, name: &str) -> String {
    unique_bin_name_excluding(bins, parent, name, "")
}

fn unique_bin_name_excluding(bins: &[Bin], parent: &str, name: &str, exclude_id: &str) -> String {
    let base = if name.trim().is_empty() { "Neuer Ordner" } else { name.trim() };
    let taken = |candidate: &str| {
        bins.iter().any(|b| {
            b.id != exclude_id && b.parent == parent && b.name.eq_ignore_ascii_case(candidate)
        })
    };
    if !taken(base) {
        return base.to_string();
    }
    for n in 2..1000 {
        let candidate = format!("{base} {n}");
        if !taken(&candidate) {
            return candidate;
        }
    }
    base.to_string()
}

/// Wiedergabe-Zustand eines Monitors (Quellmonitor; Programm nutzt die Timeline).
#[derive(Default)]
pub struct SourcePlayerState {
    pub position: f64,
    pub playing: bool,
    pub rate: f64,
    pub in_mark: Option<f64>,
    pub out_mark: Option<f64>,
    pub looping: bool,
}

pub struct PlaybackStore {
    /// Asset, das aktuell im Quellmonitor geladen ist.
    pub source_asset_id: Option<String>,
    pub source: SourcePlayerState,
    /// Programm (Timeline): Position = timeline.playhead_sec.
    pub program_playing: bool,
    pub program_rate: f64,
    /// Transient: Playhead wird gerade per Lineal/Scrubber gezogen (für
    /// Audio-Scrubbing). Panels setzen das pro UI-Frame, der Mainloop reseted es.
    pub scrub_active: bool,
    /// Audio-Scrubbing (kurze Sample-Schnipsel am Playhead) aktiviert.
    pub audio_scrub_enabled: bool,
}

impl Default for PlaybackStore {
    fn default() -> Self {
        PlaybackStore {
            source_asset_id: None,
            source: SourcePlayerState {
                rate: 1.0,
                ..Default::default()
            },
            program_playing: false,
            program_rate: 1.0,
            scrub_active: false,
            audio_scrub_enabled: true,
        }
    }
}

/// Laufzeit-Pegel der Audio-Engine (nicht persistiert): Spitzenwerte des
/// zuletzt gemischten Blocks, linear (1.0 = 0 dBFS), vor dem Hard-Clip
/// gemessen — Werte > 1 zeigen Übersteuerung an.
#[derive(Default)]
pub struct AudioStore {
    /// Peak L/R pro Audio-Spur (track_id), nach Spur-Gain/Pan.
    pub track_levels: std::collections::HashMap<String, [f32; 2]>,
    /// Peak L/R der Summe nach Master-Gain.
    pub master_level: [f32; 2],
    /// Live-Gain-Reduktion (dB, ≥ 0) je Dynamik-Effekt (fx_id) aus den
    /// laufenden Clip-/Spur-Ketten — Quelle der GR-Meter im Panel
    /// Effekteinstellungen. fx-IDs sind global eindeutig.
    pub fx_gain_reduction: std::collections::HashMap<String, f32>,
}

/// Wiedergabeauflösung: 1 = voll, darunter kleinerer Offscreen-Render.
pub const PREVIEW_SCALES: [(f64, &str); 4] =
    [(1.0, "Voll"), (0.5, "1/2"), (0.25, "1/4"), (0.125, "1/8")];

/// Ansichtsmodus des Programmmonitors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MonitorView {
    /// Komponiertes Programmbild (Standard).
    #[default]
    Program,
    /// Multicam-Raster: alle Winkel der aktiven Multicam-Quelle synchron.
    Multicam,
}

pub struct MonitorStore {
    pub source_scale: f64,
    pub program_scale: f64,
    /// Ansichtsmodus (Programm vs. Multicam-Raster).
    pub view: MonitorView,
    /// Downgesampelte Kopie des zuletzt dekodierten Frames je Programm-Clip
    /// (clip_id) — Bildquelle der Scopes (CPU-seitig, ohne GPU-Readback).
    pub preview_frames: std::collections::HashMap<String, MiniFrame>,
    /// Canvas-Größe des Programmmonitors in Pixeln (vom Panel gemeldet) —
    /// Raster-Auflösung der Titel-Engine (scharf in Anzeigegröße).
    pub program_canvas: (u32, u32),
    /// Sichere Ränder (Action-/Title-Safe) im Programmmonitor einblenden.
    pub safe_margins: bool,
    /// Performance-Overlay (Decode-/Upload-/Frame-Zeiten, Cache-Trefferquote)
    /// im Programmmonitor einblenden.
    pub show_perf_overlay: bool,
    /// Laufzeit-Performance-Messwerte (vom Mainloop/Player gefüllt).
    pub perf: PerfStats,
    /// Der Programmmonitor zeigt in diesem Frame den Sequenz-Render-Cache
    /// (ein Decoder statt Live-Compositing) — vom Player je Frame gesetzt.
    pub program_from_cache: bool,
}

/// Performance-Telemetrie der Wiedergabe (nicht persistiert). Decode-/Upload-/
/// Frame-Zeiten sind exponentiell geglättet (EMA), damit das Overlay nicht
/// flackert. Drop-Zähler liefern den Resolve-artigen Indikator.
#[derive(Clone, Copy, Default)]
pub struct PerfStats {
    /// Dauer von `PlayerEngine::tick` in ms (Decode + Frame-Auswahl), geglättet.
    pub decode_ms: f32,
    /// Dauer der Datei-Texture-Uploads in ms, geglättet.
    pub upload_ms: f32,
    /// Gesamte Frame-Zeit in ms (Wall-Clock), geglättet.
    pub frame_ms: f32,
    /// Aktuelle Bildwiederholrate der App.
    pub fps: f32,
    /// Insgesamt verworfene Video-Frames seit Start (Überlast).
    pub dropped_total: u64,
    /// In den letzten ~2 s verworfene Frames (Indikator-Zahl im Monitor).
    pub dropped_recent: u32,
    /// Frame-Cache: Treffer/Fehlversuche kumuliert.
    pub cache_hits: u64,
    pub cache_misses: u64,
    /// Frame-Cache-Belegung in MB und Eintragszahl.
    pub cache_used_mb: f32,
    pub cache_entries: u64,
}

impl PerfStats {
    /// Frame-Cache-Trefferquote 0..1 (für das Overlay).
    pub fn cache_hit_ratio(&self) -> f32 {
        let total = self.cache_hits + self.cache_misses;
        if total == 0 {
            0.0
        } else {
            self.cache_hits as f32 / total as f32
        }
    }
}

/// Kleines RGBA-Standbild (Scopes-Analyse).
#[derive(Clone)]
pub struct MiniFrame {
    pub w: usize,
    pub h: usize,
    pub rgba: Vec<u8>,
}

impl Default for MonitorStore {
    fn default() -> Self {
        MonitorStore {
            source_scale: 1.0,
            program_scale: 1.0,
            view: MonitorView::default(),
            preview_frames: Default::default(),
            program_canvas: (0, 0),
            safe_margins: false,
            show_perf_overlay: false,
            perf: PerfStats::default(),
            program_from_cache: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::{MediaInfo, MediaKind};

    fn asset(id: &str, name: &str) -> MediaAsset {
        MediaAsset {
            id: id.into(),
            path: format!("/tmp/{id}.mp4"),
            name: name.into(),
            kind: MediaKind::Video,
            info: MediaInfo {
                path: format!("/tmp/{id}.mp4"),
                file_name: format!("{id}.mp4"),
                container: "mp4".into(),
                duration_sec: 5.0,
                size_bytes: 100,
                video: Vec::new(),
                audio: Vec::new(),
                recorded_at: None,
            },
            thumbnail_path: None,
            imported_at: 0.0,
            bin_id: ROOT_BIN_ID.to_string(),
            label: None,
            offline: false,
            markers: Vec::new(),
            proxy_path: None,
            proxy_src_mtime: None,
            proxy_offline: false,
        }
    }

    #[test]
    fn create_and_nav_bins() {
        let mut m = MediaStore::default();
        let footage = m.create_bin(ROOT_BIN_ID, "Footage");
        let broll = m.create_bin(&footage, "B-Roll");
        assert_eq!(m.bins.len(), 2);
        assert_eq!(m.bin_children(ROOT_BIN_ID).len(), 1);
        assert_eq!(m.bin_children(&footage).len(), 1);
        assert_eq!(m.bin_path_label(&broll), "Projekt / Footage / B-Roll");
        // Eindeutige Namen je Eltern-Bin.
        let dup = m.create_bin(ROOT_BIN_ID, "Footage");
        assert_eq!(m.bin_name(&dup), "Footage 2");
    }

    #[test]
    fn assets_assigned_to_open_bin_and_moveable() {
        let mut m = MediaStore::default();
        let footage = m.create_bin(ROOT_BIN_ID, "Footage");
        // Import landet im geöffneten Bin.
        m.set_current_bin(&footage);
        m.add_asset(asset("a1", "a1"));
        assert_eq!(m.assets_in_bin(&footage).len(), 1);
        assert_eq!(m.assets_in_bin(ROOT_BIN_ID).len(), 0);
        // Zurück in die Wurzel verschieben.
        m.move_assets_to_bin(&["a1".into()], ROOT_BIN_ID);
        assert_eq!(m.assets_in_bin(ROOT_BIN_ID).len(), 1);
        assert_eq!(m.assets_in_bin(&footage).len(), 0);
    }

    #[test]
    fn delete_bin_keep_vs_remove() {
        let mut m = MediaStore::default();
        let footage = m.create_bin(ROOT_BIN_ID, "Footage");
        let sub = m.create_bin(&footage, "Sub");
        m.set_current_bin(&sub);
        m.add_asset(asset("a1", "a1"));
        m.set_current_bin(ROOT_BIN_ID);

        // Inhalt behalten: Asset + Unter-Bin wandern eine Ebene hoch.
        let mut keep = MediaStore::default();
        let f2 = keep.create_bin(ROOT_BIN_ID, "Footage");
        let s2 = keep.create_bin(&f2, "Sub");
        keep.set_current_bin(&s2);
        keep.add_asset(asset("a1", "a1"));
        keep.set_current_bin(ROOT_BIN_ID);
        let removed = keep.delete_bin(&f2, true);
        assert!(removed.is_empty());
        // Sub ist jetzt direkt unter der Wurzel, Asset bleibt in Sub.
        assert_eq!(keep.bin(&s2).unwrap().parent, ROOT_BIN_ID);
        assert_eq!(keep.assets_in_bin(&s2).len(), 1);

        // Inhalt mitlöschen: Asset-IDs werden zurückgegeben.
        let removed = m.delete_bin(&footage, false);
        assert_eq!(removed, vec!["a1".to_string()]);
        assert!(m.bin(&sub).is_none());
        assert!(m.asset("a1").is_none());
    }

    #[test]
    fn move_bin_prevents_cycles() {
        let mut m = MediaStore::default();
        let a = m.create_bin(ROOT_BIN_ID, "A");
        let b = m.create_bin(&a, "B");
        // A in seinen eigenen Nachfahren B zu verschieben ist verboten.
        m.move_bin(&a, &b);
        assert_eq!(m.bin(&a).unwrap().parent, ROOT_BIN_ID);
        // B unter die Wurzel verschieben ist erlaubt.
        m.move_bin(&b, ROOT_BIN_ID);
        assert_eq!(m.bin(&b).unwrap().parent, ROOT_BIN_ID);
    }

    #[test]
    fn media_undo_redo_roundtrips_bins_and_labels() {
        let mut m = MediaStore::default();
        m.add_asset(asset("a1", "a1"));
        assert!(!m.can_undo());

        let bin = m.create_bin(ROOT_BIN_ID, "Footage");
        assert!(m.can_undo());
        m.undo();
        assert!(m.bin(&bin).is_none(), "Bin-Anlage rückgängig");
        m.redo();
        assert!(m.bin(&bin).is_some(), "Bin-Anlage wiederhergestellt");

        // Etikett setzen + rückgängig.
        m.set_label(&["a1".into()], Some(MediaLabel::Orange));
        assert_eq!(m.asset("a1").unwrap().label, Some(MediaLabel::Orange));
        m.undo();
        assert_eq!(m.asset("a1").unwrap().label, None);
    }

    #[test]
    fn reconcile_normalizes_dangling_bins() {
        let mut m = MediaStore::default();
        // Bin mit fehlendem Elternteil + Asset in fehlendem Bin.
        m.bins.push(Bin { id: "x".into(), name: "X".into(), parent: "ghost".into() });
        let mut a = asset("a1", "a1");
        a.bin_id = "ghost".into();
        m.assets.push(a);
        m.reconcile_bins();
        assert_eq!(m.bin("x").unwrap().parent, ROOT_BIN_ID);
        assert_eq!(m.asset("a1").unwrap().bin_id, ROOT_BIN_ID);
    }
}
