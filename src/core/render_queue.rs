//! Render-Warteschlange: mehrere Export-Jobs (dieselbe Sequenz mit
//! verschiedenen Presets/Bereichen oder ganz andere Sequenzen) laufen
//! sequentiell im Hintergrund, während weitergearbeitet werden kann.
//!
//! Jeder Job trägt einen vollständigen, ENTKOPPELTEN Snapshot: Renderplan +
//! Einstellungen werden beim Einreihen gebaut und danach nie wieder aus der
//! Timeline gelesen. Spätere Edits oder ein Medien-Relink ändern den laufenden
//! Export also nicht — der Plan hält bereits owned Dateipfade (siehe
//! [`crate::core::export::VideoLayerPlan::path`]).
//!
//! Die Zustandsmaschine je Job: `Waiting → Running → {Done | Failed |
//! Cancelled}`. Genau ein Job läuft (sequentielle Abarbeitung); der „Pump"
//! ([`RenderQueue::next_to_start`]) startet den nächsten wartenden Job, sobald
//! keiner mehr läuft. Abgeschlossene Jobs bleiben für die Anzeige stehen, bis
//! sie entfernt/neu gestartet werden.

use crate::core::export::{ExportPhase, ExportSettings, RenderPlan};

/// Lebenszyklus eines Warteschlangen-Jobs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JobState {
    /// Eingereiht, wartet auf einen freien Worker.
    Waiting,
    /// Wird gerade gerendert (genau einer zur Zeit).
    Running,
    /// Erfolgreich fertig (Datei finalisiert).
    Done,
    /// Mit Fehler beendet (`error` gesetzt).
    Failed,
    /// Vom Benutzer abgebrochen.
    Cancelled,
}

impl JobState {
    /// Wartend oder laufend — blockiert das App-Beenden.
    pub fn is_active(self) -> bool {
        matches!(self, JobState::Waiting | JobState::Running)
    }
    /// Endzustand (fertig/Fehler/abgebrochen).
    pub fn is_finished(self) -> bool {
        matches!(self, JobState::Done | JobState::Failed | JobState::Cancelled)
    }
    pub fn label(self) -> &'static str {
        match self {
            JobState::Waiting => "Wartet",
            JobState::Running => "Läuft",
            JobState::Done => "Fertig",
            JobState::Failed => "Fehler",
            JobState::Cancelled => "Abgebrochen",
        }
    }
}

/// Ein Job in der Warteschlange.
pub struct QueueJob {
    /// Warteschlangen-lokale, monoton steigende Id (stabil über Reorder).
    pub id: u64,
    /// `services`-Job-Id, solange der Job läuft (für Fortschritt/Abbruch).
    pub service_id: Option<String>,
    /// Anzeigename (Dateiname des Ziels).
    pub name: String,
    /// Kurzbeschreibung (Codec · Auflösung · Bereich) für die Liste.
    pub summary: String,
    pub output: String,
    /// Entkoppelter Snapshot — wird beim Start in den Worker geklont.
    pub plan: RenderPlan,
    pub settings: ExportSettings,
    pub state: JobState,
    pub progress_pct: f64,
    pub phase: ExportPhase,
    pub frames_done: u64,
    pub frames_total: u64,
    pub render_fps: f64,
    pub eta_sec: Option<f64>,
    /// Zeitstempel (App-Uhr) des Starts bzw. Endes — Anzeige der Laufzeit.
    pub started_at: f64,
    pub finished_at: f64,
    pub error: Option<String>,
}

impl QueueJob {
    /// Verstrichene Renderzeit (s) — bis zum Ende eingefroren.
    pub fn elapsed(&self, now: f64) -> f64 {
        if self.started_at <= 0.0 {
            return 0.0;
        }
        let end = if self.state == JobState::Running { now } else { self.finished_at };
        (end - self.started_at).max(0.0)
    }
}

#[derive(Default)]
pub struct RenderQueue {
    pub jobs: Vec<QueueJob>,
    next_id: u64,
    /// Pausiert: kein neuer Job wird gestartet (laufender läuft zu Ende).
    pub paused: bool,
}

impl RenderQueue {
    /// Neuen Job einreihen (Zustand `Waiting`); liefert die Job-Id.
    pub fn enqueue(
        &mut self,
        name: String,
        summary: String,
        output: String,
        plan: RenderPlan,
        settings: ExportSettings,
    ) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        let frames_total = plan.total_frames;
        self.jobs.push(QueueJob {
            id,
            service_id: None,
            name,
            summary,
            output,
            plan,
            settings,
            state: JobState::Waiting,
            progress_pct: 0.0,
            phase: ExportPhase::MixAudio,
            frames_done: 0,
            frames_total,
            render_fps: 0.0,
            eta_sec: None,
            started_at: 0.0,
            finished_at: 0.0,
            error: None,
        });
        id
    }

    pub fn job(&self, id: u64) -> Option<&QueueJob> {
        self.jobs.iter().find(|j| j.id == id)
    }
    fn job_mut(&mut self, id: u64) -> Option<&mut QueueJob> {
        self.jobs.iter_mut().find(|j| j.id == id)
    }

    /// Aktuell laufender Job (für Statusleiste/Pump).
    pub fn running(&self) -> Option<&QueueJob> {
        self.jobs.iter().find(|j| j.state == JobState::Running)
    }
    pub fn is_running(&self) -> bool {
        self.jobs.iter().any(|j| j.state == JobState::Running)
    }
    /// Anzahl wartender Jobs.
    pub fn waiting_count(&self) -> usize {
        self.jobs.iter().filter(|j| j.state == JobState::Waiting).count()
    }
    /// Mindestens ein wartender oder laufender Job (App-Beenden-Warnung).
    pub fn has_active(&self) -> bool {
        self.jobs.iter().any(|j| j.state.is_active())
    }
    pub fn active_count(&self) -> usize {
        self.jobs.iter().filter(|j| j.state.is_active()).count()
    }

    /// Pump-Schritt: liefert die Id des nächsten zu startenden Jobs, falls
    /// gerade keiner läuft, nicht pausiert ist und ein Job wartet. Der Aufrufer
    /// startet ihn über `services` und meldet das Ergebnis via
    /// [`RenderQueue::mark_started`] / [`RenderQueue::mark_start_failed`].
    pub fn next_to_start(&self) -> Option<u64> {
        if self.paused || self.is_running() {
            return None;
        }
        self.jobs
            .iter()
            .find(|j| j.state == JobState::Waiting)
            .map(|j| j.id)
    }

    /// Job als laufend markieren (nach erfolgreichem `services`-Start).
    pub fn mark_started(&mut self, id: u64, service_id: String, now: f64) {
        if let Some(j) = self.job_mut(id) {
            j.state = JobState::Running;
            j.service_id = Some(service_id);
            j.started_at = now;
            j.progress_pct = 0.0;
            j.error = None;
        }
    }

    /// Start fehlgeschlagen (z. B. ungültige Settings) — direkt als Fehler.
    pub fn mark_start_failed(&mut self, id: u64, error: String, now: f64) {
        if let Some(j) = self.job_mut(id) {
            j.state = JobState::Failed;
            j.service_id = None;
            j.error = Some(error);
            j.finished_at = now;
        }
    }

    /// Fortschritts-Event eines Workers (Zuordnung über die `service_id`).
    #[allow(clippy::too_many_arguments)]
    pub fn on_progress(
        &mut self,
        service_id: &str,
        pct: f64,
        phase: ExportPhase,
        frames_done: u64,
        frames_total: u64,
        render_fps: f64,
        eta_sec: Option<f64>,
    ) {
        if let Some(j) = self
            .jobs
            .iter_mut()
            .find(|j| j.service_id.as_deref() == Some(service_id))
        {
            j.progress_pct = pct;
            j.phase = phase;
            j.frames_done = frames_done;
            j.frames_total = frames_total;
            j.render_fps = render_fps;
            j.eta_sec = eta_sec;
        }
    }

    /// Abschluss-Event eines Workers (Erfolg/Abbruch/Fehler).
    pub fn on_done(
        &mut self,
        service_id: &str,
        ok: bool,
        cancelled: bool,
        error: Option<String>,
        now: f64,
    ) {
        if let Some(j) = self
            .jobs
            .iter_mut()
            .find(|j| j.service_id.as_deref() == Some(service_id))
        {
            j.service_id = None;
            j.finished_at = now;
            if ok {
                j.state = JobState::Done;
                j.progress_pct = 100.0;
            } else if cancelled {
                j.state = JobState::Cancelled;
            } else {
                j.state = JobState::Failed;
                j.error = error;
            }
        }
    }

    /// Job abbrechen. Liefert die `service_id`, falls der Job läuft (der
    /// Aufrufer killt dann den Worker über `services`); wartende Jobs werden
    /// sofort auf `Cancelled` gesetzt.
    pub fn cancel(&mut self, id: u64, now: f64) -> Option<String> {
        let j = self.job_mut(id)?;
        match j.state {
            JobState::Running => j.service_id.clone(),
            JobState::Waiting => {
                j.state = JobState::Cancelled;
                j.finished_at = now;
                None
            }
            _ => None,
        }
    }

    /// Beendeten Job erneut einreihen (gleicher Snapshot).
    pub fn restart(&mut self, id: u64) {
        if let Some(j) = self.job_mut(id) {
            if j.state.is_finished() {
                j.state = JobState::Waiting;
                j.service_id = None;
                j.progress_pct = 0.0;
                j.frames_done = 0;
                j.render_fps = 0.0;
                j.eta_sec = None;
                j.started_at = 0.0;
                j.finished_at = 0.0;
                j.error = None;
            }
        }
    }

    /// Job entfernen (nur wenn er nicht läuft).
    pub fn remove(&mut self, id: u64) {
        if self.job(id).map(|j| j.state != JobState::Running).unwrap_or(false) {
            self.jobs.retain(|j| j.id != id);
        }
    }

    /// Alle beendeten Jobs aus der Liste werfen.
    pub fn clear_finished(&mut self) {
        self.jobs.retain(|j| !j.state.is_finished());
    }

    /// In der Liste eins nach oben (Reihenfolge der Abarbeitung).
    pub fn move_up(&mut self, id: u64) {
        if let Some(i) = self.jobs.iter().position(|j| j.id == id) {
            if i > 0 {
                self.jobs.swap(i, i - 1);
            }
        }
    }
    pub fn move_down(&mut self, id: u64) {
        if let Some(i) = self.jobs.iter().position(|j| j.id == id) {
            if i + 1 < self.jobs.len() {
                self.jobs.swap(i, i + 1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::export::PRESETS;

    fn dummy_job(queue: &mut RenderQueue, name: &str) -> u64 {
        let settings = (PRESETS[0].build)((1920, 1080), 25.0);
        let mut plan = RenderPlan::default();
        plan.total_frames = 100;
        queue.enqueue(name.into(), "H.264 · 1080p".into(), format!("/tmp/{name}.mp4"), plan, settings)
    }

    #[test]
    fn enqueue_assigns_unique_ids_and_waiting_state() {
        let mut q = RenderQueue::default();
        let a = dummy_job(&mut q, "a");
        let b = dummy_job(&mut q, "b");
        assert_ne!(a, b);
        assert_eq!(q.job(a).unwrap().state, JobState::Waiting);
        assert_eq!(q.waiting_count(), 2);
        assert!(!q.is_running());
        assert!(q.has_active());
    }

    #[test]
    fn pump_starts_only_one_at_a_time_in_order() {
        let mut q = RenderQueue::default();
        let a = dummy_job(&mut q, "a");
        let b = dummy_job(&mut q, "b");
        // Erster Pump → ältester wartender Job.
        assert_eq!(q.next_to_start(), Some(a));
        q.mark_started(a, "svc-a".into(), 1.0);
        assert_eq!(q.running().unwrap().id, a);
        // Solange a läuft, startet b nicht.
        assert_eq!(q.next_to_start(), None);
        // a fertig → b ist dran.
        q.on_done("svc-a", true, false, None, 5.0);
        assert_eq!(q.job(a).unwrap().state, JobState::Done);
        assert_eq!(q.next_to_start(), Some(b));
    }

    #[test]
    fn paused_blocks_pump_but_keeps_jobs() {
        let mut q = RenderQueue::default();
        let a = dummy_job(&mut q, "a");
        q.paused = true;
        assert_eq!(q.next_to_start(), None);
        q.paused = false;
        assert_eq!(q.next_to_start(), Some(a));
    }

    #[test]
    fn progress_routes_by_service_id() {
        let mut q = RenderQueue::default();
        let a = dummy_job(&mut q, "a");
        q.mark_started(a, "svc-a".into(), 0.0);
        q.on_progress("svc-a", 42.0, ExportPhase::RenderVideo, 42, 100, 30.0, Some(2.0));
        let job = q.job(a).unwrap();
        assert_eq!(job.progress_pct, 42.0);
        assert_eq!(job.frames_done, 42);
        // Unbekannte Id ändert nichts.
        q.on_progress("svc-x", 99.0, ExportPhase::RenderVideo, 99, 100, 0.0, None);
        assert_eq!(q.job(a).unwrap().progress_pct, 42.0);
    }

    #[test]
    fn cancel_waiting_is_immediate_running_needs_worker() {
        let mut q = RenderQueue::default();
        let a = dummy_job(&mut q, "a");
        let b = dummy_job(&mut q, "b");
        // Wartender Job: sofort abgebrochen, keine service_id.
        assert_eq!(q.cancel(b, 1.0), None);
        assert_eq!(q.job(b).unwrap().state, JobState::Cancelled);
        // Laufender Job: liefert service_id zum Killen, Zustand erst beim Done.
        q.mark_started(a, "svc-a".into(), 0.0);
        assert_eq!(q.cancel(a, 2.0), Some("svc-a".into()));
        assert_eq!(q.job(a).unwrap().state, JobState::Running);
        q.on_done("svc-a", false, true, None, 3.0);
        assert_eq!(q.job(a).unwrap().state, JobState::Cancelled);
    }

    #[test]
    fn restart_requeues_finished_job() {
        let mut q = RenderQueue::default();
        let a = dummy_job(&mut q, "a");
        q.mark_started(a, "svc-a".into(), 0.0);
        q.on_done("svc-a", false, false, Some("boom".into()), 1.0);
        assert_eq!(q.job(a).unwrap().state, JobState::Failed);
        q.restart(a);
        let job = q.job(a).unwrap();
        assert_eq!(job.state, JobState::Waiting);
        assert_eq!(job.progress_pct, 0.0);
        assert!(job.error.is_none());
        // Laufende Jobs lassen sich nicht neu starten.
        q.mark_started(a, "svc-a2".into(), 2.0);
        q.restart(a);
        assert_eq!(q.job(a).unwrap().state, JobState::Running);
    }

    #[test]
    fn remove_skips_running_and_reorder_swaps() {
        let mut q = RenderQueue::default();
        let a = dummy_job(&mut q, "a");
        let b = dummy_job(&mut q, "b");
        q.move_down(a);
        assert_eq!(q.jobs[0].id, b);
        assert_eq!(q.jobs[1].id, a);
        q.move_up(a);
        assert_eq!(q.jobs[0].id, a);
        // Laufenden Job kann man nicht entfernen.
        q.mark_started(a, "svc-a".into(), 0.0);
        q.remove(a);
        assert!(q.job(a).is_some());
        // Wartenden schon.
        q.remove(b);
        assert!(q.job(b).is_none());
    }

    #[test]
    fn clear_finished_keeps_active() {
        let mut q = RenderQueue::default();
        let a = dummy_job(&mut q, "a");
        let b = dummy_job(&mut q, "b");
        q.mark_started(a, "svc-a".into(), 0.0);
        q.on_done("svc-a", true, false, None, 1.0);
        q.clear_finished();
        assert!(q.job(a).is_none());
        assert!(q.job(b).is_some());
    }
}
