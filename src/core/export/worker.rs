use super::*;
use crate::core::animation::ClipFx;
use crate::core::audio_fx::AudioFxChain;
use crate::core::compose;
use crate::core::effects::{self, EffectInstance};
use crate::core::grade::{self};
use crate::core::timeline::{
    TimelineClip, TimelineStore,
};
use crate::services::ServiceEvent;
use crate::stores::MediaStore;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

// ============================================================ Render-Worker

/// Phase des laufenden Exports (für die Fortschrittsanzeige).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExportPhase {
    MixAudio,
    RenderVideo,
    EncodeAudio,
    Finalize,
}

impl ExportPhase {
    pub fn label(&self) -> &'static str {
        match self {
            ExportPhase::MixAudio => "Audio wird gemischt",
            ExportPhase::RenderVideo => "Video wird gerendert",
            ExportPhase::EncodeAudio => "Audio wird kodiert",
            ExportPhase::Finalize => "Datei wird abgeschlossen",
        }
    }
}

enum ExportError {
    Cancelled,
    Failed(String),
}

type ChildList = Arc<Mutex<Vec<(u64, Child)>>>;

/// Vom Worker getrackte Kindprozesse — `cancel_job` killt sie von außen,
/// damit blockierende Pipe-Reads/-Writes sofort enden.
pub(crate) struct ChildRegistry {
    list: ChildList,
    next: u64,
}

impl ChildRegistry {
    pub(crate) fn new(list: ChildList) -> ChildRegistry {
        ChildRegistry { list, next: 0 }
    }

    fn spawn(&mut self, cmd: &mut Command) -> Result<(u64, Option<std::process::ChildStdin>, Option<std::process::ChildStdout>, Option<std::process::ChildStderr>), String> {
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("ffmpeg konnte nicht gestartet werden: {e}"))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let id = self.next;
        self.next += 1;
        self.list.lock().unwrap_or_else(|p| p.into_inner()).push((id, child));
        Ok((id, stdin, stdout, stderr))
    }

    /// Prozess beenden lassen und Status liefern (entfernt ihn aus der Liste).
    fn wait(&self, id: u64) -> Option<std::process::ExitStatus> {
        let mut list = self.list.lock().unwrap_or_else(|p| p.into_inner());
        let idx = list.iter().position(|(i, _)| *i == id)?;
        let (_, mut child) = list.swap_remove(idx);
        drop(list);
        child.wait().ok()
    }

    /// Prozess hart beenden (z. B. Decoder am Segmentende).
    fn kill(&self, id: u64) {
        let mut list = self.list.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(idx) = list.iter().position(|(i, _)| *i == id) {
            let (_, mut child) = list.swap_remove(idx);
            drop(list);
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn kill_all(&self) {
        let mut list = self.list.lock().unwrap_or_else(|p| p.into_inner());
        for (_, child) in list.iter_mut() {
            let _ = child.kill();
        }
        for (_, mut child) in list.drain(..) {
            let _ = child.wait();
        }
    }
}

impl Drop for ChildRegistry {
    /// Sicherheitsnetz gegen ffmpeg-Waisen: bei einem frühen `?`-Return, einem
    /// Compositing-Fehler oder einer Panik im Renderpfad zwischen `spawn` und
    /// `wait` würde der Registry sonst kommentarlos fallengelassen und die noch
    /// laufenden Decoder/Encoder blieben als Waisen zurück. `kill_all` ist
    /// idempotent (drain) — im Erfolgsfall ist die Liste bereits leer.
    fn drop(&mut self) {
        self.kill_all();
    }
}

/// Fortschritts-Tracker: Einheiten (Frames/Samples) → %, Rate, ETA.
/// Fortschritts-Senke des Compositing-Kerns (`render_segments`). Entkoppelt
/// den geteilten Renderpfad vom konkreten Event-Typ: Voll-Export meldet über
/// [`Progress`] (Export-Events), der Sequenz-Render-Cache über [`CountProgress`]
/// (eigener Callback).
trait FrameProgress {
    fn advance(&mut self, units: u64);
}

impl FrameProgress for Progress<'_> {
    fn advance(&mut self, units: u64) {
        Progress::advance(self, units);
    }
}

/// Schlanke Fortschritts-Senke für den Render-Cache: zählt Frames und ruft
/// gedrosselt (≤ 10/s) einen Callback `(done, total)`.
struct CountProgress<'a> {
    done: u64,
    total: u64,
    last_emit: std::time::Instant,
    cb: &'a mut dyn FnMut(u64, u64),
}

impl<'a> CountProgress<'a> {
    fn new(total: u64, cb: &'a mut dyn FnMut(u64, u64)) -> CountProgress<'a> {
        CountProgress {
            done: 0,
            total: total.max(1),
            last_emit: std::time::Instant::now() - std::time::Duration::from_secs(10),
            cb,
        }
    }
}

impl FrameProgress for CountProgress<'_> {
    fn advance(&mut self, units: u64) {
        self.done = (self.done + units).min(self.total);
        let now = std::time::Instant::now();
        if self.done >= self.total || now.duration_since(self.last_emit).as_millis() >= 100 {
            self.last_emit = now;
            (self.cb)(self.done, self.total);
        }
    }
}

pub(crate) struct Progress<'a> {
    tx: &'a Sender<ServiceEvent>,
    job_id: &'a str,
    phase: ExportPhase,
    base_pct: f64,
    span_pct: f64,
    total: u64,
    done: u64,
    /// Frames/Sekunde, exponentiell geglättet.
    rate: f64,
    last_rate_at: std::time::Instant,
    last_rate_done: u64,
    last_emit: std::time::Instant,
    /// Anzeige-Skala (Video: 1 = Frames; Audio: Samples → Sekundenanzeige).
    show_frames: bool,
}

impl<'a> Progress<'a> {
    pub(crate) fn new(tx: &'a Sender<ServiceEvent>, job_id: &'a str) -> Progress<'a> {
        let now = std::time::Instant::now();
        Progress {
            tx,
            job_id,
            phase: ExportPhase::MixAudio,
            base_pct: 0.0,
            span_pct: 0.0,
            total: 1,
            done: 0,
            rate: 0.0,
            last_rate_at: now,
            last_rate_done: 0,
            last_emit: now - std::time::Duration::from_secs(10),
            show_frames: false,
        }
    }

    fn begin_phase(&mut self, phase: ExportPhase, base: f64, span: f64, total: u64, frames: bool) {
        self.phase = phase;
        self.base_pct = base;
        self.span_pct = span;
        self.total = total.max(1);
        self.done = 0;
        self.rate = 0.0;
        self.last_rate_at = std::time::Instant::now();
        self.last_rate_done = 0;
        self.show_frames = frames;
        self.emit(true);
    }

    fn advance(&mut self, units: u64) {
        self.done = (self.done + units).min(self.total);
        self.emit(false);
    }

    fn emit(&mut self, force: bool) {
        let now = std::time::Instant::now();
        if !force && now.duration_since(self.last_emit).as_millis() < 100 {
            return;
        }
        self.last_emit = now;
        // Rate über ein ~0,5-s-Fenster glätten.
        let dt = now.duration_since(self.last_rate_at).as_secs_f64();
        if dt >= 0.5 {
            let inst = (self.done - self.last_rate_done) as f64 / dt;
            self.rate = if self.rate <= 0.0 {
                inst
            } else {
                self.rate * 0.6 + inst * 0.4
            };
            self.last_rate_at = now;
            self.last_rate_done = self.done;
        }
        let frac = self.done as f64 / self.total as f64;
        let eta = if self.rate > 0.0 && self.done > 0 {
            Some(((self.total - self.done) as f64 / self.rate).max(0.0))
        } else {
            None
        };
        let _ = self.tx.send(ServiceEvent::SequenceExportProgress {
            job_id: self.job_id.to_string(),
            pct: (self.base_pct + self.span_pct * frac).clamp(0.0, 100.0),
            phase: self.phase,
            frames_done: if self.show_frames { self.done } else { 0 },
            frames_total: if self.show_frames { self.total } else { 0 },
            render_fps: if self.show_frames { self.rate } else { 0.0 },
            eta_sec: eta,
        });
    }
}

/// Einstieg für den Worker-Thread (von `Services::start_sequence_export`).
pub fn run_export_worker(
    job_id: String,
    plan: RenderPlan,
    settings: ExportSettings,
    tx: Sender<ServiceEvent>,
    cancel: Arc<AtomicBool>,
    children: ChildList,
) {
    let registry = ChildRegistry::new(Arc::clone(&children));
    let part = part_path(&settings.output);
    let wav = std::env::temp_dir().join(format!("editron-mix-{job_id}.wav"));
    // Temp-SRTs fürs Einbetten (eine je Spur; .srt-Endung für den Demuxer).
    let subs: Vec<PathBuf> = if settings.subtitles == SubtitleMode::Embed {
        plan.subtitle_tracks
            .iter()
            .enumerate()
            .map(|(i, _)| std::env::temp_dir().join(format!("editron-sub-{job_id}-{i}.srt")))
            .collect()
    } else {
        Vec::new()
    };

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        export_inner(&job_id, &plan, &settings, &tx, &cancel, registry, &part, &wav, &subs)
    }));
    let outcome = match result {
        Ok(r) => r,
        Err(panic) => {
            let msg = panic
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unbekannte Ursache".to_string());
            Err(ExportError::Failed(format!("Interner Fehler: {msg}")))
        }
    };

    // Aufräumen: Kinder sind tot, tmp-Dateien weg; .part nur bei Erfolg renamed.
    ChildRegistry::new(children).kill_all();
    let _ = std::fs::remove_file(&wav);
    for sub in &subs {
        let _ = std::fs::remove_file(sub);
    }
    let (ok, cancelled, error) = match outcome {
        Ok(()) => (true, false, None),
        Err(ExportError::Cancelled) => {
            let _ = std::fs::remove_file(&part);
            (false, true, None)
        }
        Err(ExportError::Failed(msg)) => {
            let _ = std::fs::remove_file(&part);
            (false, false, Some(msg))
        }
    };
    let _ = tx.send(ServiceEvent::SequenceExportDone {
        job_id,
        ok,
        cancelled,
        error,
        output: settings.output.clone(),
    });
}

pub(crate) fn part_path(output: &str) -> PathBuf {
    PathBuf::from(format!("{output}.part"))
}

/// Pfad einer Sidecar-SRT: `<ziel>.srt` bei einer Spur, sonst
/// `<ziel>.<spurname>.srt` (z. B. `film.U2.srt`).
pub fn sidecar_srt_path(output: &str, track_name: &str, single: bool) -> PathBuf {
    let stem = Path::new(output).with_extension("");
    if single {
        PathBuf::from(format!("{}.srt", stem.display()))
    } else {
        PathBuf::from(format!("{}.{}.srt", stem.display(), track_name))
    }
}

#[allow(clippy::too_many_arguments)]
fn export_inner(
    job_id: &str,
    plan: &RenderPlan,
    settings: &ExportSettings,
    tx: &Sender<ServiceEvent>,
    cancel: &AtomicBool,
    mut children: ChildRegistry,
    part: &Path,
    wav: &Path,
    subs: &[PathBuf],
) -> Result<(), ExportError> {
    let mut progress = Progress::new(tx, job_id);
    let _ = std::fs::remove_file(part);

    // ---- Sonderfall: Bild-Sequenz (nur Video, jeder Frame eine Datei) ----
    if settings.container.image_sequence {
        progress.begin_phase(ExportPhase::RenderVideo, 0.0, 99.0, plan.total_frames, true);
        render_image_sequence(job_id, plan, settings, cancel, &mut children, &mut progress)
            .map_err(fail_or_cancel(cancel))?;
        if cancel.load(Ordering::Relaxed) {
            return Err(ExportError::Cancelled);
        }
        progress.begin_phase(ExportPhase::Finalize, 99.0, 1.0, 1, false);
        progress.advance(1);
        progress.emit(true);
        return Ok(());
    }

    let with_audio = settings.audio.is_some();
    let with_video = settings.video.is_some();

    // Einzubettende Untertitel als Temp-SRTs schreiben (Encoder-Inputs).
    for (sub, track) in subs.iter().zip(&plan.subtitle_tracks) {
        std::fs::write(sub, crate::core::subtitle::format_srt(&track.cues)).map_err(|e| {
            ExportError::Failed(format!("Untertitel-Zwischendatei konnte nicht geschrieben werden: {e}"))
        })?;
    }

    // ---- Phase A: Audio-Mixdown in eine temporäre f32-WAV ----
    if with_audio {
        let audio = settings.audio.as_ref().expect("audio settings");
        let span = if with_video { 6.0 } else { 85.0 };
        let total_units = plan.audio_total_units(audio.sample_rate);
        progress.begin_phase(ExportPhase::MixAudio, 0.0, span, total_units.max(1), false);
        mix_audio_to_wav(plan, audio, wav, cancel, &mut children, &mut progress)
            .map_err(fail_or_cancel(cancel))?;
    }

    // ---- Phase B: Video rendern bzw. Audio-only kodieren ----
    if with_video {
        let base = if with_audio { 6.0 } else { 0.0 };
        progress.begin_phase(ExportPhase::RenderVideo, base, 93.0 - base, plan.total_frames, true);
        render_video(
            plan,
            settings,
            with_audio.then_some(wav),
            subs,
            part,
            cancel,
            &mut children,
            &mut progress,
        )
        .map_err(fail_or_cancel(cancel))?;
    } else {
        progress.begin_phase(ExportPhase::EncodeAudio, 85.0, 14.0, 1, false);
        encode_audio_only(settings, wav, part, cancel, &mut children)
            .map_err(fail_or_cancel(cancel))?;
    }

    if cancel.load(Ordering::Relaxed) {
        return Err(ExportError::Cancelled);
    }

    // ---- Sidecar-Untertitel neben die Zieldatei schreiben ----
    if settings.subtitles == SubtitleMode::Sidecar {
        let single = plan.subtitle_tracks.len() == 1;
        for track in &plan.subtitle_tracks {
            let path = sidecar_srt_path(&settings.output, &track.name, single);
            std::fs::write(&path, crate::core::subtitle::format_srt(&track.cues)).map_err(
                |e| {
                    ExportError::Failed(format!(
                        "Untertitel-Datei konnte nicht geschrieben werden ({}): {e}",
                        path.display()
                    ))
                },
            )?;
        }
    }

    // ---- Finalisieren: atomar an den Zielort ----
    progress.begin_phase(ExportPhase::Finalize, 99.0, 1.0, 1, false);
    std::fs::rename(part, &settings.output).map_err(|e| {
        ExportError::Failed(format!(
            "Fertige Datei konnte nicht umbenannt werden ({} → {}): {e}",
            part.display(),
            settings.output
        ))
    })?;
    progress.advance(1);
    progress.emit(true);
    Ok(())
}

/// Bei gesetztem Abbruch-Flag wird jeder Folgefehler (gekillte Pipes) zu
/// `Cancelled` statt zu einem irreführenden Fehlertext.
fn fail_or_cancel(cancel: &AtomicBool) -> impl Fn(String) -> ExportError + '_ {
    move |msg| {
        if cancel.load(Ordering::Relaxed) {
            ExportError::Cancelled
        } else {
            ExportError::Failed(msg)
        }
    }
}

// ----------------------------------------------------------- Audio-Mixdown

/// WAV-Header für IEEE-Float (Format 3) inkl. fact-Chunk; liefert den
/// Daten-Offset.
fn write_wav_header(
    f: &mut std::fs::File,
    sample_rate: u32,
    channels: u16,
    data_bytes: u32,
) -> std::io::Result<u64> {
    let byte_rate = sample_rate * channels as u32 * 4;
    let block_align = channels * 4;
    let sample_frames = data_bytes / block_align as u32;
    let mut h: Vec<u8> = Vec::with_capacity(58);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&(50u32 + data_bytes).to_le_bytes()); // Chunks nach "WAVE"
    h.extend_from_slice(b"WAVE");
    h.extend_from_slice(b"fmt ");
    h.extend_from_slice(&18u32.to_le_bytes());
    h.extend_from_slice(&3u16.to_le_bytes()); // WAVE_FORMAT_IEEE_FLOAT
    h.extend_from_slice(&channels.to_le_bytes());
    h.extend_from_slice(&sample_rate.to_le_bytes());
    h.extend_from_slice(&byte_rate.to_le_bytes());
    h.extend_from_slice(&block_align.to_le_bytes());
    h.extend_from_slice(&32u16.to_le_bytes()); // Bits pro Sample
    h.extend_from_slice(&0u16.to_le_bytes()); // cbSize
    h.extend_from_slice(b"fact");
    h.extend_from_slice(&4u32.to_le_bytes());
    h.extend_from_slice(&sample_frames.to_le_bytes());
    h.extend_from_slice(b"data");
    h.extend_from_slice(&data_bytes.to_le_bytes());
    f.write_all(&h)?;
    Ok(h.len() as u64)
}

/// Alle Audio-Clips nacheinander in die WAV mischen (Read-Modify-Write an
/// der Zielposition) — konstanter Speicherbedarf, beliebig viele Clips.
pub(crate) fn mix_audio_to_wav(
    plan: &RenderPlan,
    audio: &AudioSettings,
    wav: &Path,
    cancel: &AtomicBool,
    children: &mut ChildRegistry,
    progress: &mut Progress,
) -> Result<(), String> {
    let rate = audio.sample_rate;
    let ch = audio.channels.clamp(1, 2) as usize;
    let total_frames = (plan.duration * rate as f64).round().max(1.0) as u64;
    let data_bytes = total_frames * ch as u64 * 4;
    if data_bytes >= u32::MAX as u64 - 1024 {
        return Err("Audio-Mix überschreitet die 4-GB-Grenze des WAV-Zwischenformats.".into());
    }

    let (mut file, data_off) = create_silent_wav(wav, rate, ch, data_bytes)?;

    // Schnellpfad-Clips (Spuren ohne Bus-FX/Automation): direkt in den Master.
    mix_clips_into_wav(
        &mut file, data_off, total_frames, rate, ch, &plan.audio, cancel, children, progress,
    )?;

    // Spuren mit Bus-FX und/oder Automation: getrennt mischen und einsummieren
    // (Bus-FX wirken auf die Spur-Summe — exakt wie der Player-Mixdown).
    for (idx, track) in plan.audio_tracks.iter().enumerate() {
        process_audio_track(
            &mut file, data_off, total_frames, rate, ch, wav, idx, track, cancel, children,
            progress,
        )?;
    }

    file.sync_all().ok();
    Ok(())
}

/// Leere f32-WAV (Stille) anlegen; liefert (Datei, Daten-Offset).
fn create_silent_wav(
    path: &Path,
    rate: u32,
    ch: usize,
    data_bytes: u64,
) -> Result<(std::fs::File, u64), String> {
    let mut file = std::fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(|e| format!("Audio-Zwischendatei konnte nicht angelegt werden: {e}"))?;
    let data_off = write_wav_header(&mut file, rate, ch as u16, data_bytes as u32)
        .map_err(|e| format!("WAV-Header: {e}"))?;
    // Mit Stille auffüllen (f32 0.0 = Null-Bytes — set_len reicht).
    file.set_len(data_off + data_bytes)
        .map_err(|e| format!("Audio-Zwischendatei: {e}"))?;
    Ok((file, data_off))
}

/// Alle übergebenen Clips nacheinander in die WAV mischen (Read-Modify-Write
/// an der Zielposition) — konstanter Speicher, beliebig viele Clips. Wird
/// für den Master (Schnellpfad-Clips) UND für Per-Spur-WAVs genutzt.
#[allow(clippy::too_many_arguments)]
fn mix_clips_into_wav(
    file: &mut std::fs::File,
    data_off: u64,
    total_frames: u64,
    rate: u32,
    ch: usize,
    clips: &[AudioClipPlan],
    cancel: &AtomicBool,
    children: &mut ChildRegistry,
    progress: &mut Progress,
) -> Result<(), String> {
    for clip in clips {
        if cancel.load(Ordering::Relaxed) {
            return Err("abgebrochen".into());
        }
        let offset_frames = (clip.start_in_mix * rate as f64).round().max(0.0) as u64;
        let want_frames =
            ((clip.duration * rate as f64).round() as u64).min(total_frames.saturating_sub(offset_frames));
        if want_frames == 0 {
            continue;
        }

        let mut cmd = Command::new(crate::services::ffmpeg_bin());
        cmd.args(["-v", "error", "-ss", &format!("{:.4}", clip.src_in)])
            // -t schneidet die QUELLE: Medienspanne = Ausgabedauer × speed.
            .args(["-t", &format!("{:.4}", clip.duration * clip.speed)])
            .args(["-i", &clip.path])
            .args(["-vn", "-sn"]);
        // Pitch-korrigiertes Tempo — identische Kette wie die Wiedergabe.
        if let Some(chain) = atempo_chain(clip.speed) {
            cmd.args(["-filter:a", &chain]);
        }
        cmd.args(["-f", "f32le", "-ac", &ch.to_string(), "-ar", &rate.to_string()])
            .arg("pipe:1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let (id, _, stdout, _) = children.spawn(&mut cmd)?;
        let mut stdout = stdout.ok_or("ffmpeg-stdout nicht verfügbar")?;

        // Pro Seite wirksamer Faktor; Mono mittelt beide Seiten.
        let gains: [f32; 2] = [clip.gain_l, clip.gain_r];
        let mono_gain = (clip.gain_l + clip.gain_r) * 0.5;
        // Lautstärke-Kurve (dB-Keyframes) und Übergangs-Crossfades:
        // blockweise ausgewertet — 256 Frames ≈ 5 ms bei 48 kHz, glatt
        // genug für Fades.
        let has_envelope =
            clip.volume.is_animated() || clip.volume.value != 0.0 || !clip.fades.is_empty();
        const ENV_BLOCK: usize = 256;

        // Audio-Effekt-Kette (identischer DSP wie im Player-Mixdown);
        // animierte Parameter werden je ENV_BLOCK nachgestimmt.
        let fx_refs: Vec<&EffectInstance> = clip.effects.iter().collect();
        let mut fx_chain = AudioFxChain::build(&fx_refs, rate, ch, clip.src_in);
        let fx_animated = clip.effects.iter().any(|e| e.any_animated());

        const CHUNK_FRAMES: usize = 32768;
        let mut decoded = vec![0u8; CHUNK_FRAMES * ch * 4];
        let mut existing = vec![0u8; CHUNK_FRAMES * ch * 4];
        let mut fresh = vec![0f32; CHUNK_FRAMES * ch];
        let mut frames_done: u64 = 0;
        'clip: while frames_done < want_frames {
            if cancel.load(Ordering::Relaxed) {
                children.kill(id);
                return Err("abgebrochen".into());
            }
            let want_now = ((want_frames - frames_done) as usize).min(CHUNK_FRAMES);
            let want_bytes = want_now * ch * 4;
            // Block vollständig lesen (EOF beendet den Clip vorzeitig — Rest bleibt Stille).
            let mut filled = 0;
            while filled < want_bytes {
                match stdout.read(&mut decoded[filled..want_bytes]) {
                    Ok(0) => break,
                    Ok(n) => filled += n,
                    Err(e) => {
                        children.kill(id);
                        return Err(format!("Audio-Decoder ({}): {e}", clip.path));
                    }
                }
            }
            let got_frames = filled / (ch * 4);
            if got_frames == 0 {
                break 'clip;
            }
            for (i, s) in fresh[..got_frames * ch].iter_mut().enumerate() {
                let off = i * 4;
                *s = f32::from_le_bytes([
                    decoded[off],
                    decoded[off + 1],
                    decoded[off + 2],
                    decoded[off + 3],
                ]);
            }
            let byte_pos = data_off + (offset_frames + frames_done) * ch as u64 * 4;
            let block = &mut existing[..got_frames * ch * 4];
            file.seek(SeekFrom::Start(byte_pos)).map_err(|e| e.to_string())?;
            file.read_exact(block).map_err(|e| format!("Mix-Lesen: {e}"))?;
            let mut fi = 0usize;
            while fi < got_frames {
                let n = ENV_BLOCK.min(got_frames - fi);
                let media_t =
                    clip.src_in + (frames_done + fi as u64) as f64 / rate as f64 * clip.speed;
                if let Some(chain) = fx_chain.as_mut() {
                    if fx_animated {
                        chain.retune(&fx_refs, media_t);
                    }
                    chain.process(&mut fresh[fi * ch..(fi + n) * ch]);
                }
                let env = if has_envelope {
                    // Crossfade-Hüllkurve in Mix-Zeit (identische Kurven
                    // wie der Player-Mixdown).
                    let mix_t =
                        clip.start_in_mix + (frames_done + fi as u64) as f64 / rate as f64;
                    let fade: f64 = clip
                        .fades
                        .iter()
                        .map(|f| f.gain_at(mix_t))
                        .product();
                    db_to_linear(clip.volume.eval(media_t)) * fade as f32
                } else {
                    1.0
                };
                for i in fi * ch..(fi + n) * ch {
                    let gain = env * if ch == 2 { gains[i % 2] } else { mono_gain };
                    let off = i * 4;
                    let old = f32::from_le_bytes([
                        block[off],
                        block[off + 1],
                        block[off + 2],
                        block[off + 3],
                    ]);
                    let sum = old + fresh[i] * gain;
                    block[off..off + 4].copy_from_slice(&sum.to_le_bytes());
                }
                fi += n;
            }
            file.seek(SeekFrom::Start(byte_pos)).map_err(|e| e.to_string())?;
            file.write_all(block).map_err(|e| format!("Mix-Schreiben: {e}"))?;
            frames_done += got_frames as u64;
            progress.advance(got_frames as u64);
            if filled < want_bytes {
                break 'clip; // EOF des Decoders
            }
        }
        // Nicht gelieferte Samples als erledigt verbuchen (Fortschritt stimmt).
        progress.advance(want_frames.saturating_sub(frames_done));
        children.kill(id);
    }
    file.sync_all().ok();
    Ok(())
}

/// Eine Spur MIT Bus-FX/Automation verarbeiten: Clips in eine temporäre WAV
/// summieren, diese blockweise durch die Bus-Effektkette schicken, Spur-
/// Gain/Pan (inkl. Automation, Sequenzzeit) und Master anwenden und additiv
/// in die Master-WAV mischen. Identische DSP-Kette (`AudioFxChain`) und
/// Gain-Mathematik wie der Player → Wiedergabe und Export klingen gleich.
#[allow(clippy::too_many_arguments)]
fn process_audio_track(
    master: &mut std::fs::File,
    data_off: u64,
    total_frames: u64,
    rate: u32,
    ch: usize,
    base_wav: &Path,
    idx: usize,
    track: &AudioTrackPlan,
    cancel: &AtomicBool,
    children: &mut ChildRegistry,
    progress: &mut Progress,
) -> Result<(), String> {
    let data_bytes = total_frames * ch as u64 * 4;
    let tmp = base_wav.with_extension(format!("track{idx}.wav"));
    let mut run = || -> Result<(), String> {
        // 1. Clips der Spur in die Temp-WAV (nur Clip-Gain).
        let (mut tfile, toff) = create_silent_wav(&tmp, rate, ch, data_bytes)?;
        mix_clips_into_wav(
            &mut tfile, toff, total_frames, rate, ch, &track.clips, cancel, children, progress,
        )?;

        // 2. Bus-FX + Spur-Gain/Pan + Master, blockweise, in den Master.
        let master_lin = db_to_linear(track.master_db);
        let fx_refs: Vec<&EffectInstance> = track.effects.iter().collect();
        let mut fx_chain = AudioFxChain::build(&fx_refs, rate, ch, track.seq_start);
        let fx_animated = track.effects.iter().any(|e| e.any_animated());
        const ENV_BLOCK: usize = 256;
        const CHUNK_FRAMES: usize = 32768;
        let mut tbuf = vec![0u8; CHUNK_FRAMES * ch * 4];
        let mut mbuf = vec![0u8; CHUNK_FRAMES * ch * 4];
        let mut fresh = vec![0f32; CHUNK_FRAMES * ch];
        let mut frames_done: u64 = 0;
        while frames_done < total_frames {
            if cancel.load(Ordering::Relaxed) {
                return Err("abgebrochen".into());
            }
            let now = ((total_frames - frames_done) as usize).min(CHUNK_FRAMES);
            let bytes = now * ch * 4;
            let tpos = toff + frames_done * ch as u64 * 4;
            tfile.seek(SeekFrom::Start(tpos)).map_err(|e| e.to_string())?;
            tfile
                .read_exact(&mut tbuf[..bytes])
                .map_err(|e| format!("Spur-Lesen: {e}"))?;
            for i in 0..now * ch {
                let off = i * 4;
                fresh[i] =
                    f32::from_le_bytes([tbuf[off], tbuf[off + 1], tbuf[off + 2], tbuf[off + 3]]);
            }
            let mpos = data_off + frames_done * ch as u64 * 4;
            master.seek(SeekFrom::Start(mpos)).map_err(|e| e.to_string())?;
            master
                .read_exact(&mut mbuf[..bytes])
                .map_err(|e| format!("Mix-Lesen: {e}"))?;
            let mut fi = 0usize;
            while fi < now {
                let n = ENV_BLOCK.min(now - fi);
                let mix_t = (frames_done + fi as u64) as f64 / rate as f64;
                if let Some(chain) = fx_chain.as_mut() {
                    if fx_animated {
                        chain.retune(&fx_refs, track.seq_start + mix_t);
                    }
                    chain.process(&mut fresh[fi * ch..(fi + n) * ch]);
                }
                // Spur-Gain/Pan inkl. Automation (Sequenzzeit) × Master.
                let g = db_to_linear(track.gain_db_at(mix_t));
                let (pl, pr) = pan_gains(track.pan_at(mix_t));
                let (gl, gr) = (g * pl * master_lin, g * pr * master_lin);
                let mono = (gl + gr) * 0.5;
                for i in fi * ch..(fi + n) * ch {
                    let gain = if ch == 2 {
                        if i % 2 == 0 {
                            gl
                        } else {
                            gr
                        }
                    } else {
                        mono
                    };
                    let off = i * 4;
                    let old =
                        f32::from_le_bytes([mbuf[off], mbuf[off + 1], mbuf[off + 2], mbuf[off + 3]]);
                    let sum = old + fresh[i] * gain;
                    mbuf[off..off + 4].copy_from_slice(&sum.to_le_bytes());
                }
                fi += n;
            }
            master.seek(SeekFrom::Start(mpos)).map_err(|e| e.to_string())?;
            master
                .write_all(&mbuf[..bytes])
                .map_err(|e| format!("Mix-Schreiben: {e}"))?;
            frames_done += now as u64;
            progress.advance(now as u64);
        }
        Ok(())
    };
    let result = run();
    let _ = std::fs::remove_file(&tmp);
    result
}

// ----------------------------------------------------------- Video-Render

/// Ausgabe-Farbraum des Exports (ehrliche Tags + korrekte RGB→YUV-Matrix).
/// Wird aus dem dominanten Quellmaterial erkannt; SDR-Default ist BT.709.
/// HDR-Quellen (PQ/HLG) und BT.2020 werden durchgereicht statt nach 709
/// fehlgetaggt — „10-Bit-HDR-Material wird nicht mehr zerstört". Vollständiges
/// HDR-Grading bleibt ausgeklammert (die Korrektur rechnet weiter in 709-Gamma).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum OutputColor {
    #[default]
    Bt709,
    /// BT.2020, SDR-Transfer (Wide-Gamut-Quelle ohne HDR-Kurve).
    Bt2020,
    /// BT.2020 + PQ (SMPTE ST 2084) — HDR10.
    Bt2020Pq,
    /// BT.2020 + HLG (ARIB STD-B67).
    Bt2020Hlg,
}

impl OutputColor {
    /// Aus den ffprobe-Farbtags eines Streams ableiten (siehe
    /// `VideoStreamInfo`). Unbekannt/leer ⇒ BT.709.
    pub fn from_stream(s: &crate::core::types::VideoStreamInfo) -> OutputColor {
        let trc = s.color_transfer.as_deref().unwrap_or("").to_ascii_lowercase();
        let prim = s.color_primaries.as_deref().unwrap_or("").to_ascii_lowercase();
        let space = s.color_space.as_deref().unwrap_or("").to_ascii_lowercase();
        if trc.contains("2084") || trc.contains("pq") {
            return OutputColor::Bt2020Pq;
        }
        if trc.contains("b67") || trc.contains("hlg") {
            return OutputColor::Bt2020Hlg;
        }
        if prim.contains("2020") || space.contains("2020") {
            return OutputColor::Bt2020;
        }
        OutputColor::Bt709
    }

    /// ffmpeg-Tags (color_primaries, color_trc, colorspace).
    pub fn tags(self) -> (&'static str, &'static str, &'static str) {
        match self {
            OutputColor::Bt709 => ("bt709", "bt709", "bt709"),
            OutputColor::Bt2020 => ("bt2020", "bt2020-10", "bt2020nc"),
            OutputColor::Bt2020Pq => ("bt2020", "smpte2084", "bt2020nc"),
            OutputColor::Bt2020Hlg => ("bt2020", "arib-std-b67", "bt2020nc"),
        }
    }

    /// Matrix für die RGB→YUV-Wandlung im `scale`-Filter.
    pub fn scale_matrix(self) -> &'static str {
        match self {
            OutputColor::Bt709 => "bt709",
            _ => "bt2020nc",
        }
    }

    /// HDR-Transfer (PQ/HLG)? Solche Quellen werden für die SDR-Vorschau
    /// tone-gemappt (`core/player.rs`).
    pub fn is_hdr(self) -> bool {
        matches!(self, OutputColor::Bt2020Pq | OutputColor::Bt2020Hlg)
    }
}

/// Encoder-Argumente für den Video-Codec (pure Funktion, testbar). Wählt das
/// passende Qualitäts-Flag je Encoder-Backend (CRF/CQ/global_quality/QP/
/// Bitrate) und behandelt VAAPI (Render-Device + `hwupload`) gesondert.
/// `color` = erkannter Ausgabe-Farbraum (ehrliche Tags + passende Matrix).
pub fn video_codec_args(
    v: &VideoSettings,
    container: &ContainerDef,
    color: OutputColor,
) -> Vec<String> {
    let vaapi = v.encoder.vaapi;
    let mut args: Vec<String> = Vec::new();
    // VAAPI braucht das Render-Device, bevor der Encoder initialisiert wird.
    if vaapi {
        args.extend(["-vaapi_device".into(), vaapi_device()]);
    }
    args.extend(["-c:v".into(), v.encoder.id.into()]);
    let mut pix_fmt = v.codec.pix_fmt;
    match v.codec.quality {
        QualityKind::Profiles(profiles) => {
            let (arg, _, fmt) = profiles[v.profile.min(profiles.len() - 1)];
            args.extend(["-profile:v".into(), arg.into()]);
            pix_fmt = fmt;
            if v.codec.id == "prores" {
                args.extend(["-vendor".into(), "apl0".into()]);
            }
        }
        QualityKind::CrfOrBitrate { .. } => {
            args.extend(quality_args(v));
        }
    }
    // 10-Bit-Schalter für CRF/Bitrate-Codecs (Software-Pfad): höheres
    // Pixelformat + codec-spezifisches 10-Bit-Profil. VAAPI bleibt 8-Bit
    // (nv12), da Hardware-10-Bit encoderspezifisch ist.
    if v.tenbit && !vaapi {
        if let Some(fmt) = codec_tenbit_pix_fmt(v.codec.id) {
            pix_fmt = fmt;
            match v.codec.id {
                "hevc" => args.extend(["-profile:v".into(), "main10".into()]),
                "h264" => args.extend(["-profile:v".into(), "high10".into()]),
                "vp9" => args.extend(["-profile:v".into(), "2".into()]),
                _ => {} // AV1: yuv420p10le genügt, kein Profil-Flag nötig.
            }
        }
    }
    // Encoder-Tempo: nur Software-Encoder verstehen die x264/x265/SVT-Presets.
    if !v.encoder.is_hardware() && !v.codec.speed_presets.is_empty() {
        let preset = v.codec.speed_presets[v.speed.min(v.codec.speed_presets.len() - 1)];
        args.extend(["-preset".into(), preset.into()]);
    }
    if v.codec.id == "vp9" {
        args.extend(["-row-mt".into(), "1".into(), "-deadline".into(), "good".into()]);
    }
    if v.codec.id == "hevc" && matches!(container.id, "mp4" | "mov") {
        // Apple-Player erwarten hvc1 statt hev1.
        args.extend(["-tag:v".into(), "hvc1".into()]);
    }
    // RGBA → Ziel-Farbraum wandeln + ehrlich taggen (BT.709 oder, bei
    // erkanntem Wide-Gamut/HDR-Material, BT.2020 (+ PQ/HLG) durchgereicht).
    // VAAPI lädt zusätzlich in eine GPU-Surface.
    let (prim, trc, space) = color.tags();
    let mat = color.scale_matrix();
    if vaapi {
        args.extend([
            "-vf".into(),
            format!("scale=out_color_matrix={mat}:out_range=tv,format=nv12,hwupload"),
        ]);
    } else {
        args.extend(["-pix_fmt".into(), pix_fmt.into()]);
        args.extend(["-vf".into(), format!("scale=out_color_matrix={mat}:out_range=tv")]);
    }
    args.extend([
        "-color_primaries".into(),
        prim.into(),
        "-color_trc".into(),
        trc.into(),
        "-colorspace".into(),
        space.into(),
    ]);
    args
}

/// Qualitäts-Flags für CRF-/Bitrate-Codecs je nach Encoder-Backend.
fn quality_args(v: &VideoSettings) -> Vec<String> {
    match v.quality {
        VideoQuality::Bitrate(kbps) => vec!["-b:v".into(), format!("{kbps}k")],
        VideoQuality::Crf(val) => match v.encoder.quality {
            EncoderQuality::Crf => {
                let mut a = vec!["-crf".into(), val.to_string()];
                if v.codec.id == "vp9" {
                    // libvpx: CRF wirkt nur mit -b:v 0.
                    a.extend(["-b:v".into(), "0".into()]);
                }
                a
            }
            EncoderQuality::Cq(..) => {
                // NVENC: konstante Qualität über VBR mit cq + b:v 0.
                vec![
                    "-rc".into(), "vbr".into(),
                    "-cq".into(), val.to_string(),
                    "-b:v".into(), "0".into(),
                ]
            }
            EncoderQuality::GlobalQuality(..) => {
                // Intel QSV: ICQ-ähnlich über global_quality.
                vec!["-global_quality".into(), val.to_string()]
            }
            EncoderQuality::Qp(..) => {
                // VAAPI: konstante Quantisierung.
                vec!["-rc_mode".into(), "CQP".into(), "-qp".into(), val.to_string()]
            }
            EncoderQuality::BitrateOnly => {
                // VideoToolbox kennt kein CRF — sinnvoller Bitrate-Fallback.
                vec!["-b:v".into(), "12000k".into()]
            }
        },
    }
}

/// Audio-Encoder-Argumente (pure Funktion, testbar).
pub fn audio_codec_args(a: &AudioSettings) -> Vec<String> {
    let mut args: Vec<String> = vec!["-c:a".into(), a.codec.encoder.into()];
    if !a.codec.bitrates.is_empty() {
        args.extend(["-b:a".into(), format!("{}k", a.bitrate_kbps)]);
    }
    args
}

// ------------------------------------------------------- Bild-Sequenz-Export

/// Encoder-Argumente für die Bild-Sequenz-Codecs (PNG/JPEG/TIFF). PNG/TIFF
/// sind verlustfrei (RGB), JPEG nutzt das CRF-Feld als `-q:v`-Qualität.
pub fn image_codec_args(v: &VideoSettings) -> Vec<String> {
    match v.codec.id {
        "mjpeg" => {
            let q = match v.quality {
                VideoQuality::Crf(q) => q.clamp(2, 31),
                VideoQuality::Bitrate(_) => 3,
            };
            vec![
                "-c:v".into(), "mjpeg".into(),
                "-q:v".into(), q.to_string(),
                "-pix_fmt".into(), "yuvj420p".into(),
            ]
        }
        "tiff" => vec!["-c:v".into(), "tiff".into(), "-pix_fmt".into(), "rgb24".into()],
        // PNG (Standard): verlustfrei mit Alpha.
        _ => vec!["-c:v".into(), "png".into(), "-pix_fmt".into(), "rgba".into()],
    }
}

/// ffmpeg-Encoder-Argumente für einen Einzel-Frame-Export nach Endung
/// (Programmmonitor-Kamera). Muxer ist immer `image2`.
pub fn frame_export_args(ext: &str) -> Vec<String> {
    match ext.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => vec![
            "-c:v".into(), "mjpeg".into(),
            "-q:v".into(), "2".into(),
            "-pix_fmt".into(), "yuvj420p".into(),
            "-frames:v".into(), "1".into(),
        ],
        "tif" | "tiff" => vec![
            "-c:v".into(), "tiff".into(),
            "-pix_fmt".into(), "rgb24".into(),
            "-frames:v".into(), "1".into(),
        ],
        // PNG (Standard): verlustfrei mit Alpha.
        _ => vec![
            "-c:v".into(), "png".into(),
            "-pix_fmt".into(), "rgba".into(),
            "-frames:v".into(), "1".into(),
        ],
    }
}

/// Zielmuster einer Bild-Sequenz: `<verzeichnis>/<stamm>_%06d.<ext>` —
/// abgeleitet aus dem gewählten Basis-Pfad (`/dir/name.png`).
pub fn image_sequence_pattern(output: &str) -> String {
    let path = Path::new(output);
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_else(|| "png".into());
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "frame".into());
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    dir.join(format!("{stem}_%06d.{ext}")).to_string_lossy().into_owned()
}

/// Rendert eine Bild-Sequenz (nur Video-Phase). Atomar: erst in ein temporäres
/// Unterverzeichnis schreiben, bei Erfolg die fertigen Frames an den Zielort
/// verschieben — ein Abbruch/Fehler hinterlässt keine halbe Sequenz.
fn render_image_sequence(
    job_id: &str,
    plan: &RenderPlan,
    settings: &ExportSettings,
    cancel: &AtomicBool,
    children: &mut ChildRegistry,
    progress: &mut Progress,
) -> Result<(), String> {
    let video = settings.video.as_ref().ok_or("Bild-Sequenz braucht Video-Einstellungen")?;
    let fps = fps_arg(video.fps);
    let out_path = Path::new(&settings.output);
    let dir = out_path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .ok_or("Zielordner der Bild-Sequenz ist ungültig")?;
    let ext = out_path
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_else(|| settings.container.ext.into());
    let stem = out_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "frame".into());

    // Temporäres Unterverzeichnis (gleiche Partition ⇒ rename ist atomar/billig).
    let tmp_dir = dir.join(format!(".editron-seq-{job_id}"));
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir)
        .map_err(|e| format!("Temporäres Sequenz-Verzeichnis fehlgeschlagen: {e}"))?;
    let tmp_pattern = tmp_dir.join(format!("f_%06d.{ext}"));

    let mut cmd = Command::new(crate::services::ffmpeg_bin());
    cmd.args(["-y", "-v", "error"])
        .args(["-f", "rawvideo", "-pixel_format", pipe_pix_fmt(video)])
        .args(["-video_size", &format!("{}x{}", video.width, video.height)])
        .args(["-framerate", &fps])
        .args(["-i", "pipe:0"])
        .arg("-an")
        .args(image_codec_args(video))
        .args(["-start_number", &settings.image_start.to_string()])
        .args(["-f", "image2"])
        .arg(&tmp_pattern)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let (enc_id, stdin, _, stderr) = children.spawn(&mut cmd)?;
    let mut enc_in = stdin.ok_or("Encoder-stdin nicht verfügbar")?;
    let mut stderr = stderr.ok_or("Encoder-stderr nicht verfügbar")?;
    let stderr_task = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });

    let write_err = render_segments(plan, video, &fps, &mut enc_in, cancel, children, progress).err();
    drop(enc_in);
    let status = children.wait(enc_id);
    let stderr_buf = stderr_task.join().unwrap_or_default();

    let finish = (|| -> Result<(), String> {
        if cancel.load(Ordering::Relaxed) {
            return Err("abgebrochen".into());
        }
        let ok = status.map(|s| s.success()).unwrap_or(false);
        if !ok || write_err.is_some() {
            let tail = stderr_tail(&stderr_buf);
            let detail = if tail.is_empty() {
                write_err.clone().unwrap_or_else(|| "Encoder ohne Fehlermeldung beendet".into())
            } else {
                tail
            };
            return Err(format!("Bild-Sequenz-Encoder fehlgeschlagen: {detail}"));
        }
        // Fertige Frames an den Zielort verschieben: f_000123.ext → stem_000123.ext.
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&tmp_dir)
            .map_err(|e| format!("Sequenz-Verzeichnis konnte nicht gelesen werden: {e}"))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file())
            .collect();
        entries.sort();
        for src in &entries {
            let num = src
                .file_stem()
                .and_then(|s| s.to_str())
                .and_then(|s| s.strip_prefix("f_"))
                .unwrap_or("000000");
            let dst = dir.join(format!("{stem}_{num}.{ext}"));
            std::fs::rename(src, &dst).map_err(|e| {
                format!("Sequenz-Bild konnte nicht finalisiert werden ({}): {e}", dst.display())
            })?;
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&tmp_dir);
    finish
}

#[allow(clippy::too_many_arguments)]
fn render_video(
    plan: &RenderPlan,
    settings: &ExportSettings,
    wav: Option<&Path>,
    subs: &[PathBuf],
    part: &Path,
    cancel: &AtomicBool,
    children: &mut ChildRegistry,
    progress: &mut Progress,
) -> Result<(), String> {
    let video = settings.video.as_ref().expect("video settings");
    let fps = fps_arg(video.fps);

    // ---- Encoder-Prozess ----
    let mut cmd = Command::new(crate::services::ffmpeg_bin());
    cmd.args(["-y", "-v", "error"])
        .args(["-f", "rawvideo", "-pixel_format", pipe_pix_fmt(video)])
        .args(["-video_size", &format!("{}x{}", video.width, video.height)])
        .args(["-framerate", &fps])
        .args(["-i", "pipe:0"]);
    if let Some(wav) = wav {
        cmd.args(["-i", &wav.to_string_lossy()]);
    }
    for sub in subs {
        cmd.args(["-i", &sub.to_string_lossy()]);
    }
    cmd.args(["-map", "0:v:0"]);
    if wav.is_some() {
        cmd.args(["-map", "1:a:0"]);
    }
    // Untertitel-Streams hinter Video/Audio mappen (Input-Reihenfolge).
    let sub_base = 1 + usize::from(wav.is_some());
    for i in 0..subs.len() {
        cmd.args(["-map", &format!("{}:s:0", sub_base + i)]);
    }
    cmd.args(video_codec_args(video, settings.container, plan.color));
    if let (Some(_), Some(a)) = (wav, settings.audio.as_ref()) {
        cmd.args(audio_codec_args(a));
    }
    if !subs.is_empty() {
        if let Some(codec) = settings.container.subtitle_codec {
            cmd.args(["-c:s", codec]);
        }
        for (i, track) in plan.subtitle_tracks.iter().enumerate().take(subs.len()) {
            cmd.arg(format!("-metadata:s:s:{i}"))
                .arg(format!("title={}", track.name));
        }
    }
    if settings.container.faststart {
        cmd.args(["-movflags", "+faststart"]);
    }
    // Muxer explizit — die .part-Zwischendatei hat keine Format-Endung.
    cmd.args(["-f", settings.container.muxer]);
    cmd.arg(part)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let (enc_id, stdin, _, stderr) = children.spawn(&mut cmd)?;
    let mut enc_in = stdin.ok_or("Encoder-stdin nicht verfügbar")?;
    let mut stderr = stderr.ok_or("Encoder-stderr nicht verfügbar")?;
    // stderr nebenläufig leeren, sonst blockiert ffmpeg an der vollen Pipe.
    let stderr_task = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });

    // ---- Segmente sequenziell in die Encoder-Pipe pumpen ----
    let write_err = render_segments(plan, video, &fps, &mut enc_in, cancel, children, progress).err();

    // ---- Encoder abschließen ----
    drop(enc_in); // EOF → Encoder finalisiert den Container
    let status = children.wait(enc_id);
    let stderr_buf = stderr_task.join().unwrap_or_default();
    if cancel.load(Ordering::Relaxed) {
        return Err("abgebrochen".into());
    }
    let ok = status.map(|s| s.success()).unwrap_or(false);
    if !ok || write_err.is_some() {
        let tail = stderr_tail(&stderr_buf);
        let detail = if tail.is_empty() {
            write_err.unwrap_or_else(|| "Encoder ohne Fehlermeldung beendet".into())
        } else {
            tail
        };
        return Err(format!("Video-Encoder fehlgeschlagen: {detail}"));
    }
    Ok(())
}

/// Renderplan für einen Sequenz-Frame-Bereich `[start_frame, end_frame)` —
/// VIDEO-ONLY (kein Audio/Untertitel-Mux). Auf dem Main-Thread bauen (greift
/// auf Timeline/Medien zu) und den OWNED Plan an [`render_cache_plan`]
/// übergeben, das ihn im Hintergrund-Thread rendert (entkoppelt wie der
/// Voll-Export von [`build_render_plan`]).
pub fn build_cache_plan(
    timeline: &TimelineStore,
    media: &MediaStore,
    width: u32,
    height: u32,
    fps: f64,
    start_frame: u64,
    end_frame: u64,
) -> RenderPlan {
    let total_frames = end_frame.saturating_sub(start_frame).max(1);
    let start_sec = start_frame as f64 / fps.max(1.0);
    let solo_any = timeline.tracks.iter().any(|t| t.solo);
    let segments =
        plan_video_segments(timeline, media, &NoNests, start_sec, total_frames, fps, solo_any, false);
    let duration = total_frames as f64 / fps.max(1.0);
    RenderPlan {
        duration,
        width,
        height,
        fps,
        total_frames,
        segments,
        audio: Vec::new(),
        audio_tracks: Vec::new(),
        subtitle_tracks: Vec::new(),
        nests: HashMap::new(),
        nest_media: HashMap::new(),
        color: detect_output_color(timeline, media, start_sec, start_sec + duration),
    }
}

/// Einen Cache-Renderplan über den Compositing-Kern ([`render_segments`]) in
/// eine Intra-Frame-Cache-Datei rendern — ohne Audio. Reiner CPU-Pfad, im
/// Hintergrund-Thread sicher (kein GL-Kontext). `encode_args`/`muxer` bestimmen
/// den Cache-Codec (ProRes Proxy o. Ä.). Schreibt erst in `<out>.part` und
/// benennt bei Erfolg atomar um. `on_progress` wird gedrosselt mit
/// `(done, total)` aufgerufen.
#[allow(clippy::too_many_arguments)]
pub fn render_cache_plan(
    plan: &RenderPlan,
    encode_args: &[String],
    muxer: &str,
    out: &Path,
    cancel: &AtomicBool,
    children: ChildList,
    on_progress: &mut dyn FnMut(u64, u64),
) -> Result<(), String> {
    let mut children = ChildRegistry::new(children);
    let children = &mut children;
    let (width, height, fps) = (plan.width, plan.height, plan.fps);
    let total_frames = plan.total_frames;
    // Platzhalter-VideoSettings: `render_segments`/`render_segment_composited`
    // lesen daraus nur width/height/fps (das Compositing-Ziel) — der Codec
    // wird vom Cache-Encoder unten gesetzt, nicht hierüber.
    let video = VideoSettings {
        codec: video_codec("h264"),
        encoder: &video_codec("h264").encoders[0],
        width,
        height,
        fps,
        quality: VideoQuality::Crf(0),
        speed: 0,
        profile: 0,
        tenbit: false,
    };

    let fps_s = fps_arg(fps);
    let part = out.with_extension("part");
    let mut cmd = Command::new(crate::services::ffmpeg_bin());
    cmd.args(["-y", "-v", "error"])
        .args(["-f", "rawvideo", "-pixel_format", pipe_pix_fmt(&video)])
        .args(["-video_size", &format!("{width}x{height}")])
        .args(["-framerate", &fps_s])
        .args(["-i", "pipe:0"])
        .arg("-an");
    for a in encode_args {
        cmd.arg(a);
    }
    cmd.args(["-f", muxer])
        .arg(&part)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let (enc_id, stdin, _, stderr) = children.spawn(&mut cmd)?;
    let mut enc_in = stdin.ok_or("Cache-Encoder-stdin nicht verfügbar")?;
    let mut stderr = stderr.ok_or("Cache-Encoder-stderr nicht verfügbar")?;
    let stderr_task = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });

    let write_err = {
        let mut progress = CountProgress::new(total_frames, on_progress);
        render_segments(plan, &video, &fps_s, &mut enc_in, cancel, children, &mut progress).err()
    };
    drop(enc_in); // EOF → Encoder finalisiert die Cache-Datei

    let status = children.wait(enc_id);
    let stderr_buf = stderr_task.join().unwrap_or_default();
    if cancel.load(Ordering::Relaxed) {
        let _ = std::fs::remove_file(&part);
        return Err("abgebrochen".into());
    }
    let ok = status.map(|s| s.success()).unwrap_or(false);
    if !ok || write_err.is_some() {
        let tail = stderr_tail(&stderr_buf);
        let _ = std::fs::remove_file(&part);
        let detail = if tail.is_empty() {
            write_err.unwrap_or_else(|| "Cache-Encoder ohne Fehlermeldung beendet".into())
        } else {
            tail
        };
        return Err(format!("Render-Cache fehlgeschlagen: {detail}"));
    }
    std::fs::rename(&part, out)
        .map_err(|e| format!("Cache-Datei konnte nicht finalisiert werden: {e}"))?;
    Ok(())
}

/// Pumpt alle Segmente eines Renderplans als rohe RGBA-Frames in eine
/// Encoder-Pipe (`enc_in`). Das ist der gemeinsame Compositing-Kern von
/// Voll-Export UND Sequenz-Render-Cache — drei Pfade je Segment: Lücke
/// (Schwarz), Schnellpfad (genau ein Layer ohne Transformation, direkt von
/// ffmpeg skaliert/pad't) und voller CPU-Compositing-Pfad. Liefert `Err` nur
/// bei echtem Schreib-/Compositing-Fehler; ein Abbruch (`cancel`) endet
/// stillschweigend mit `Ok(())` (der Aufrufer prüft `cancel` selbst).
fn render_segments(
    plan: &RenderPlan,
    video: &VideoSettings,
    fps: &str,
    enc_in: &mut std::process::ChildStdin,
    cancel: &AtomicBool,
    children: &mut ChildRegistry,
    progress: &mut dyn FrameProgress,
) -> Result<(), String> {
    // Pipe-Format: 8 Bit (rgba) oder 16 Bit (rgba64le) je nach Ziel-Bittiefe.
    // `frame_size` ist die Bytegröße EINES Frames in der Encoder-Pipe.
    let bpp = pipe_bytes_per_px(video);
    let pipe_fmt = pipe_pix_fmt(video);
    let frame_size = video.width as usize * video.height as usize * bpp;
    let black: Vec<u8> = {
        let mut px = vec![0u8; frame_size];
        for p in px.chunks_exact_mut(bpp) {
            // Alpha opak: rgba8 ⇒ Byte 3 = 255; rgba64le ⇒ u16-Alpha = 65535.
            if bpp == 8 {
                p[6] = 255;
                p[7] = 255;
            } else {
                p[3] = 255;
            }
        }
        px
    };

    // Verschachtelte Sequenzen einmal in renderbare Timelines überführen.
    let nest_ctx = NestRenderCtx::from_plan(plan);

    let mut write_err: Option<String> = None;
    // Laufender Frame-Cursor: Exportzeit der Segmente (Übergangs-Fenster).
    let mut frame_cursor: u64 = 0;
    'segments: for segment in &plan.segments {
        let seg_start_frame = frame_cursor;
        frame_cursor += segment.frames;
        // Lücke → Schwarzbild.
        if segment.layers.is_empty() {
            for _ in 0..segment.frames {
                if cancel.load(Ordering::Relaxed) {
                    break 'segments;
                }
                if let Err(e) = enc_in.write_all(&black) {
                    write_err = Some(e.to_string());
                    break 'segments;
                }
                progress.advance(1);
            }
            continue;
        }

        // Schnellpfad: ein Layer ohne Transformation — ffmpeg skaliert/pad't
        // direkt in die Encoder-Pipe (kein CPU-Compositing nötig).
        if segment.layers.len() == 1 && segment.layers[0].is_identity() {
            let layer = &segment.layers[0];
            let freeze = !layer.image && layer.media_step == 0.0;
            let mut cmd = Command::new(crate::services::ffmpeg_bin());
            cmd.args(["-v", "error"]);
            if layer.image {
                cmd.args(["-loop", "1", "-framerate", fps]);
            } else {
                cmd.args(["-ss", &format!("{:.4}", layer.src_in)]);
            }
            cmd.args(["-i", &layer.path]).args(["-an", "-sn"]);
            // Konstante Geschwindigkeit über dieselbe setpts/fps-Kette wie
            // Vorschau und Compositing-Pfad (identische Frame-Auswahl);
            // Standbild: einen Frame dekodieren, Halte-Logik füllt den Rest.
            let setpts = if layer.image || freeze {
                String::new()
            } else {
                speed_setpts_filter(layer.media_step)
            };
            let filter = format!(
                "{setpts}fps={fps},scale={w}:{h}:force_original_aspect_ratio=decrease:flags=bicubic,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:color=black",
                w = video.width,
                h = video.height
            );
            let dec_frames = if freeze { 1 } else { segment.frames };
            cmd.args(["-vf", &filter])
                .args(["-frames:v", &dec_frames.to_string()])
                .args(["-f", "rawvideo", "-pix_fmt", pipe_fmt])
                .arg("pipe:1")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            let (dec_id, _, stdout, _) = children.spawn(&mut cmd)?;
            let mut dec_out = stdout.ok_or("Decoder-stdout nicht verfügbar")?;

            let mut frame = vec![0u8; frame_size];
            let mut last_frame: Option<Vec<u8>> = None;
            let mut decoder_dead = false;
            for _ in 0..segment.frames {
                if cancel.load(Ordering::Relaxed) {
                    children.kill(dec_id);
                    break 'segments;
                }
                let buf: &[u8] = if decoder_dead {
                    last_frame.as_deref().unwrap_or(&black)
                } else {
                    let mut filled = 0;
                    while filled < frame_size {
                        match dec_out.read(&mut frame[filled..]) {
                            Ok(0) => break,
                            Ok(n) => filled += n,
                            Err(_) => break,
                        }
                    }
                    if filled == frame_size {
                        last_frame = Some(frame.clone());
                        &frame
                    } else {
                        // Decoder liefert weniger als geplant (Quelle kürzer,
                        // defekte Datei): letzten Frame halten statt abbrechen.
                        decoder_dead = true;
                        last_frame.as_deref().unwrap_or(&black)
                    }
                };
                if let Err(e) = enc_in.write_all(buf) {
                    write_err = Some(e.to_string());
                    children.kill(dec_id);
                    break 'segments;
                }
                progress.advance(1);
            }
            children.kill(dec_id);
            continue;
        }

        // Compositing-Pfad: ein Decoder je Layer, CPU mischt jeden Frame.
        match render_segment_composited(
            segment,
            seg_start_frame,
            video,
            fps,
            enc_in,
            cancel,
            children,
            &nest_ctx,
            progress,
        ) {
            Ok(()) => {}
            Err(CompErr::Cancelled) => break 'segments,
            Err(CompErr::Failed(e)) => {
                write_err = Some(e);
                break 'segments;
            }
        }
    }

    match write_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

enum CompErr {
    Cancelled,
    Failed(String),
}

/// Frame-Quelle eines Compositing-Layers: laufende Decoder-Pipe (vorwärts)
/// oder chunkweises Rückwärts-Dekodieren mit Frame-Puffer.
enum LayerSource {
    Pipe { dec_id: u64, out: ChildStdout },
    Reverse(ReverseDecode),
}

/// Rückwärts-Wiedergabe: ffmpeg streamt nur vorwärts — Chunks VOR der
/// Zielzeit werden vorwärts dekodiert (identische setpts/fps-Kette wie der
/// Vorwärtspfad ⇒ identische Frame-Auswahl) und rückwärts ausgeliefert.
struct ReverseDecode {
    path: String,
    /// Komplette -vf-Kette (setpts + fps + scale + pad).
    filter: String,
    /// Pipe-Pixelformat der Quelle ("rgba" = 4 B/px bzw. "rgba64le" = 8 B/px bei
    /// >8 Bit). MUSS zum `src_bpp`-dimensionierten `frame_size` passen, sonst
    /// > liest der Decoder Müll (10-Bit-Reverse-Export wäre sonst korrupt/schwarz).
    src_fmt: String,
    /// Medienzeit des NÄCHSTEN auszugebenden Frames (läuft abwärts).
    media_next: f64,
    /// Medien-Sekunden pro Ausgabeframe (|media_step| / fps).
    step: f64,
    chunk_frames: usize,
    /// Gepufferte Frames in Ausgabe-Reihenfolge (Medienzeit absteigend).
    buf: std::collections::VecDeque<Vec<u8>>,
    exhausted: bool,
}

impl ReverseDecode {
    /// Nächsten Chunk synchron dekodieren (Worker-Thread; Abbruch killt die
    /// Kindprozesse über die Registry und beendet die Reads).
    fn refill(&mut self, children: &mut ChildRegistry, frame_size: usize) {
        if self.exhausted {
            return;
        }
        if self.media_next < -0.5 * self.step {
            self.exhausted = true;
            return;
        }
        let top = self.media_next.max(0.0);
        let want = self.chunk_frames.max(1);
        let lo = (top - (want as f64 - 1.0) * self.step).max(0.0);
        let n = (((top - lo) / self.step.max(1e-9)).round() as usize) + 1;
        let mut cmd = Command::new(crate::services::ffmpeg_bin());
        cmd.args(["-v", "error", "-ss", &format!("{lo:.4}")])
            .args(["-i", &self.path])
            .args(["-an", "-sn"])
            .args(["-vf", &self.filter])
            .args(["-frames:v", &n.to_string()])
            .args(["-f", "rawvideo", "-pix_fmt", &self.src_fmt])
            .arg("pipe:1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let Ok((id, _, Some(mut out), _)) = children.spawn(&mut cmd) else {
            self.exhausted = true;
            return;
        };
        let mut frames: Vec<Option<Vec<u8>>> = Vec::with_capacity(n);
        'read: for _ in 0..n {
            let mut buf = vec![0u8; frame_size];
            let mut filled = 0;
            while filled < frame_size {
                match out.read(&mut buf[filled..]) {
                    Ok(0) => break 'read,
                    Ok(k) => filled += k,
                    Err(_) => break 'read,
                }
            }
            if filled < frame_size {
                break;
            }
            frames.push(Some(buf));
        }
        children.kill(id);
        if frames.is_empty() {
            self.exhausted = true;
            return;
        }
        // Ausgabe-Reihenfolge: Medienzeit absteigend; am Chunk-Ende fehlende
        // Frames (EOF) hält der letzte dekodierte.
        let m = frames.len();
        for i in (0..n).rev() {
            let idx = i.min(m - 1);
            let f = if i == idx {
                frames[idx].take().expect("Frame einmal entnommen")
            } else {
                frames[idx].clone().expect("Frame noch vorhanden")
            };
            self.buf.push_back(f);
        }
        self.media_next = lo - self.step;
        if lo <= 0.0 {
            // Unterhalb von Medienzeit 0 gibt es nichts mehr.
            self.exhausted = true;
        }
    }
}

/// Ein Layer-Decoder im Compositing-Pfad: liefert transparent gepolsterte
/// RGBA-Frames in Decode-Auflösung (das volle Zielframe repräsentierend).
struct LayerStream {
    src: LayerSource,
    /// Letzter vollständiger Frame, f32-RGBA 0..1 (initial transparent).
    frame: Vec<f32>,
    /// Roh-Bytes aus der Decoder-Pipe (`w*h*src_bpp`), vor der f32-Wandlung.
    read_buf: Vec<u8>,
    /// Bytes pro Pixel der Quelle: 4 = rgba8, 8 = rgba64le (>8-Bit-Quelle).
    src_bpp: usize,
    dead: bool,
    w: usize,
    h: usize,
    src_in: f64,
    /// Medienfortschritt pro Ausgabesekunde (signiert; 0 = Standbild).
    media_step: f64,
    fx: ClipFx,
    /// Effekt-Stapel (Keyframes in Medienzeit) — vor dem Grading angewendet.
    effects: Vec<EffectInstance>,
    /// Vorberechnete Farbkorrektur (Identität ⇒ kein Grading-Pass).
    grade: grade::GradeParams,
    /// Sichtbarer Inhalt im gepolsterten Puffer (Vignetten-Bezugsrahmen).
    content: (usize, usize, usize, usize),
    /// Übergangs-Fenster dieses Layers (Exportzeit).
    transitions: Vec<PlanTransition>,
}

/// Ein Layer des Compositing-Pfads: Decoder-Stream, Farbfläche (Dip) oder
/// CPU-gerasterter Titel.
enum SegLayer {
    Stream(LayerStream),
    Solid {
        /// 2×2-f32-RGBA-Puffer in Volltonfarbe (bilinear gesampelt = uniform).
        data: Vec<f32>,
        transitions: Vec<PlanTransition>,
    },
    Title(TitleLayer),
    /// Verschachtelte Sequenz, rekursiv komponiert.
    Nest(NestLayer),
}

/// Titel-Layer: einmal gerastert (gemeinsamer Rasterizer mit dem
/// Programmmonitor), statisch über das Segment; Effekte mit Keyframes
/// laufen pro Frame über eine Arbeitskopie (Reihenfolge wie bei
/// Decoder-Layern: Effekte → Farbkorrektur).
struct TitleLayer {
    base: Vec<f32>,
    scratch: Vec<f32>,
    use_scratch: bool,
    w: usize,
    h: usize,
    /// Vertikale Raster-Erweiterung (Abspann-Rolle): Quad-Höhe × k.
    extend_k: u32,
    src_in: f64,
    /// Medienfortschritt pro Ausgabesekunde (signiert; 0 = Standbild).
    media_step: f64,
    fx: ClipFx,
    effects: Vec<EffectInstance>,
    grade: grade::GradeParams,
    transitions: Vec<PlanTransition>,
}

impl TitleLayer {
    /// Frame vorbereiten: ohne aktive Effekte bleibt der (ggf. vorab
    /// gegradete) Basis-Raster stehen; sonst Kopie → Effekte → Grading.
    fn advance(&mut self, threads: usize, t_off: f64) {
        self.use_scratch = false;
        if self.effects.is_empty() {
            return;
        }
        let resolved = effects::resolve_video_effects(
            &self.effects,
            self.src_in + t_off * self.media_step,
        );
        self.scratch.copy_from_slice(&self.base);
        if !resolved.is_empty() {
            effects::apply_effects_buffer(
                &mut self.scratch,
                self.w,
                self.h,
                (0, 0, self.w, self.h),
                &resolved,
                threads,
            );
        }
        if !self.grade.is_identity() {
            grade::grade_buffer(
                &mut self.scratch,
                self.w,
                self.h,
                (0, 0, self.w, self.h),
                &self.grade,
                threads,
            );
        }
        self.use_scratch = true;
    }

    fn current(&self) -> &[f32] {
        if self.use_scratch {
            &self.scratch
        } else {
            &self.base
        }
    }
}

impl LayerStream {
    /// Nächsten Frame einlesen; bei EOF/Kurz-Read bleibt der letzte stehen.
    /// Frische Frames laufen direkt durch Effekte + Farbkorrektur (in
    /// place — der gehaltene EOF-Frame ist damit bereits verarbeitet).
    /// `t_off` = Segmentzeit des Frames (Effekt-Keyframes in Medienzeit).
    fn advance(&mut self, threads: usize, t_off: f64, children: &mut ChildRegistry) {
        if self.dead {
            return;
        }
        // Roh-Bytes der Quelle (rgba8 oder rgba64le) lesen.
        let size = self.w * self.h * self.src_bpp;
        let got = match &mut self.src {
            LayerSource::Pipe { out, .. } => {
                let mut filled = 0;
                while filled < size {
                    match out.read(&mut self.read_buf[filled..size]) {
                        Ok(0) => break,
                        Ok(n) => filled += n,
                        Err(_) => break,
                    }
                }
                filled == size
            }
            LayerSource::Reverse(rev) => {
                if rev.buf.is_empty() {
                    rev.refill(children, size);
                }
                match rev.buf.pop_front() {
                    Some(f) => {
                        self.read_buf.copy_from_slice(&f);
                        true
                    }
                    None => false,
                }
            }
        };
        if got {
            // Roh-Bytes → f32-RGBA (display-referred 0..1).
            if self.src_bpp == 8 {
                crate::core::pixbuf::rgba64le_into_f32(&self.read_buf, &mut self.frame);
            } else {
                crate::core::pixbuf::rgba8_into_f32(&self.read_buf, &mut self.frame);
            }
            if !self.effects.is_empty() {
                let resolved = effects::resolve_video_effects(
                    &self.effects,
                    self.src_in + t_off * self.media_step,
                );
                if !resolved.is_empty() {
                    effects::apply_effects_buffer(
                        &mut self.frame,
                        self.w,
                        self.h,
                        self.content,
                        &resolved,
                        threads,
                    );
                }
            }
            if !self.grade.is_identity() {
                grade::grade_buffer(
                    &mut self.frame,
                    self.w,
                    self.h,
                    self.content,
                    &self.grade,
                    threads,
                );
            }
        } else {
            self.dead = true;
        }
    }
}

/// Segment mit Transformationen rendern: je Layer ein ffmpeg-Decoder
/// (Decode-Auflösung wächst mit der maximalen Skalierung im Segment, damit
/// Zooms scharf bleiben), pro Frame werden die animierten Parameter
/// ausgewertet und die Layer per CPU-Compositor auf das Canvas gemischt.
#[allow(clippy::too_many_arguments)]
/// Self-contained Nest-Kontext des Worker-Threads: rekonstruierte innere
/// Timelines + Blatt-Medien. Dient als [`compose::NestResolver`] für die
/// rekursive Komposition.
struct NestRenderCtx {
    timelines: HashMap<String, TimelineStore>,
    media: HashMap<String, NestMediaInfo>,
}

impl NestRenderCtx {
    fn from_plan(plan: &RenderPlan) -> NestRenderCtx {
        NestRenderCtx {
            timelines: plan
                .nests
                .iter()
                .map(|(id, ns)| (id.clone(), ns.to_timeline()))
                .collect(),
            media: plan.nest_media.clone(),
        }
    }
}

impl compose::NestResolver for NestRenderCtx {
    fn nested_timeline(&self, seq_id: &str) -> Option<&TimelineStore> {
        self.timelines.get(seq_id)
    }
}

/// Ein Blatt-Frame einer verschachtelten Sequenz per Einzelbild-Extraktion
/// (Original, contain-fit + transparent gepolstert, w×h). Generatoren
/// (Titel/Untertitel) innerhalb von Nests werden hier (noch) nicht gerastert.
fn leaf_frame(
    clip: &TimelineClip,
    media_t: f64,
    w: usize,
    h: usize,
    media: &HashMap<String, NestMediaInfo>,
    children: &mut ChildRegistry,
) -> Option<Vec<f32>> {
    if clip.is_generator() {
        return None;
    }
    let info = media.get(&clip.asset_id)?;
    let filter = format!(
        "scale={w}:{h}:force_original_aspect_ratio=decrease:flags=bicubic,format=rgba,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:color=black@0.0"
    );
    let mut cmd = Command::new(crate::services::ffmpeg_bin());
    cmd.args(["-v", "error"]);
    if info.image {
        cmd.args(["-loop", "1", "-framerate", "1"]);
    } else {
        cmd.args(["-ss", &format!("{:.4}", media_t.max(0.0))]);
    }
    cmd.args(["-i", &info.path])
        .args(["-an", "-sn"])
        .args(["-vf", &filter])
        .args(["-frames:v", "1"])
        .args(["-f", "rawvideo", "-pix_fmt", "rgba"])
        .arg("pipe:1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let (id, _, stdout, _) = children.spawn(&mut cmd).ok()?;
    let mut out = stdout?;
    let mut buf = vec![0u8; w * h * 4];
    let mut filled = 0;
    while filled < buf.len() {
        match out.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(k) => filled += k,
            Err(_) => break,
        }
    }
    children.kill(id);
    if filled < buf.len() {
        return None;
    }
    // Blatt-Decode bleibt vorerst 8-Bit (rgba) → f32. Höhere Quell-Bittiefe
    // für Nest-Blätter ist eine spätere Verfeinerung (Stufe 1).
    Some(crate::core::pixbuf::rgba8_to_f32(&buf))
}

/// Nest-Layer im Compositing-Pfad: hält die rekursiv komponierte innere
/// Sequenz als volles Zielframe-Puffer (w×h), auf den die äußeren Clip-
/// Parameter (Effekte/Grade/Transform/Übergang) wirken.
struct NestLayer {
    seq_id: String,
    /// Compositing-Puffergröße (innere Auflösung × Skalierung).
    w: usize,
    h: usize,
    /// Natürliche Größe für die Quad-Berechnung (innere Sequenzauflösung) —
    /// das innere Frame wird contain-fit ins äußere gelegt.
    nw: usize,
    nh: usize,
    src_in: f64,
    media_step: f64,
    fx: ClipFx,
    grade: grade::GradeParams,
    effects: Vec<EffectInstance>,
    transitions: Vec<PlanTransition>,
    frame: Vec<f32>,
}

impl NestLayer {
    fn advance(
        &mut self,
        threads: usize,
        t_off: f64,
        ctx: &NestRenderCtx,
        children: &mut ChildRegistry,
    ) {
        let inner_t = self.src_in + t_off * self.media_step;
        let Some(inner_tl) = ctx.timelines.get(&self.seq_id) else {
            return;
        };
        let (w, h) = (self.w, self.h);
        let media = &ctx.media;
        let mut fetch = |clip: &TimelineClip, _media_t: f64, lw: usize, lh: usize| {
            leaf_frame(clip, _media_t, lw, lh, media, children)
        };
        self.frame =
            compose::composite_sequence_frame(inner_tl, ctx, inner_t, w, h, threads, &mut fetch, 1);
        // Effekte + Farbkorrektur des Nest-Clips auf das komponierte Frame
        // (gleiche Reihenfolge wie bei Decoder-Layern: Effekte → Grading).
        if !self.effects.is_empty() {
            let resolved = effects::resolve_video_effects(&self.effects, inner_t);
            if !resolved.is_empty() {
                effects::apply_effects_buffer(&mut self.frame, w, h, (0, 0, w, h), &resolved, threads);
            }
        }
        if !self.grade.is_identity() {
            grade::grade_buffer(&mut self.frame, w, h, (0, 0, w, h), &self.grade, threads);
        }
    }
}

fn render_segment_composited(
    segment: &VideoSegment,
    seg_start_frame: u64,
    video: &VideoSettings,
    fps_arg: &str,
    enc_in: &mut std::process::ChildStdin,
    cancel: &AtomicBool,
    children: &mut ChildRegistry,
    nests: &NestRenderCtx,
    progress: &mut dyn FrameProgress,
) -> Result<(), CompErr> {
    let (tw, th) = (video.width as usize, video.height as usize);
    let fps = video.fps;
    let seg_dur = segment.frames as f64 / fps;
    // Pipe-Bittiefe (= Ziel-Bittiefe): bestimmt die finale Quantisierung.
    let hi_bit = pipe_hi_bit(video);

    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(8);
    let mut layers: Vec<SegLayer> = Vec::new();
    let kill_layers = |children: &ChildRegistry, layers: &[SegLayer]| {
        for l in layers {
            if let SegLayer::Stream(LayerStream {
                src: LayerSource::Pipe { dec_id, .. },
                ..
            }) = l
            {
                children.kill(*dec_id);
            }
        }
    };

    for plan_layer in &segment.layers {
        // Titel: CPU-Raster statt Decoder — Auflösung wächst mit der
        // maximalen Skalierung im Segment (Schärfe bei Zoom, wie Streams).
        if let Some(spec) = &plan_layer.title {
            let m1 = plan_layer.src_in + seg_dur * plan_layer.media_step;
            let max_s = compose::max_scale_in_window(
                &plan_layer.fx,
                plan_layer.src_in.min(m1),
                plan_layer.src_in.max(m1),
            )
            .clamp(1.0, 2.0);
            let dw = ((((tw as f64 * max_s) / 2.0).round() as usize) * 2).clamp(2, 4096);
            let dh = ((((th as f64 * max_s) / 2.0).round() as usize) * 2).clamp(2, 4096);
            let raster = crate::core::text_raster::render_title(spec, dw as u32, dh as u32);
            let (rw, rh, extend_k) = (raster.w, raster.h, raster.extend_k);
            let mut base = crate::core::pixbuf::rgba8_to_f32(&raster.data);
            let grade_params = grade::precompute(&plan_layer.grade);
            let dynamic = effects::has_active_video_effects(&plan_layer.effects);
            if !dynamic && !grade_params.is_identity() {
                // Ohne Effekte ist das Grading statisch — einmal einbrennen.
                grade::grade_buffer(&mut base, rw, rh, (0, 0, rw, rh), &grade_params, threads);
            }
            layers.push(SegLayer::Title(TitleLayer {
                scratch: if dynamic { base.clone() } else { Vec::new() },
                base,
                use_scratch: false,
                w: rw,
                h: rh,
                extend_k,
                src_in: plan_layer.src_in,
                media_step: plan_layer.media_step,
                fx: plan_layer.fx.clone(),
                effects: if dynamic { plan_layer.effects.clone() } else { Vec::new() },
                grade: grade_params,
                transitions: plan_layer.transitions.clone(),
            }));
            continue;
        }
        // Farbflächen (Dips) brauchen keinen Decoder (2×2-f32-Voltonfläche).
        if let Some(color) = plan_layer.solid {
            let c = [
                color[0] as f32 / 255.0,
                color[1] as f32 / 255.0,
                color[2] as f32 / 255.0,
                1.0,
            ];
            let mut data = Vec::with_capacity(2 * 2 * 4);
            for _ in 0..4 {
                data.extend_from_slice(&c);
            }
            layers.push(SegLayer::Solid {
                data,
                transitions: plan_layer.transitions.clone(),
            });
            continue;
        }
        // Verschachtelte Sequenz: rekursiv komponieren statt dekodieren. Die
        // Decode-Auflösung folgt (wie Streams) der maximalen Skalierung.
        if let Some(seq_id) = &plan_layer.nest_seq {
            let m1 = plan_layer.src_in + seg_dur * plan_layer.media_step;
            let max_s = compose::max_scale_in_window(
                &plan_layer.fx,
                plan_layer.src_in.min(m1),
                plan_layer.src_in.max(m1),
            )
            .clamp(1.0, 2.0);
            // Natürliche Größe = innere Sequenzauflösung (Fallback: Zielraster).
            let (nw, nh) = if plan_layer.natural_w > 0 && plan_layer.natural_h > 0 {
                (plan_layer.natural_w as usize, plan_layer.natural_h as usize)
            } else {
                (tw, th)
            };
            // Compositing-Puffer in INNERER Auflösung × Skalierung (Schärfe bei
            // Zoom), Seitenverhältnis der inneren Sequenz bewahrt.
            let dw = ((((nw as f64 * max_s) / 2.0).round() as usize) * 2).clamp(2, 4096);
            let dh = ((((nh as f64 * max_s) / 2.0).round() as usize) * 2).clamp(2, 4096);
            layers.push(SegLayer::Nest(NestLayer {
                seq_id: seq_id.clone(),
                w: dw,
                h: dh,
                nw,
                nh,
                src_in: plan_layer.src_in,
                media_step: plan_layer.media_step,
                fx: plan_layer.fx.clone(),
                grade: grade::precompute(&plan_layer.grade),
                effects: plan_layer.effects.clone(),
                transitions: plan_layer.transitions.clone(),
                // Initial opak schwarz, bis advance das erste Frame liefert.
                frame: {
                    let mut b = vec![0f32; dw * dh * 4];
                    for px in b.chunks_exact_mut(4) {
                        px[3] = 1.0;
                    }
                    b
                },
            }));
            continue;
        }
        // Decode-Auflösung: Zielgröße × max. Skalierung (gedeckelt) — mehr
        // als die Quelle hergibt, skaliert ffmpeg ohnehin nicht hoch (Schärfe
        // gewinnt nur, solange Quellpixel vorhanden sind).
        let m1 = plan_layer.src_in + seg_dur * plan_layer.media_step;
        let max_s = compose::max_scale_in_window(
            &plan_layer.fx,
            plan_layer.src_in.min(m1),
            plan_layer.src_in.max(m1),
        )
        .clamp(1.0, 2.0);
        let dw = ((((tw as f64 * max_s) / 2.0).round() as usize) * 2).clamp(2, 4096);
        let dh = ((((th as f64 * max_s) / 2.0).round() as usize) * 2).clamp(2, 4096);

        let freeze = !plan_layer.image && plan_layer.media_step == 0.0;
        let reverse = !plan_layer.image && plan_layer.media_step < 0.0;
        // Konstante Geschwindigkeit über dieselbe setpts/fps-Kette wie der
        // Schnellpfad und die Vorschau (identische Frame-Auswahl).
        let setpts = if plan_layer.image || freeze {
            String::new()
        } else {
            speed_setpts_filter(plan_layer.media_step.abs())
        };
        // Contain-Fit + TRANSPARENTES Padding: der Puffer repräsentiert das
        // volle Frame, Alpha (z. B. PNG) bleibt erhalten. >8-Bit-Quelle ⇒
        // 16-Bit-Decode (rgba64le), damit Log-/HDR-Material verlustfrei in die
        // f32-Pipeline läuft; 8-Bit-Quelle bleibt rgba (keine Bandbreite drauf).
        let src_hi = plan_layer.src_bit_depth > 8;
        let src_fmt = if src_hi { "rgba64le" } else { "rgba" };
        let src_bpp = if src_hi { 8 } else { 4 };
        let filter = format!(
            "{setpts}fps={fps_arg},scale={dw}:{dh}:force_original_aspect_ratio=decrease:flags=bicubic,format={src_fmt},pad={dw}:{dh}:(ow-iw)/2:(oh-ih)/2:color=black@0.0"
        );
        // Sichtbarer Inhalt im Puffer: contain-fit der Quelle, zentriert
        // (Spiegel der ffmpeg-Filterkette scale=…:decrease + pad-center).
        let content = if plan_layer.natural_w > 0 && plan_layer.natural_h > 0 {
            let (nw, nh) = (plan_layer.natural_w as f64, plan_layer.natural_h as f64);
            let fit = (dw as f64 / nw).min(dh as f64 / nh);
            let cw = ((nw * fit).round() as usize).clamp(1, dw);
            let ch = ((nh * fit).round() as usize).clamp(1, dh);
            ((dw - cw) / 2, (dh - ch) / 2, cw, ch)
        } else {
            (0, 0, dw, dh)
        };

        let source = if reverse {
            // Rückwärts: Chunk-Decode mit Frame-Puffer (Budget ≈ 192 MB).
            let frame_bytes = dw * dh * src_bpp;
            let chunk = (192 * 1024 * 1024 / frame_bytes.max(1)).clamp(2, 128);
            LayerSource::Reverse(ReverseDecode {
                path: plan_layer.path.clone(),
                filter: filter.clone(),
                src_fmt: src_fmt.to_string(),
                media_next: plan_layer.src_in,
                step: plan_layer.media_step.abs() / fps,
                chunk_frames: chunk,
                buf: Default::default(),
                exhausted: false,
            })
        } else {
            let mut cmd = Command::new(crate::services::ffmpeg_bin());
            cmd.args(["-v", "error"]);
            if plan_layer.image {
                cmd.args(["-loop", "1", "-framerate", fps_arg]);
            } else {
                cmd.args(["-ss", &format!("{:.4}", plan_layer.src_in)]);
            }
            // Standbild: ein Frame genügt — die Halte-Logik füllt den Rest.
            let dec_frames = if freeze { 1 } else { segment.frames };
            cmd.args(["-i", &plan_layer.path])
                .args(["-an", "-sn"])
                .args(["-vf", &filter])
                .args(["-frames:v", &dec_frames.to_string()])
                .args(["-f", "rawvideo", "-pix_fmt", src_fmt])
                .arg("pipe:1")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            let (dec_id, _, stdout, _) = match children.spawn(&mut cmd) {
                Ok(v) => v,
                Err(e) => {
                    kill_layers(children, &layers);
                    return Err(CompErr::Failed(e));
                }
            };
            let Some(out) = stdout else {
                kill_layers(children, &layers);
                return Err(CompErr::Failed("Decoder-stdout nicht verfügbar".into()));
            };
            LayerSource::Pipe { dec_id, out }
        };
        layers.push(SegLayer::Stream(LayerStream {
            src: source,
            // f32-Frame (verarbeitet) + Roh-Byte-Lesepuffer (Quell-Bittiefe:
            // 4 B/px = rgba8, 8 B/px = rgba64le).
            frame: vec![0f32; dw * dh * 4],
            read_buf: vec![0u8; dw * dh * src_bpp],
            src_bpp,
            dead: false,
            w: dw,
            h: dh,
            src_in: plan_layer.src_in,
            media_step: plan_layer.media_step,
            fx: plan_layer.fx.clone(),
            effects: plan_layer.effects.clone(),
            grade: grade::precompute(&plan_layer.grade),
            content,
            transitions: plan_layer.transitions.clone(),
        }));
    }

    let mut canvas = vec![0f32; tw * th * 4];

    for f in 0..segment.frames {
        if cancel.load(Ordering::Relaxed) {
            kill_layers(children, &layers);
            return Err(CompErr::Cancelled);
        }
        for layer in &mut layers {
            match layer {
                SegLayer::Stream(s) => s.advance(threads, f as f64 / fps, children),
                SegLayer::Title(t) => t.advance(threads, f as f64 / fps),
                SegLayer::Nest(n) => n.advance(threads, f as f64 / fps, nests, children),
                SegLayer::Solid { .. } => {}
            }
        }
        // Canvas opak schwarz zurücksetzen (f32).
        for px in canvas.chunks_exact_mut(4) {
            px[0] = 0.0;
            px[1] = 0.0;
            px[2] = 0.0;
            px[3] = 1.0;
        }
        let t_off = f as f64 / fps;
        // Exportzeit des Frames — Bezugssystem der Übergangs-Fenster.
        let seq_t = (seg_start_frame + f) as f64 / fps;
        let frames: Vec<compose::CpuLayerFrame> = layers
            .iter()
            .filter_map(|layer| match layer {
                SegLayer::Stream(l) => {
                    let fx = compose::eval_fx(&l.fx, l.src_in + t_off * l.media_step);
                    let t_fx = eval_plan_transitions(&l.transitions, seq_t);
                    let opacity = fx.opacity * t_fx.opacity;
                    if opacity <= 0.0 {
                        return None;
                    }
                    // Der Layer-Puffer repräsentiert das volle Frame →
                    // natürliche Größe = Framegröße (Fit-Faktor 1).
                    let mut quad =
                        compose::layer_quad(tw as f64, th as f64, tw as f64, th as f64, &fx);
                    compose::apply_transition_to_quad(&mut quad, &t_fx, tw as f64, th as f64);
                    Some(compose::CpuLayerFrame {
                        data: &l.frame,
                        w: l.w,
                        h: l.h,
                        quad,
                        opacity,
                        mask: t_fx.mask.map(|m| compose::mask_to_pixels(&m, tw, th)),
                    })
                }
                SegLayer::Solid { data, transitions } => {
                    let t_fx = eval_plan_transitions(transitions, seq_t);
                    if t_fx.opacity <= 0.0 {
                        return None;
                    }
                    Some(compose::CpuLayerFrame {
                        data,
                        w: 2,
                        h: 2,
                        quad: compose::LayerQuad {
                            cx: tw as f64 / 2.0,
                            cy: th as f64 / 2.0,
                            w: tw as f64,
                            h: th as f64,
                            rot_deg: 0.0,
                        },
                        opacity: t_fx.opacity,
                        mask: None,
                    })
                }
                // Identische Quad-Mathematik wie Streams: der Titel-Raster
                // repräsentiert das volle Frame (Fit-Faktor 1).
                SegLayer::Title(l) => {
                    let fx = compose::eval_fx(&l.fx, l.src_in + t_off * l.media_step);
                    let t_fx = eval_plan_transitions(&l.transitions, seq_t);
                    let opacity = fx.opacity * t_fx.opacity;
                    if opacity <= 0.0 {
                        return None;
                    }
                    let mut quad =
                        compose::layer_quad(tw as f64, th as f64, tw as f64, th as f64, &fx);
                    // Erweiterter Raster (Abspann): Quad vertikal strecken.
                    quad.h *= l.extend_k as f64;
                    compose::apply_transition_to_quad(&mut quad, &t_fx, tw as f64, th as f64);
                    Some(compose::CpuLayerFrame {
                        data: l.current(),
                        w: l.w,
                        h: l.h,
                        quad,
                        opacity,
                        mask: t_fx.mask.map(|m| compose::mask_to_pixels(&m, tw, th)),
                    })
                }
                // Nest: das innere Frame (innere Auflösung) wird contain-fit
                // ins äußere Frame gelegt — natürliche Größe = innere Auflösung.
                SegLayer::Nest(l) => {
                    let fx = compose::eval_fx(&l.fx, l.src_in + t_off * l.media_step);
                    let t_fx = eval_plan_transitions(&l.transitions, seq_t);
                    let opacity = fx.opacity * t_fx.opacity;
                    if opacity <= 0.0 {
                        return None;
                    }
                    let mut quad =
                        compose::layer_quad(tw as f64, th as f64, l.nw as f64, l.nh as f64, &fx);
                    compose::apply_transition_to_quad(&mut quad, &t_fx, tw as f64, th as f64);
                    Some(compose::CpuLayerFrame {
                        data: &l.frame,
                        w: l.w,
                        h: l.h,
                        quad,
                        opacity,
                        mask: t_fx.mask.map(|m| compose::mask_to_pixels(&m, tw, th)),
                    })
                }
            })
            .collect();
        compose::composite_frame(&mut canvas, tw, th, &frames, threads);
        // f32-Canvas → Pipe-Format: 16 Bit (rgba64le, verlustarm) für >8-Bit-
        // Ziele, sonst 8 Bit mit TPDF-Dithering (bricht Restbanding).
        let out_bytes = if hi_bit {
            crate::core::pixbuf::f32_to_rgba64le(&canvas)
        } else {
            crate::core::pixbuf::f32_to_rgba8_dithered(&canvas, tw, th)
        };
        if let Err(e) = enc_in.write_all(&out_bytes) {
            kill_layers(children, &layers);
            return Err(CompErr::Failed(e.to_string()));
        }
        progress.advance(1);
    }
    kill_layers(children, &layers);
    Ok(())
}

fn encode_audio_only(
    settings: &ExportSettings,
    wav: &Path,
    part: &Path,
    cancel: &AtomicBool,
    children: &mut ChildRegistry,
) -> Result<(), String> {
    let audio = settings.audio.as_ref().expect("audio settings");
    let mut cmd = Command::new(crate::services::ffmpeg_bin());
    cmd.args(["-y", "-v", "error"])
        .args(["-i", &wav.to_string_lossy()])
        .args(["-map", "0:a:0"])
        .args(audio_codec_args(audio));
    if settings.container.faststart {
        cmd.args(["-movflags", "+faststart"]);
    }
    cmd.args(["-f", settings.container.muxer]);
    cmd.arg(part)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let (id, _, _, stderr) = children.spawn(&mut cmd)?;
    let mut stderr = stderr.ok_or("Encoder-stderr nicht verfügbar")?;
    let stderr_task = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });
    let status = children.wait(id);
    let stderr_buf = stderr_task.join().unwrap_or_default();
    if cancel.load(Ordering::Relaxed) {
        return Err("abgebrochen".into());
    }
    if !status.map(|s| s.success()).unwrap_or(false) {
        let tail = stderr_tail(&stderr_buf);
        return Err(format!(
            "Audio-Encoder fehlgeschlagen: {}",
            if tail.is_empty() { "ohne Fehlermeldung".into() } else { tail }
        ));
    }
    Ok(())
}

fn stderr_tail(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines
        .iter()
        .rev()
        .take(4)
        .rev()
        .copied()
        .collect::<Vec<_>>()
        .join("\n")
}

