//! Wiedergabe-Engine (SequencePlayer-Pendant): dekodiert Video über
//! ffmpeg-Pipes (rawvideo/rgba) in Texturen und Audio (f32le/48 kHz) über
//! einen eigenen Mixdown in einen einzelnen raylib-AudioStream. Für den
//! Programmmonitor läuft EIN Decoder je sichtbarem Video-Clip am Playhead
//! (Texturen unter "player://clip/<id>" — der Monitor komponiert die Layer
//! mit ihren animierten Transformationen); Ton kommt aus den aktiven
//! Audio-Clips (Spur-Gain/Pan, Clip-Gain inkl. Lautstärke-Keyframes und
//! Master-Fader werden beim Mischen angewendet, Spitzenpegel landen in
//! `state.audio` für die Mixer-Meter); der Quellmonitor spielt das geladene
//! Asset unter "player://source".

use crate::core::audio_fx::{db_to_linear, pan_gains, AudioFxChain};
use crate::core::compose;
use crate::core::effects::EffectInstance;
use crate::core::frame_cache::{seek_decision, FrameCache, FrameKey, ScrubCoalescer, SeekAction};
use crate::core::timeline::{TimelineClip, TrackKind};
use crate::state::AppState;
use crate::ui::textures::TextureCache;
use raylib::audio::RaylibAudio;
use raylib::core::texture::{Image, RaylibTexture2D};
use raylib::{RaylibHandle, RaylibThread};
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{sync_channel, Receiver, TryRecvError};
use std::sync::Arc;

pub const SOURCE_KEY: &str = "player://source";
/// Texture-Schlüssel des Programm-Bildes aus dem Sequenz-Render-Cache (ein
/// Decoder statt Live-Compositing während der Wiedergabe gecachter Bereiche).
pub const RENDER_CACHE_KEY: &str = "player://rendercache";

/// Texture-Schlüssel des CPU-komponierten Programmbildes (Vollbild). Wird
/// belegt, wenn am Playhead ein Adjustment Layer aktiv ist: dessen Korrektur-
/// Pass wirkt auf das zusammengesetzte Bild ALLER Spuren darunter, was die
/// GPU-Per-Layer-Pipeline nicht abbilden kann — der Player komponiert das
/// Programm dann über den geteilten Compositing-Kern (`composite_sequence_frame`,
/// formelgleich zum Export) und der Monitor zeichnet diese eine Textur.
pub const PROGRAM_COMPOSITE_KEY: &str = "player://programcomposite";

/// Texture-Schlüssel eines Programm-Layers (Decoder je Clip).
pub fn clip_texture_key(clip_id: &str) -> String {
    format!("player://clip/{clip_id}")
}

/// Decoder-Map-ID einer Multicam-Winkel-Kachel (eigener Decoder je Winkel im
/// Multicam-Raster). Die zugehörige Textur ist `clip_texture_key(&mc_angle_id)`
/// — wie bei normalen Programm-Layern leitet der Player den Textur-Schlüssel aus
/// der Map-ID ab.
pub fn mc_angle_id(clip_id: &str, angle: u32) -> String {
    format!("mc/{clip_id}/{angle}")
}

/// Topmost sichtbarer Multicam-Clip am Playhead (den das Multicam-Raster zeigt).
pub fn active_multicam_clip(state: &AppState) -> Option<&crate::core::timeline::TimelineClip> {
    let t = state.timeline.playhead_sec;
    let mut found = None;
    for layer in compose::visible_program_layers(&state.timeline, t) {
        if let compose::ProgramLayer::Clip { clip, .. } = layer {
            if clip.multicam.is_some() {
                found = Some(clip);
            }
        }
    }
    found
}

const MAX_DECODE_WIDTH: f64 = 1920.0;
/// Mindestabstand zwischen Scrub-Restarts (Sekunden).
const SCRUB_RESTART_INTERVAL: f64 = 0.12;

const AUDIO_RATE: u32 = 48000;
const AUDIO_CHANNELS: usize = 2;
/// Frames pro Mix-Block und raylib-Sub-Buffer (~85 ms bei 48 kHz).
/// WICHTIG: raylib hebt die Sub-Buffer-Größe intern auf die Perioden-
/// größe des Geräts an (PulseAudio hier: 3600 Frames). Jeder Update
/// füllt genau einen Sub-Buffer — ist der Block kleiner als der
/// Sub-Buffer, füllt raylib den Rest mit Stille auf und der Ton wird
/// zerhackt. 4096 liegt über den üblichen Periodengrößen.
const AUDIO_CHUNK_FRAMES: usize = 4096;
/// Sequenz-Drift, ab der die Audio-Uhr neu verankert wird (Sekunden) — fängt
/// Seeks und Loop-Sprünge während der Wiedergabe ab; der Puffer-Vorlauf
/// (~1–2 Blöcke) bleibt darunter.
const AUDIO_RESYNC_TOLERANCE: f64 = 0.35;
/// Positionssprung (Sekunden), ab dem die Integrated-Lautheitsmessung neu
/// startet. Ein bloßes Fortsetzen nach Pause liegt darunter ⇒ Messung bleibt.
const LOUDNESS_RESET_TOLERANCE: f64 = 0.75;
/// Anti-Klick-Rampe an harten Schnittkanten (Frames; ~5 ms bei 48 kHz).
const CLICK_RAMP_FRAMES: usize = 240;
/// Sub-Block der Hüllkurven-Auswertung (Frames; ~5 ms) — glatt genug für
/// Fades/Lautstärke-Keyframes, identisch zum Export-Mix.
const ENV_BLOCK_FRAMES: usize = 256;
/// Decoder-Vorlauf: Audio-Clips so früh starten, dass beim Erreichen der
/// Render-Kante Samples gepuffert sind (Sekunden Output-/Gerätezeit).
const AUDIO_PREROLL: f64 = 0.5;
/// Höchster Shuttle-Betrag mit hörbarem Ton; darüber (z. B. 8×) stumm.
const MAX_SHUTTLE_AUDIO_RATE: f64 = 4.0;
/// Nominale Geräte-Latenz (Output-Frames) zwischen geschriebenem und gehörtem
/// Sample. raylib puffert ZWEI Sub-Buffer; `heard_pos` wird direkt NACH dem
/// Block-Schreiben gemessen (Puffer dann am vollsten ≈ 2 Blöcke) — diese
/// Konstante zentriert daher den AV-Versatz auf nahezu null. Reine Konstante,
/// die Drift-/Rate-Kopplung läuft über `master_out` (gerätegetaktet).
const NOMINAL_LATENCY_FRAMES: u64 = AUDIO_CHUNK_FRAMES as u64 * 2;
/// Proportionaler Slew-Anteil pro Tick: so viel der Rest-Drift wird je Frame
/// aufgeholt (Delay-Locked-Loop gegen die Geräte-Uhr).
const SLEW_GAIN: f64 = 0.20;
/// Maximaler Playhead-Slew pro Tick (Sekunden) — verhindert sichtbares Springen.
const MAX_SLEW_PER_TICK: f64 = 0.012;
/// Zielmenge Output-Frames im Decoder-Umkehrpuffer eines Reverse-Audio-Chunks.
const REVERSE_AUDIO_CHUNK_FRAMES: usize = AUDIO_RATE as usize; // ~1 s Output
/// Audio-Scrubbing: Länge eines Grains (Sekunden) und Refresh-Intervall.
const SCRUB_GRAIN_SEC: f64 = 0.10;

// ---------------------------------------------------------------- Video

struct VideoFrame {
    index: u64,
    data: Vec<u8>,
}

/// Gerade Decode-Maße aus Quelle × Monitor-Skalierung.
fn decode_dims(src_w: u32, src_h: u32, scale: f64) -> (i32, i32) {
    let target_w = ((src_w as f64).min(MAX_DECODE_WIDTH) * scale).max(64.0);
    let w = ((target_w / 2.0).round() * 2.0) as i32;
    let h =
        (((src_h as f64 / src_w.max(1) as f64) * w as f64 / 2.0).round() * 2.0).max(2.0) as i32;
    (w, h)
}

/// Anzahl Frames, die beim Pausieren in JEDE Richtung um den Playhead
/// vorausdekodiert werden (Read-Ahead) — Frame-Stepping (←/→) reagiert dann
/// sofort aus dem Cache. ~0,5 s bei 25 fps.
const PREFETCH_RADIUS: i64 = 12;
/// Mindest-Stillstand des Playheads (Sekunden), bevor Read-Ahead startet —
/// während aktiven Scrubbens wird nicht prefetcht (Debounce über die App-Uhr).
const PREFETCH_SETTLE: f64 = 0.10;

// ----------------------------------------------------------- Hardware-Decode

/// Gewählte Hardware-Decode-Methode für eine Decoder-Kette (oder None = SW).
type HwMethod = Option<String>;

/// Verfügbare hwaccel-Methoden EINMALIG über `ffmpeg -hwaccels` erkennen
/// (gecacht). Gibt die Liste in der von ffmpeg gemeldeten Reihenfolge zurück
/// (plattform-typische Bestenliste: videotoolbox/cuda/vaapi/qsv …).
fn available_hwaccels() -> &'static [String] {
    use std::sync::OnceLock;
    static HW: OnceLock<Vec<String>> = OnceLock::new();
    HW.get_or_init(|| {
        let out = Command::new(crate::services::ffmpeg_bin())
            .args(["-hide_banner", "-hwaccels"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output();
        let Ok(out) = out else {
            return Vec::new();
        };
        let text = String::from_utf8_lossy(&out.stdout);
        // Brauchbare, generische Decode-Beschleuniger; reine Anzeige-/Wrapper-
        // Einträge ignorieren wir.
        const USABLE: [&str; 7] = [
            "videotoolbox",
            "cuda",
            "nvdec",
            "vaapi",
            "qsv",
            "d3d11va",
            "dxva2",
        ];
        text.lines()
            .map(|l| l.trim())
            .filter(|l| USABLE.contains(l))
            .map(|l| l.to_string())
            .collect()
    })
}

/// Die zu verwendende hwaccel-Methode bestimmen: bevorzugt die erzwungene
/// (Settings/Env), sonst die erste erkannte. None, wenn aus oder keine
/// verfügbar.
fn resolve_hwaccel(enabled: bool, forced: Option<&str>) -> HwMethod {
    if !enabled {
        return None;
    }
    if let Some(m) = forced {
        // Erzwungene Methode immer respektieren (auch wenn `-hwaccels` sie nicht
        // listet — manche Builds melden sie nicht, akzeptieren sie aber).
        return Some(m.to_string());
    }
    available_hwaccels().first().cloned()
}

/// `-hwaccel <method>` VOR `-i` ergänzen. Ohne `-hwaccel_output_format` lädt
/// ffmpeg die dekodierten Frames automatisch in den Systemspeicher zurück, so
/// dass die nachfolgende `scale,format=rgba`-Kette unverändert greift (sauberer
/// Fallback-freundlicher Pfad). Wird nichts gesetzt, bleibt es Software-Decode.
fn apply_hwaccel(cmd: &mut Command, hw: &HwMethod) {
    if let Some(method) = hw {
        cmd.args(["-hwaccel", method]);
    }
}

/// Reader-Thread: rohe RGBA-Frames aus der Decoder-Pipe in den Kanal.
fn spawn_frame_reader(
    mut stdout: std::process::ChildStdout,
    frame_size: usize,
    capacity: usize,
) -> Receiver<VideoFrame> {
    let (tx, rx) = sync_channel::<VideoFrame>(capacity.max(1));
    std::thread::spawn(move || {
        let mut index = 0u64;
        loop {
            let mut buf = vec![0u8; frame_size];
            let mut filled = 0;
            while filled < frame_size {
                match stdout.read(&mut buf[filled..]) {
                    Ok(0) => return, // EOF
                    Ok(n) => filled += n,
                    Err(_) => return,
                }
            }
            if tx.send(VideoFrame { index, data: buf }).is_err() {
                return; // Session beendet
            }
            index += 1;
        }
    });
    rx
}

struct VideoSession {
    path: String,
    w: i32,
    h: i32,
    fps: f64,
    /// Medienfortschritt pro Ausgabeframe-Sekunde (Clip-Speed; 0 = Standbild).
    speed: f64,
    /// Medienzeit des Frames mit Index 0 (Sekunden in der Quelldatei).
    start_media_time: f64,
    rx: Receiver<VideoFrame>,
    child: Child,
    last_frame_index: Option<u64>,
    started_at: f64,
    /// Mit Hardware-Decode gestartet (für die Fallback-Erkennung).
    hw: HwMethod,
    /// Rückstand (in Frames) beim letzten Tick — Drop-Zählung ohne Doppelzählen.
    prev_lag_frames: i64,
}

impl VideoSession {
    #[allow(clippy::too_many_arguments)]
    fn start(
        path: &str,
        media_time: f64,
        src_w: u32,
        src_h: u32,
        fps: f64,
        speed: f64,
        scale: f64,
        hw: &HwMethod,
        hdr: bool,
        now: f64,
    ) -> Option<VideoSession> {
        let fps = if fps > 0.0 { fps } else { 25.0 };
        let media_time = media_time.max(0.0);
        let (w, h) = decode_dims(src_w, src_h, scale);

        // Konstante Geschwindigkeit über dieselbe setpts-Kette wie der
        // Export (identische Frame-Auswahl); Standbild: genau ein Frame,
        // der gehalten wird.
        let setpts = crate::core::export::speed_setpts_filter(speed.max(f64::MIN_POSITIVE));
        let mut cmd = Command::new(crate::services::ffmpeg_bin());
        cmd.args(["-v", "error"]);
        apply_hwaccel(&mut cmd, hw);
        cmd.args(["-ss", &format!("{media_time:.4}")])
            .args(["-i", path])
            .args(["-an", "-sn"])
            .args(["-f", "rawvideo", "-pix_fmt", "rgba"]);
        // HDR-Quellen für die SDR-Vorschau tone-mappen (vor dem Skalieren).
        let tm = hdr_tonemap_prefix(hdr);
        if speed == 0.0 {
            cmd.args(["-vf", &format!("{tm}scale={w}:{h}")])
                .args(["-frames:v", "1"]);
        } else {
            cmd.args(["-vf", &format!("{tm}{setpts}scale={w}:{h}")])
                // Framerate-Konvertierung (Wiederholen/Auslassen) auf die Ziel-
                // rate; NTSC-Raten als exakter Bruch (30000/1001), kein Drift.
                .args(["-r", &crate::core::export::fps_arg(fps)]);
        }
        let mut child = cmd
            .arg("pipe:1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let stdout = child.stdout.take()?;
        let rx = spawn_frame_reader(stdout, (w * h * 4) as usize, 4);
        Some(VideoSession {
            path: path.to_string(),
            w,
            h,
            fps,
            speed,
            start_media_time: media_time,
            rx,
            child,
            last_frame_index: None,
            started_at: now,
            hw: hw.clone(),
            prev_lag_frames: 0,
        })
    }

    fn media_time_of(&self, index: u64) -> f64 {
        self.start_media_time + index as f64 * self.speed / self.fps
    }
}

impl Drop for VideoSession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Rückwärts-Wiedergabe: ffmpeg streamt nur vorwärts — ein Chunk VOR der
/// Zielzeit wird vorwärts dekodiert (gleiche setpts/Skalierungs-Kette) und
/// aus dem Frame-Puffer absteigend ausgeliefert; läuft die Zielzeit unter
/// den Chunk-Anfang, startet der nächste, frühere Chunk.
struct ReverseSession {
    path: String,
    w: i32,
    h: i32,
    fps: f64,
    /// |Medienfortschritt| pro Ausgabesekunde (Clip-Speed).
    speed: f64,
    /// Medienzeit von frames[0] (Chunk aufsteigend dekodiert).
    chunk_start: f64,
    /// Geplante Frame-Anzahl des Chunks.
    chunk_len: usize,
    frames: Vec<Vec<u8>>,
    rx: Receiver<VideoFrame>,
    child: Child,
    /// Zuletzt hochgeladener Frame-Index (Re-Uploads vermeiden).
    uploaded: Option<usize>,
    started_at: f64,
}

impl ReverseSession {
    #[allow(clippy::too_many_arguments)]
    fn start(
        path: &str,
        media_top: f64,
        src_w: u32,
        src_h: u32,
        fps: f64,
        speed: f64,
        scale: f64,
        hw: &HwMethod,
        hdr: bool,
        now: f64,
    ) -> Option<ReverseSession> {
        let fps = if fps > 0.0 { fps } else { 25.0 };
        let speed = speed.max(1e-6);
        let (w, h) = decode_dims(src_w, src_h, scale);
        let frame_bytes = (w * h * 4) as usize;
        // Puffer-Budget ≈ 96 MB — bei kleinen Vorschaugrößen längere Chunks.
        let want = (96 * 1024 * 1024 / frame_bytes.max(1)).clamp(2, 96);
        let step = speed / fps;
        let top = media_top.max(0.0);
        let chunk_start = (top - (want as f64 - 1.0) * step).max(0.0);
        let chunk_len = (((top - chunk_start) / step).round() as usize) + 1;

        let setpts = crate::core::export::speed_setpts_filter(speed);
        let mut cmd = Command::new(crate::services::ffmpeg_bin());
        cmd.args(["-v", "error"]);
        apply_hwaccel(&mut cmd, hw);
        let mut child = cmd
            .args(["-ss", &format!("{chunk_start:.4}")])
            .args(["-i", path])
            .args(["-an", "-sn"])
            .args(["-f", "rawvideo", "-pix_fmt", "rgba"])
            .args(["-vf", &format!("{}{setpts}scale={w}:{h}", hdr_tonemap_prefix(hdr))])
            .args(["-r", &crate::core::export::fps_arg(fps)])
            .args(["-frames:v", &chunk_len.to_string()])
            .arg("pipe:1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let stdout = child.stdout.take()?;
        // Kapazität = Chunk-Länge: ffmpeg kann durchlaufen und beenden.
        let rx = spawn_frame_reader(stdout, frame_bytes, chunk_len);
        Some(ReverseSession {
            path: path.to_string(),
            w,
            h,
            fps,
            speed,
            chunk_start,
            chunk_len,
            frames: Vec::new(),
            rx,
            child,
            uploaded: None,
            started_at: now,
        })
    }

    /// Medienzeit des höchsten Chunk-Frames.
    fn chunk_top(&self) -> f64 {
        self.chunk_start + (self.chunk_len as f64 - 1.0) * self.speed / self.fps
    }

    /// Eingehende Frames einsammeln; Frame-Index zur Zielzeit liefern,
    /// sofern er bereits dekodiert ist.
    fn frame_for(&mut self, media_t: f64) -> Option<usize> {
        while let Ok(fr) = self.rx.try_recv() {
            if fr.index as usize == self.frames.len() {
                self.frames.push(fr.data);
            }
        }
        if self.frames.is_empty() {
            return None;
        }
        let step = self.speed / self.fps;
        let idx = (((media_t - self.chunk_start) / step).round() as i64)
            .clamp(0, self.chunk_len as i64 - 1) as usize;
        Some(idx.min(self.frames.len() - 1))
    }
}

impl Drop for ReverseSession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Read-Ahead beim Pausieren: dekodiert ein zusammenhängendes Frame-Fenster
/// (vor UND nach dem Playhead) im Hintergrund und füllt damit den Frame-Cache,
/// damit Frame-Stepping (←/→) sofort aus dem Cache bedient wird. Eigener
/// Reader-Thread (wie die Wiedergabe-Sessions); pro Tick nicht-blockierend
/// geleert. Läuft genau einmal pro Fensterposition, dann beendet.
struct Prefetch {
    path: String,
    w: i32,
    h: i32,
    fps: f64,
    /// Medienzeit des Frames mit Index 0.
    start_media: f64,
    rx: Receiver<VideoFrame>,
    child: Child,
    /// Bereits dekodierte Frames dieses Fensters.
    received: u64,
    /// Geplante Frame-Anzahl des Fensters.
    count: u64,
}

impl Prefetch {
    fn start(
        path: &str,
        start_media: f64,
        w: i32,
        h: i32,
        fps: f64,
        count: u64,
        hw: &HwMethod,
    ) -> Option<Prefetch> {
        let fps = if fps > 0.0 { fps } else { 25.0 };
        let start_media = start_media.max(0.0);
        let mut cmd = Command::new(crate::services::ffmpeg_bin());
        cmd.args(["-v", "error"]);
        apply_hwaccel(&mut cmd, hw);
        let mut child = cmd
            .args(["-ss", &format!("{start_media:.4}")])
            .args(["-i", path])
            .args(["-an", "-sn"])
            .args(["-f", "rawvideo", "-pix_fmt", "rgba"])
            .args(["-vf", &format!("scale={w}:{h}")])
            .args(["-r", &crate::core::export::fps_arg(fps)])
            .args(["-frames:v", &count.to_string()])
            .arg("pipe:1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let stdout = child.stdout.take()?;
        let rx = spawn_frame_reader(stdout, (w * h * 4) as usize, count.max(1) as usize);
        Some(Prefetch {
            path: path.to_string(),
            w,
            h,
            fps,
            start_media,
            rx,
            child,
            received: 0,
            count,
        })
    }

    /// Bereitstehende Frames in den Cache schieben; `true`, wenn das Fenster
    /// fertig (oder die Pipe zu) ist und die Prefetch-Session verworfen werden
    /// kann.
    fn drain_into(&mut self, cache: &mut FrameCache) -> bool {
        loop {
            match self.rx.try_recv() {
                Ok(frame) => {
                    let media_time = self.start_media + frame.index as f64 / self.fps;
                    let key = FrameKey::at_time(&self.path, self.w, self.h, self.fps, media_time);
                    cache.insert(key, Arc::new(frame.data));
                    self.received = self.received.max(frame.index + 1);
                }
                Err(TryRecvError::Empty) => return self.received >= self.count,
                Err(TryRecvError::Disconnected) => return true,
            }
        }
    }
}

impl Drop for Prefetch {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Decoder-Session eines Programm-Layers: vorwärts (Pipe folgt dem
/// Playhead) oder rückwärts (Chunk + Frame-Puffer).
enum Session {
    Forward(VideoSession),
    Reverse(ReverseSession),
}

// ---------------------------------------------------------------- Audio
// `db_to_linear`/`pan_gains` leben in `core/audio_fx.rs` und werden von
// Player UND Export genutzt — so klingen Wiedergabe und Export gleich.

/// Glättet die Spur-Gains über einen Block (Rampe `from`→`to`, zipper-frei
/// bei Fader-/Automations-Sprüngen), misst den Spitzenpegel NACH dem Fader
/// und liefert (Peak L, Peak R).
fn apply_stereo_ramp(buf: &mut [f32], from: (f32, f32), to: (f32, f32)) -> (f32, f32) {
    let frames = buf.len() / AUDIO_CHANNELS;
    if frames == 0 {
        return (0.0, 0.0);
    }
    let inv = 1.0 / frames as f32;
    let (mut pl, mut pr) = (0f32, 0f32);
    for (i, fr) in buf.chunks_exact_mut(AUDIO_CHANNELS).enumerate() {
        let u = i as f32 * inv;
        let gl = from.0 + (to.0 - from.0) * u;
        let gr = from.1 + (to.1 - from.1) * u;
        fr[0] *= gl;
        fr[1] *= gr;
        pl = pl.max(fr[0].abs());
        pr = pr.max(fr[1].abs());
    }
    (pl, pr)
}

/// Summen-Ausgabestream. raylib-rs' `AudioStream::update` übergibt
/// fälschlich die Byte-Anzahl als frameCount an `UpdateAudioStream`;
/// raylib verwirft solche Writes („Attempting to write too many frames
/// to buffer“) und es kommt nie Ton an. Deshalb direkter FFI-Aufruf mit
/// korrekter Frame-Zahl und eigener Lebensdauer-Verwaltung.
struct MasterStream {
    raw: raylib::ffi::AudioStream,
    playing: bool,
}

impl MasterStream {
    fn new(audio: &'static RaylibAudio) -> MasterStream {
        // Sub-Buffer = Blockgröße: `is_processed` wird genau dann true,
        // wenn ein kompletter Mix-Block nachgefüllt werden kann.
        audio.set_audio_stream_buffer_size_default(AUDIO_CHUNK_FRAMES as i32);
        let stream = audio.new_audio_stream(AUDIO_RATE, 32, AUDIO_CHANNELS as u32);
        let raw = unsafe { stream.inner() };
        MasterStream {
            raw,
            playing: false,
        }
    }

    /// Mindestens ein Sub-Buffer ist abgespielt und kann gefüllt werden.
    fn is_processed(&self) -> bool {
        unsafe { raylib::ffi::IsAudioStreamProcessed(self.raw) }
    }

    /// Einen Block (interleaved L/R) schreiben und Wiedergabe sicherstellen.
    fn write(&mut self, interleaved: &[f32]) {
        debug_assert_eq!(interleaved.len() % AUDIO_CHANNELS, 0);
        unsafe {
            raylib::ffi::UpdateAudioStream(
                self.raw,
                interleaved.as_ptr() as *const std::ffi::c_void,
                (interleaved.len() / AUDIO_CHANNELS) as i32,
            );
            if !self.playing {
                raylib::ffi::PlayAudioStream(self.raw);
                self.playing = true;
            }
        }
    }

    /// Stream anhalten und gepufferte Reste verwerfen, damit der Ton bei
    /// Pause sofort stoppt statt ~2 Blöcke nachzulaufen.
    fn flush_stop(&mut self) {
        if self.playing {
            unsafe { raylib::ffi::StopAudioStream(self.raw) };
            self.playing = false;
        }
    }
}

impl Drop for MasterStream {
    fn drop(&mut self) {
        unsafe { raylib::ffi::UnloadAudioStream(self.raw) };
    }
}

/// ffmpeg-Audio-Pipe: dekodiert ab `media_time` mit pitch-korrigiertem Tempo
/// `tempo` (atempo-Kette wie der Export) und liefert f32le-Stereo (48 kHz) als
/// Chunks über einen Kanal. `max_media` begrenzt die Quell-Spanne (Reverse-
/// Chunks / Clip-Spanne), `None` dekodiert bis zum Dateiende.
fn spawn_audio_pipe(
    path: &str,
    media_time: f64,
    tempo: f64,
    max_media: Option<f64>,
) -> Option<(Child, Receiver<Vec<f32>>)> {
    let media_time = media_time.max(0.0);
    let mut cmd = Command::new(crate::services::ffmpeg_bin());
    cmd.args(["-v", "error", "-ss", &format!("{media_time:.4}")]);
    if let Some(t) = max_media {
        if t.is_finite() && t > 0.0 {
            cmd.args(["-t", &format!("{t:.4}")]);
        }
    }
    cmd.args(["-i", path]).args(["-vn", "-sn"]);
    if let Some(chain) = crate::core::export::atempo_chain(tempo) {
        cmd.args(["-filter:a", &chain]);
    }
    let mut child = cmd
        .args([
            "-f",
            "f32le",
            "-ac",
            &AUDIO_CHANNELS.to_string(),
            "-ar",
            &AUDIO_RATE.to_string(),
        ])
        .arg("pipe:1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = sync_channel::<Vec<f32>>(8);
    std::thread::spawn(move || {
        let chunk_bytes = AUDIO_CHUNK_FRAMES * AUDIO_CHANNELS * 4;
        loop {
            let mut buf = vec![0u8; chunk_bytes];
            let mut filled = 0;
            while filled < chunk_bytes {
                match stdout.read(&mut buf[filled..]) {
                    Ok(0) => {
                        if filled > 0 {
                            let _ = tx.send(bytes_to_f32(&buf[..filled]));
                        }
                        return;
                    }
                    Ok(n) => filled += n,
                    Err(_) => return,
                }
            }
            if tx.send(bytes_to_f32(&buf)).is_err() {
                return;
            }
        }
    });
    Some((child, rx))
}

/// Stereo-Puffer in-place umkehren (Frame-weise, L/R erhalten).
fn reverse_stereo(buf: &mut [f32]) {
    let frames = buf.len() / AUDIO_CHANNELS;
    for i in 0..frames / 2 {
        let a = i * AUDIO_CHANNELS;
        let b = (frames - 1 - i) * AUDIO_CHANNELS;
        for c in 0..AUDIO_CHANNELS {
            buf.swap(a + c, b + c);
        }
    }
}

/// Vorwärts-Audio-Quelle: ein ffmpeg-Prozess, Samples laufen vorwärts.
struct AudioFwd {
    child: Child,
    rx: Receiver<Vec<f32>>,
    eof: bool,
}

impl Drop for AudioFwd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Hintergrund-Decode eines Reverse-Chunks (vorwärts dekodiert, wird nach EOF
/// umgekehrt).
struct RevJob {
    child: Child,
    rx: Receiver<Vec<f32>>,
    accum: Vec<f32>,
    eof: bool,
}

impl Drop for RevJob {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Rückwärts-Audio-Quelle (Profi-NLE-Scrubbing-Charakter): dekodiert
/// Medien-Chunks VORWÄRTS (ab fallender Obergrenze), kehrt die Samples um und
/// liefert sie absteigend aus — segmentiertes Vorwärts-Decoding mit
/// Umkehr-Puffer, doppelt gepuffert (kein Aussetzer an Chunk-Grenzen).
struct AudioRev {
    path: String,
    tempo: f64,
    /// Obere Mediengrenze des NÄCHSTEN zu dekodierenden Chunks (läuft abwärts).
    media_hi: f64,
    /// Medienspanne eines Chunks (= Chunk-Output-Frames / Rate × Tempo).
    span: f64,
    job: Option<RevJob>,
    ready: std::collections::VecDeque<Vec<f32>>,
    /// Keine weiteren Chunks mehr (Medienanfang erreicht).
    done: bool,
}

impl AudioRev {
    fn new(path: &str, tempo: f64, media_top: f64) -> AudioRev {
        let span = REVERSE_AUDIO_CHUNK_FRAMES as f64 / AUDIO_RATE as f64 * tempo.max(1e-6);
        AudioRev {
            path: path.to_string(),
            tempo,
            media_hi: media_top.max(0.0),
            span,
            job: None,
            ready: std::collections::VecDeque::new(),
            done: media_top <= 0.0,
        }
    }

    /// Laufenden Chunk-Decode vorantreiben und ggf. den nächsten anstoßen.
    fn pump(&mut self) {
        if let Some(job) = self.job.as_mut() {
            loop {
                match job.rx.try_recv() {
                    Ok(c) => job.accum.extend(c),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        job.eof = true;
                        break;
                    }
                }
            }
            if job.eof {
                let mut acc = std::mem::take(&mut job.accum);
                self.job = None; // Child beenden (Drop)
                reverse_stereo(&mut acc);
                if !acc.is_empty() {
                    self.ready.push_back(acc);
                }
            }
        }
        if self.job.is_none() && !self.done && self.ready.len() < 2 {
            let hi = self.media_hi;
            if hi <= 0.0 {
                self.done = true;
                return;
            }
            let lo = (hi - self.span).max(0.0);
            match spawn_audio_pipe(&self.path, lo, self.tempo, Some(hi - lo)) {
                Some((child, rx)) => {
                    self.job = Some(RevJob {
                        child,
                        rx,
                        accum: Vec::new(),
                        eof: false,
                    });
                    self.media_hi = lo;
                    if lo <= 0.0 {
                        self.done = true;
                    }
                }
                None => self.done = true,
            }
        }
    }

    fn take_chunk(&mut self) -> Option<Vec<f32>> {
        self.ready.pop_front()
    }

    fn finished(&self) -> bool {
        self.done && self.job.is_none() && self.ready.is_empty()
    }
}

/// Vorwärts- oder Rückwärts-Samplequelle eines Clips/der Quelle.
enum AudioSrc {
    Forward(AudioFwd),
    Reverse(AudioRev),
    /// In-Memory-Quelle (nur Tests): liefert ihren Puffer einmalig blockweise.
    #[cfg(test)]
    Mem { data: Vec<f32>, pos: usize },
}

/// Ein hörbarer Clip (oder Quellmonitor) im Streaming-Mixdown. Platzierung
/// erfolgt sample-genau auf einer globalen Output-Frame-Achse: das erste
/// Sample sitzt bei `oa`, das letzte vor `ob`. Lautstärke-Keyframes und
/// Übergangs-Fades werden per Sub-Block in MEDIEN-/SEQUENZZEIT ausgewertet
/// (identisch zum Export), harte Schnittkanten erhalten eine Anti-Klick-Rampe.
struct ClipAudio {
    clip_id: String,
    /// Spur des Clips (None = Quellmonitor) — Ziel der Pegelmessung/Bus.
    track_id: Option<String>,
    path: String,
    /// atempo-Faktor = clip-speed × |rate| (Shuttle), für (Re-)Start-Vergleich.
    tempo: f64,
    /// Medienrichtung relativ zur Output-Achse (Reverse-Quelle).
    media_backward: bool,
    /// Output-Frame des ENTER-Rands (erstes Sample, global) und EXIT-Rands.
    oa: i64,
    ob: i64,
    /// Bereits verbrauchte Output-Frames seit `oa`.
    emitted: i64,
    /// Sequenzzeit bei `oa` und d(Sequenz)/d(Output-Frame).
    enter_seq: f64,
    seq_per_out: f64,
    /// Medienzeit bei `oa` und d(Medien)/d(Output-Frame) (Hüllkurven-Mapping).
    media_enter: f64,
    media_per_out: f64,
    /// Clip-Grund-Gain (dB) + Lautstärke-Keyframes (Medienzeit) + Fades.
    base_gain_db: f64,
    vol: crate::core::animation::AnimatedParam,
    fades: Vec<(f64, f64, bool, bool)>,
    /// Anti-Klick-Rampe an der Start-/Endkante anwenden. Bei lückenlos
    /// medienkontinuierlichen Nachbarclips (Razor-Schnitt durchgehenden Tons)
    /// abgeschaltet — sonst entstünde an jeder Schnittkante eine Lautstärke-Delle.
    ramp_start: bool,
    ramp_end: bool,
    src: AudioSrc,
    buf: Vec<f32>,
    eof: bool,
    fx_chain: Option<AudioFxChain>,
    /// Aktuelle Audio-Effekt-Instanzen (zum Nachführen animierter Parameter in
    /// `fill`, ohne sie pro Tick durchreichen zu müssen).
    fx_effects: Vec<EffectInstance>,
    /// In `fill` bereits durch die FX-Kette verarbeitete Output-Frames seit
    /// Clip-Eintritt — bestimmt die Medienzeit, an der die FX-Parameter
    /// nachgeführt werden (Keyframe-Automation), exakt wie der Export.
    fx_pos: i64,
}

impl ClipAudio {
    /// Effekt-Kette mit dem Clip-Zustand abgleichen (Code wie zuvor).
    fn sync_effects(&mut self, effects: &[EffectInstance], media_t: f64) {
        // Instanzen merken — `fill` führt animierte Parameter sub-blockweise nach.
        self.fx_effects = effects.to_vec();
        let refs: Vec<&EffectInstance> = effects.iter().collect();
        let active = refs.iter().any(|e| e.enabled && e.kind.is_audio());
        if !active {
            self.fx_chain = None;
            return;
        }
        match self.fx_chain.as_mut() {
            Some(chain) if chain.matches(&refs) => chain.retune(&refs, media_t),
            _ => self.fx_chain = AudioFxChain::build(&refs, AUDIO_RATE, AUDIO_CHANNELS, media_t),
        }
    }

    /// `self.buf` auf ≥ `want` Frames füllen (FX einmalig in Output-Reihenfolge
    /// anwenden); bricht bei leerer Pipe oder EOF ab.
    fn fill(&mut self, want: usize) {
        let want_samples = want * AUDIO_CHANNELS;
        while self.buf.len() < want_samples && !self.eof {
            let mut chunk = match &mut self.src {
                AudioSrc::Forward(f) => match f.rx.try_recv() {
                    Ok(c) => c,
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        f.eof = true;
                        self.eof = true;
                        break;
                    }
                },
                AudioSrc::Reverse(r) => {
                    r.pump();
                    match r.take_chunk() {
                        Some(c) => c,
                        None => {
                            if r.finished() {
                                self.eof = true;
                            }
                            break;
                        }
                    }
                }
                #[cfg(test)]
                AudioSrc::Mem { data, pos } => {
                    if *pos >= data.len() {
                        self.eof = true;
                        break;
                    }
                    let end = (*pos + AUDIO_CHUNK_FRAMES * AUDIO_CHANNELS).min(data.len());
                    let chunk = data[*pos..end].to_vec();
                    *pos = end;
                    chunk
                }
            };
            // FX in Sub-Blöcken (≤ ENV_BLOCK) verarbeiten und je Block an der
            // exakten Medienzeit nachführen — sonst friert animierte Clip-Audio-
            // FX-Automation in der Wiedergabe am Eintrittswert ein (der Export
            // führt dieselben Parameter alle ENV_BLOCK_FRAMES nach: Parität).
            let total = chunk.len() / AUDIO_CHANNELS;
            let refs: Vec<&EffectInstance> = self.fx_effects.iter().collect();
            if let Some(chain) = self.fx_chain.as_mut() {
                let mut off = 0usize;
                while off < total {
                    let n = (total - off).min(ENV_BLOCK_FRAMES);
                    let media_t =
                        (self.media_enter + self.fx_pos as f64 * self.media_per_out).max(0.0);
                    chain.retune(&refs, media_t);
                    let s = off * AUDIO_CHANNELS;
                    let e = (off + n) * AUDIO_CHANNELS;
                    chain.process(&mut chunk[s..e]);
                    self.fx_pos += n as i64;
                    off += n;
                }
            } else {
                // Position auch ohne aktive FX mitführen, damit ein FX-Toggle
                // mitten in der Wiedergabe die Automation phasenrichtig fortsetzt.
                self.fx_pos += total as i64;
            }
            self.buf.extend(chunk);
        }
    }

    /// Verpasste Frames aus dem Puffer entfernen (FX-Zustand bleibt stetig).
    fn drop_frames(&mut self, n: usize) {
        self.fill(n);
        let d = (n * AUDIO_CHANNELS).min(self.buf.len());
        self.buf.drain(..d);
    }

    /// Grund-Gain × Lautstärke-Keyframes × Übergangs-Fade an (media, seq).
    fn env_gain(&self, media: f64, seq: f64) -> f32 {
        let mut g = db_to_linear(self.base_gain_db + self.vol.eval(media));
        for (w0, w1, fade_in, eq) in &self.fades {
            if seq >= *w0 && seq < *w1 && *w1 > *w0 {
                let p = (seq - *w0) / (*w1 - *w0);
                g *= crate::core::transitions::audio_gain(*eq, *fade_in, p) as f32;
            }
        }
        g
    }

    /// Anti-Klick-Rampe (0..1) am Output-Frame `gi` (0 = ENTER-Rand): linearer
    /// Auf-/Abbau über `CLICK_RAMP_FRAMES` an beiden harten Kanten.
    fn edge_ramp(&self, gi: i64) -> f32 {
        let len = self.ob - self.oa;
        let r = CLICK_RAMP_FRAMES as f32;
        let mut g = 1.0f32;
        if self.ramp_start && gi < CLICK_RAMP_FRAMES as i64 {
            g = g.min((gi as f32 + 0.5) / r);
        }
        let from_end = len - 1 - gi;
        if self.ramp_end && from_end < CLICK_RAMP_FRAMES as i64 {
            g = g.min((from_end as f32 + 0.5) / r);
        }
        g.clamp(0.0, 1.0)
    }

    /// Den Clip sample-genau in den Block `[block_out, block_out+frames)`
    /// (interleaved) mischen. Liefert (gemischte Frames, Peak L, Peak R).
    fn mix_block(&mut self, out: &mut [f32], block_out: i64) -> (usize, f32, f32) {
        let frames = (out.len() / AUDIO_CHANNELS) as i64;
        let block_lo = block_out;
        let block_hi = block_out + frames;
        let mut cur = self.oa + self.emitted;
        // Hinter der Render-Kante (Stall) — verpasste Frames überspringen.
        if cur < block_lo {
            let skip = (block_lo - cur) as usize;
            self.drop_frames(skip);
            self.emitted += skip as i64;
            cur = self.oa + self.emitted;
        }
        if cur >= block_hi || cur >= self.ob {
            return (0, 0.0, 0.0);
        }
        let start_off = (cur - block_lo) as usize;
        let avail_to_end = (self.ob - cur).max(0) as usize;
        let room = (frames as usize).saturating_sub(start_off);
        let want = room.min(avail_to_end);
        if want == 0 {
            return (0, 0.0, 0.0);
        }
        self.fill(want);
        let have = (self.buf.len() / AUDIO_CHANNELS).min(want);
        if have == 0 {
            return (0, 0.0, 0.0);
        }
        let (mut peak_l, mut peak_r) = (0f32, 0f32);
        let mut done = 0usize;
        while done < have {
            let n = (have - done).min(ENV_BLOCK_FRAMES);
            // Hüllkurve am Mittelpunkt des Sub-Blocks auswerten.
            let frame_mid = (self.emitted + done as i64) as f64 + n as f64 * 0.5;
            let media_t = (self.media_enter + frame_mid * self.media_per_out).max(0.0);
            let seq_t = self.enter_seq + frame_mid * self.seq_per_out;
            let env = self.env_gain(media_t, seq_t);
            for i in 0..n {
                let gi = self.emitted + (done + i) as i64;
                let g = env * self.edge_ramp(gi);
                let bi = (done + i) * AUDIO_CHANNELS;
                let oi = (start_off + done + i) * AUDIO_CHANNELS;
                let l = self.buf[bi] * g;
                let r = self.buf[bi + 1] * g;
                out[oi] += l;
                out[oi + 1] += r;
                peak_l = peak_l.max(l.abs());
                peak_r = peak_r.max(r.abs());
            }
            done += n;
        }
        self.buf.drain(..have * AUDIO_CHANNELS);
        self.emitted += have as i64;
        (have, peak_l, peak_r)
    }
}

fn bytes_to_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

/// Platzierungsfertige Audio-Quelle eines Clips/der Quelle für diesen Tick.
struct Want {
    clip_id: String,
    track_id: Option<String>,
    path: String,
    /// atempo-Faktor = clip-speed × |rate|.
    tempo: f64,
    media_backward: bool,
    /// Output-Frame des ersten/letzten Samples (global, Decoder-Platzierung).
    oa: i64,
    ob: i64,
    enter_seq: f64,
    seq_per_out: f64,
    media_enter: f64,
    media_per_out: f64,
    base_gain_db: f64,
    vol: crate::core::animation::AnimatedParam,
    fades: Vec<(f64, f64, bool, bool)>,
    ramp_start: bool,
    ramp_end: bool,
    effects: Vec<EffectInstance>,
}

/// Stoßen zwei Audio-Clips lückenlos UND medienkontinuierlich aneinander
/// (Razor-Schnitt durch durchgehenden Ton)? Dann erzeugt die Schnittkante
/// keinen Knackser — die Anti-Klick-Rampe entfällt (sonst Lautstärke-Delle an
/// jedem Schnitt). Nur Vorwärts-Clips gleicher Geschwindigkeit/Quelle.
fn audio_seamless(
    timeline: &crate::core::timeline::TimelineStore,
    clip: &TimelineClip,
    at_end: bool,
) -> bool {
    if clip.reverse || clip.freeze {
        return false;
    }
    let eps = 1e-4;
    timeline.clips.iter().any(|o| {
        o.id != clip.id
            && o.enabled
            && o.track_id == clip.track_id
            && o.asset_id == clip.asset_id
            && !o.reverse
            && !o.freeze
            && (o.eff_speed() - clip.eff_speed()).abs() < 1e-6
            && if at_end {
                (o.start - clip.end()).abs() < eps && (o.media_in() - clip.media_out()).abs() < eps
            } else {
                (o.end() - clip.start).abs() < eps && (o.media_out() - clip.media_in()).abs() < eps
            }
    })
}

/// Sanfter Slew-Schritt (Sekunden) zur Drift `drift` (gehört − Playhead):
/// Proportionalterm, pro Tick begrenzt — Delay-Locked-Loop gegen die
/// gerätegetaktete Audio-Uhr, ohne sichtbares Springen des Playheads.
fn slew_step(drift: f64) -> f64 {
    (drift * SLEW_GAIN).clamp(-MAX_SLEW_PER_TICK, MAX_SLEW_PER_TICK)
}

/// Output-Frame-Bereich [lo, hi] (lo ≤ hi), in dem die Sequenz-/Quellspanne
/// [a0, a1] ausgegeben wird (richtungsbewusst über die Uhr).
fn clock_out_range(clock: TargetClock, a0: f64, a1: f64) -> (i64, i64) {
    let o0 = clock.out_of(a0);
    let o1 = clock.out_of(a1);
    (o0.min(o1).round() as i64, o0.max(o1).round() as i64)
}

/// Want eines Programm-Clips bauen, falls er im Render-/Vorlauf-Fenster liegt.
#[allow(clippy::too_many_arguments)]
fn program_clip_want(
    clock: TargetClock,
    block_lo: i64,
    spawn_hi: i64,
    clip: &TimelineClip,
    track_id: String,
    path: &str,
    timeline: &crate::core::timeline::TimelineStore,
    fades: &[(f64, f64, bool, bool)],
    a0: f64,
    a1: f64,
    backward: bool,
    tempo: f64,
) -> Option<Want> {
    let rate_f = AUDIO_RATE as f64;
    let (cut_lo, ob) = clock_out_range(clock, a0, a1);
    if ob <= block_lo || cut_lo > spawn_hi {
        return None;
    }
    // Bereits laufende Clips (Seek hinein) starten an der Render-Kante.
    let oa = cut_lo.max(block_lo);
    let enter_seq = clock.anchor_pos + (oa - clock.anchor_out as i64) as f64 * clock.rate / rate_f;
    let media_enter = clip.media_time_at(enter_seq).max(0.0);
    // Kontinuität erst HIER prüfen (O(Clips)), wenn der Clip wirklich im
    // Fenster liegt. Beim Hineinspringen (oa > cut_lo) ist die Startkante
    // immer eine Diskontinuität → Rampe; bei natürlichem Start nur, wenn kein
    // lückenloser Vorgänger anschließt. Die Endkante richtet sich nach dem
    // Nachfolger.
    let ramp_start = oa > cut_lo || !audio_seamless(timeline, clip, false);
    let cont_end = audio_seamless(timeline, clip, true);
    Some(Want {
        clip_id: clip.id.clone(),
        track_id: Some(track_id),
        path: path.to_string(),
        tempo,
        media_backward: backward,
        oa,
        ob,
        enter_seq,
        seq_per_out: clock.rate / rate_f,
        media_enter,
        media_per_out: if backward {
            -tempo / rate_f
        } else {
            tempo / rate_f
        },
        base_gain_db: clip.gain_db,
        vol: clip.fx.volume_db.clone(),
        fades: fades.to_vec(),
        ramp_start,
        ramp_end: !cont_end,
        effects: clip
            .effects
            .iter()
            .filter(|e| e.kind.is_audio())
            .cloned()
            .collect(),
    })
}


/// ClipAudio aus einem Want bauen (Vorwärts-Pipe bzw. Reverse-Quelle starten).
fn build_clip_audio(w: &Want) -> Option<ClipAudio> {
    let src = if w.media_backward {
        AudioSrc::Reverse(AudioRev::new(&w.path, w.tempo, w.media_enter))
    } else {
        let span = ((w.ob - w.oa).max(0) as f64) / AUDIO_RATE as f64 * w.tempo + 1.0;
        let (child, rx) = spawn_audio_pipe(&w.path, w.media_enter, w.tempo, Some(span))?;
        AudioSrc::Forward(AudioFwd {
            child,
            rx,
            eof: false,
        })
    };
    let mut clip = ClipAudio {
        clip_id: w.clip_id.clone(),
        track_id: w.track_id.clone(),
        path: w.path.clone(),
        tempo: w.tempo,
        media_backward: w.media_backward,
        oa: w.oa,
        ob: w.ob,
        emitted: 0,
        enter_seq: w.enter_seq,
        seq_per_out: w.seq_per_out,
        media_enter: w.media_enter,
        media_per_out: w.media_per_out,
        base_gain_db: w.base_gain_db,
        vol: w.vol.clone(),
        fades: w.fades.clone(),
        ramp_start: w.ramp_start,
        ramp_end: w.ramp_end,
        src,
        buf: Vec::new(),
        eof: false,
        fx_chain: None,
        fx_effects: Vec::new(),
        fx_pos: 0,
    };
    clip.sync_effects(&w.effects, w.media_enter);
    Some(clip)
}

/// Scrubbing-Stimme: kurzes Vorwärts-Grain aus dem obersten hörbaren
/// Audio-Clip am Playhead (ungeregelt in den Master, Clip-Grund-Gain; eine
/// Anti-Klick-Hüllkurve fenstert das Grain über `edge_ramp`).
fn build_scrub_voice(state: &AppState, pos: f64, master_out: u64) -> Option<ClipAudio> {
    let solo_any = state.timeline.tracks.iter().any(|tr| tr.solo);
    let mut chosen: Option<&TimelineClip> = None;
    for track in state.timeline.tracks.iter().filter(|tr| {
        tr.kind == TrackKind::Audio && !tr.muted && (!solo_any || tr.solo)
    }) {
        for clip in state.timeline.clips.iter().filter(|c| {
            c.track_id == track.id && c.enabled && c.media_step() != 0.0
        }) {
            if pos >= clip.start && pos < clip.end() {
                chosen = Some(clip); // spätere Spur „gewinnt“ (oberste Lage)
            }
        }
    }
    let clip = chosen?;
    let asset = state.media.asset(&clip.asset_id)?;
    let use_proxy = state.media.use_proxies;
    if !asset.preview_playable(use_proxy) || asset.info.audio.is_empty() {
        return None;
    }
    let tempo = clip.eff_speed();
    let media = compose::clip_media_time(clip, pos).max(0.0);
    let grain = (SCRUB_GRAIN_SEC * AUDIO_RATE as f64) as i64;
    let oa = master_out as i64;
    let path = asset.decode_path(use_proxy).to_string();
    let (child, rx) = spawn_audio_pipe(&path, media, tempo, Some(SCRUB_GRAIN_SEC * tempo + 0.2))?;
    Some(ClipAudio {
        clip_id: "scrub".into(),
        track_id: None,
        path,
        tempo,
        media_backward: false,
        oa,
        ob: oa + grain,
        emitted: 0,
        enter_seq: pos,
        seq_per_out: 1.0 / AUDIO_RATE as f64,
        media_enter: media,
        media_per_out: tempo / AUDIO_RATE as f64,
        base_gain_db: clip.gain_db,
        vol: crate::core::animation::AnimatedParam::fixed(0.0),
        fades: Vec::new(),
        ramp_start: true,
        ramp_end: true,
        src: AudioSrc::Forward(AudioFwd {
            child,
            rx,
            eof: false,
        }),
        buf: Vec::new(),
        eof: false,
        fx_chain: None,
        fx_effects: Vec::new(),
        fx_pos: 0,
    })
}

// ---------------------------------------------------------------- Engine

/// Zielzustand eines Monitors in diesem Tick.
#[derive(Clone)]
struct VideoTarget {
    path: String,
    media_time: f64,
    src_w: u32,
    src_h: u32,
    fps: f64,
    playing: bool,
    rate: f64,
    scale: f64,
    /// Medienfortschritt pro Sequenzsekunde (signiert; 0 = Standbild) —
    /// aus `TimelineClip::media_step()`, Quelle des Decoder-Tempos.
    media_step: f64,
    /// Quelle ist HDR (PQ/HLG) und wird aus dem Original dekodiert ⇒ für die
    /// SDR-Vorschau tone-gemappt (zscale/tonemap). Bei Proxy-Decode false.
    hdr: bool,
}

/// Filter-Präfix, das HDR (PQ/HLG, BT.2020) für die SDR-Vorschau tonemappt:
/// linearisieren → Hable-Tonemap → BT.709/limited. Leer für SDR-Quellen.
/// Braucht ffmpeg mit `zscale` (libzimg); fehlt es, schlägt der Decode mit
/// diesem Filter fehl und der Layer bleibt beim letzten Frame (Fallback).
fn hdr_tonemap_prefix(hdr: bool) -> &'static str {
    if hdr {
        "zscale=t=linear:npl=100,tonemap=hable,zscale=t=bt709:m=bt709:r=tv,"
    } else {
        ""
    }
}

/// Querschnitts-Zustände, die `drive_video` je Layer braucht: Frame-Cache,
/// aufgelöste hwaccel-Methode + Fehlschlag-Set (Fallback) und ein Drop-Zähler.
struct DriveCtx<'a> {
    cache: &'a mut FrameCache,
    /// Für DIESEN Pfad aufgelöste Hardware-Decode-Methode (None = Software).
    hw: HwMethod,
    /// Pfade mit fehlgeschlagenem Hardware-Decode (für den Fallback gemerkt).
    hw_failed: &'a mut std::collections::HashSet<String>,
    /// In diesem Tick verworfene Frames (über alle Layer summiert).
    drops: &'a mut u32,
}

/// hwaccel für einen konkreten Decode-Pfad wählen: aus, wenn global aus oder
/// der Pfad zuvor in Hardware scheiterte (dauerhafter Software-Fallback).
fn pick_hw(base: &HwMethod, failed: &std::collections::HashSet<String>, path: &str) -> HwMethod {
    if base.is_none() || failed.contains(path) {
        None
    } else {
        base.clone()
    }
}

/// Audio-getaktete Uhr eines Wiedergabeziels (Programm/Quelle): verankert die
/// Position an der gerätegetakteten Render-Kante. Der Playhead wird per
/// Slewing (Delay-Locked-Loop) sanft an die gehörte Audio-Position gezogen —
/// Bild folgt Ton, keine akkumulierende Drift über lange Wiedergaben.
#[derive(Clone, Copy)]
struct TargetClock {
    /// Globaler Output-Frame am Anker (Wert von `master_out` beim Verankern).
    anchor_out: u64,
    /// Position (Sequenz- bzw. Quell-Sekunde) am Anker.
    anchor_pos: f64,
    /// Signierter Wiedergabefaktor (rate) seit dem Anker.
    rate: f64,
    /// Mindestens ein echter Audio-Block geschrieben — Uhr gültig (Slew aktiv).
    primed: bool,
}

impl TargetClock {
    /// Sequenz-/Quellzeit der nächsten zu rendernden Output-Frame (Render-Kante).
    fn render_pos(&self, master_out: u64) -> f64 {
        self.anchor_pos
            + (master_out as i64 - self.anchor_out as i64) as f64 * self.rate / AUDIO_RATE as f64
    }
    /// Aktuell GEHÖRTE Position (Render-Kante minus Geräte-Latenz).
    fn heard_pos(&self, master_out: u64) -> f64 {
        let played =
            (master_out as i64 - NOMINAL_LATENCY_FRAMES as i64 - self.anchor_out as i64).max(0);
        self.anchor_pos + played as f64 * self.rate / AUDIO_RATE as f64
    }
    /// Globaler Output-Frame, an dem Position `pos` ausgegeben wird.
    fn out_of(&self, pos: f64) -> f64 {
        self.anchor_out as f64 + (pos - self.anchor_pos) * AUDIO_RATE as f64 / self.rate
    }
}

pub struct PlayerEngine {
    master: Option<MasterStream>,
    /// Ein Decoder je sichtbarem Programm-Layer (Schlüssel = Clip-ID).
    program_videos: std::collections::HashMap<String, Option<Session>>,
    source_video: Option<Session>,
    /// Decoder des Sequenz-Render-Caches (Programmbild aus der Cache-Datei,
    /// während der Wiedergabe gecachter Bereiche — statt N Layer-Decoder).
    rendercache_video: Option<Session>,
    /// RAM-begrenzter LRU-Cache dekodierter RGBA-Frames (Scrub/Read-Ahead).
    frame_cache: FrameCache,
    /// Read-Ahead-Sessions je Programm-Clip (Fenster um den Playhead).
    prefetch: std::collections::HashMap<String, Prefetch>,
    /// Zuletzt prefetchtes Fenster-Zentrum (Decode-Frame) je Clip — verhindert
    /// ständiges Neu-Prefetchen an derselben Stelle.
    prefetched_at: std::collections::HashMap<String, i64>,
    /// Decode-Pfade, bei denen Hardware-Decode fehlschlug → dauerhaft Software.
    hw_failed: std::collections::HashSet<String>,
    /// Playhead beim letzten Tick (Änderungserkennung fürs Read-Ahead).
    last_playhead: f64,
    /// Koalesziert Scrub-Anfragen auf die zuletzt angefragte Position und
    /// debounct das Read-Ahead, bis der Playhead steht (statt jeden Maus-Tick
    /// vorauszudekodieren).
    scrub_coalesce: ScrubCoalescer,
    /// Verworfene Video-Frames seit Start (Überlast) + Ringfenster der letzten
    /// ~2 s für den Monitor-Indikator.
    drops_total: u64,
    drops_recent: std::collections::VecDeque<(f64, u32)>,
    /// Streaming-Mixer-Stimmen (Programm-Clips + Quelle), sample-genau platziert.
    audio_clips: Vec<ClipAudio>,
    /// Geschriebene Output-Frames seit Streamstart (gerätegetaktete Uhr).
    master_out: u64,
    /// Audio-Uhr von Programm bzw. Quelle (None = inaktiv → Playhead wall-clock).
    prog_clock: Option<TargetClock>,
    src_clock: Option<TargetClock>,
    /// Aktive Audio-Scrubbing-Stimme (kurzes Grain am Playhead).
    scrub: Option<ClipAudio>,
    /// Sequenzzeit des letzten Scrub-Grains (Refresh-Drosselung).
    scrub_last_pos: f64,
    /// Wiederverwendeter Master-Mix-Block (interleaved L/R).
    mix_buf: Vec<f32>,
    /// Wiederverwendeter Per-Spur-Buffer (Clips einer Spur werden hier
    /// summiert, bevor Spur-FX + Spur-Gain/Pan greifen).
    track_buf: Vec<f32>,
    /// Bus-Effekt-Kette je Audio-Spur (Schlüssel = Spur-ID), Zustände bleiben
    /// über Blöcke hinweg. Pro Tick mit dem Spur-Zustand synchronisiert.
    track_fx: std::collections::HashMap<String, AudioFxChain>,
    /// Geglättete Spur-Gains (L, R) je Spur — Rampenstart des nächsten Blocks.
    track_gain_smooth: std::collections::HashMap<String, (f32, f32)>,
    /// EDITRON_AUDIO_DEBUG=1: einmal pro Sekunde Mix-Statistik auf stderr.
    debug: bool,
    debug_last: f64,
    debug_blocks: u64,
    /// Ticks, in denen ein freier Sub-Buffer mangels Decoder-Daten leer blieb.
    debug_starved: u64,
    debug_ticks: u64,
    /// Letzte gemessene Slew-Korrektur (Sekunden) für das Debug-Log.
    debug_slew: f64,
    /// Zuletzt für einen sichtbaren Nest-Clip gerendertes (innere Sequenz,
    /// Frame-Index, w, h) — überspringt das (teure) Neurendern bei stehendem
    /// Playhead. Schlüssel = Clip-ID.
    nest_sig: std::collections::HashMap<String, (String, i64, u32, u32)>,
    /// Signatur des zuletzt CPU-komponierten Programmbildes (Adjustment-Layer-
    /// Vorschau): (timeline-Revision, media-Revision, Frame-Index, Inhalts-Hash,
    /// w, h) — überspringt das Neukomponieren bei stehendem Playhead/unverändertem
    /// Inhalt. Der Inhalts-Hash deckt Live-Gesten ab (Grade-/fx-Slider bumpen
    /// `revision` NICHT). None ⇒ kein Adjustment aktiv / Textur freigegeben.
    program_composite_sig: Option<(u64, u64, i64, u64, u32, u32)>,
    /// BS.1770-Lautheitsmesser des Master-Mixblocks (Mixer-Metering).
    loudness: crate::core::loudness::LoudnessMeter,
    /// Playhead-Position des zuletzt in den Lautheitsmesser gefütterten Blocks
    /// — unterscheidet Fortsetzen nach Pause (Integrated bleibt) von einem Seek
    /// (Integrated startet neu).
    loudness_last_pos: f64,
}

impl PlayerEngine {
    pub fn new() -> PlayerEngine {
        // Audio-Gerät einmalig initialisieren; 'static über Box::leak, weil
        // der Master-Stream die RaylibAudio-Referenz überleben muss.
        let audio = RaylibAudio::init_audio_device()
            .ok()
            .map(|a| &*Box::leak(Box::new(a)));
        if audio.is_none() {
            eprintln!("[player] Audio-Gerät konnte nicht initialisiert werden — Wiedergabe ohne Ton");
        }
        PlayerEngine {
            master: audio.map(MasterStream::new),
            program_videos: Default::default(),
            source_video: None,
            rendercache_video: None,
            frame_cache: FrameCache::new(
                crate::core::settings::DEFAULT_FRAME_CACHE_MB as usize * 1024 * 1024,
            ),
            prefetch: Default::default(),
            prefetched_at: Default::default(),
            hw_failed: Default::default(),
            last_playhead: f64::NAN,
            scrub_coalesce: ScrubCoalescer::default(),
            drops_total: 0,
            drops_recent: Default::default(),
            audio_clips: Vec::new(),
            master_out: 0,
            prog_clock: None,
            src_clock: None,
            scrub: None,
            scrub_last_pos: f64::NAN,
            mix_buf: Vec::new(),
            track_buf: Vec::new(),
            track_fx: Default::default(),
            track_gain_smooth: Default::default(),
            debug: std::env::var("EDITRON_AUDIO_DEBUG").is_ok(),
            debug_last: 0.0,
            debug_blocks: 0,
            debug_starved: 0,
            debug_ticks: 0,
            debug_slew: 0.0,
            nest_sig: Default::default(),
            program_composite_sig: None,
            loudness: crate::core::loudness::LoudnessMeter::new(AUDIO_RATE, AUDIO_CHANNELS),
            loudness_last_pos: f64::NAN,
        }
    }

    /// Sichtbare Nest-Clips rekursiv komponieren und als Clip-Textur
    /// hochladen. Reuse des Export-Compositing-Kerns ([`compose::
    /// composite_sequence_frame`]) → Vorschau und Export sind pixelgleich. Die
    /// inneren Blatt-Frames werden per Einzelbild-Extraktion geholt; das
    /// Ergebnis wird je (innere Sequenz, Frame, Größe) zwischengespeichert,
    /// damit ein stehender Playhead nicht jedes Tick neu rendert.
    fn render_nest_previews(
        &mut self,
        rl: &mut RaylibHandle,
        thread: &RaylibThread,
        state: &AppState,
        textures: &mut TextureCache,
    ) {
        let t = state.timeline.playhead_sec;
        let seq_fps = state.timeline.settings.rate.fps().max(1.0);
        let nests: Vec<(String, String, f64)> =
            compose::visible_program_layers(&state.timeline, t)
                .into_iter()
                .filter_map(|layer| match layer {
                    compose::ProgramLayer::Clip { clip, .. } => clip
                        .nest_seq
                        .clone()
                        .map(|n| (clip.id.clone(), n, compose::nest_inner_time(clip, t))),
                    compose::ProgramLayer::Solid { .. }
                    | compose::ProgramLayer::Adjustment { .. } => None,
                })
                .collect();
        let mut alive: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (clip_id, inner_id, inner_t) in nests {
            alive.insert(clip_id.clone());
            let Some(inner) = state.timeline.timeline_of(&inner_id) else {
                continue;
            };
            let (iw, ih) = (inner.settings.width.max(2), inner.settings.height.max(2));
            // Vorschau-Auflösung deckeln (Tempo), inneres Seitenverhältnis wahren.
            let s = (960.0f32 / iw as f32).min(1.0);
            let w = ((iw as f32 * s).round() as usize).max(2);
            let h = ((ih as f32 * s).round() as usize).max(2);
            let frame_idx = (inner_t * seq_fps).round() as i64;
            let sig = (inner_id.clone(), frame_idx, w as u32, h as u32);
            if self.nest_sig.get(&clip_id) == Some(&sig) {
                continue;
            }
            if let Some(data) = compose_nest_preview(state, &inner_id, inner_t, w, h) {
                upload_frame(
                    rl,
                    thread,
                    textures,
                    &clip_texture_key(&clip_id),
                    w as i32,
                    h as i32,
                    &data,
                    None,
                );
                self.nest_sig.insert(clip_id, sig);
            }
        }
        // Nicht mehr sichtbare Nest-Clips vergessen (Neurender beim Wiederkehren).
        self.nest_sig.retain(|id, _| alive.contains(id));
    }

    /// Programmbild CPU-komponieren, wenn am Playhead ein Adjustment Layer
    /// aktiv ist (Korrektur-Pass auf das Gesamtbild — von der GPU-Per-Layer-
    /// Pipeline nicht abbildbar). Nutzt den geteilten Compositing-Kern
    /// (`composite_sequence_frame`, formelgleich zum Export); das Ergebnis liegt
    /// unter `PROGRAM_COMPOSITE_KEY`, signaturgecacht (kein Neukomponieren bei
    /// stehendem Playhead). Setzt `state.monitor.program_adjustment` für den
    /// Monitor (zeichnet dann diese eine Vollbild-Textur).
    fn render_adjustment_preview(
        &mut self,
        rl: &mut RaylibHandle,
        thread: &RaylibThread,
        state: &mut AppState,
        textures: &mut TextureCache,
    ) {
        use std::hash::{Hash, Hasher};
        let t = state.timeline.playhead_sec;
        let layers = compose::visible_program_layers(&state.timeline, t);
        let active = layers
            .iter()
            .any(|l| matches!(l, compose::ProgramLayer::Adjustment { .. }));
        state.monitor.program_adjustment = active;
        if !active {
            if self.program_composite_sig.is_some() {
                self.program_composite_sig = None;
                textures.remove(PROGRAM_COMPOSITE_KEY);
            }
            return;
        }
        // Inhalts-Hash der sichtbaren Clips: Grade/fx/Effekte/Blend — fängt
        // Live-Gesten (Slider/Gizmo) ab, die `revision` NICHT bumpen, sodass die
        // Vorschau beim Ziehen live aktualisiert (wie der GPU-Pfad).
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for layer in &layers {
            let clip = match layer {
                compose::ProgramLayer::Clip { clip, .. } => *clip,
                compose::ProgramLayer::Adjustment { clip } => *clip,
                compose::ProgramLayer::Solid { .. } => continue,
            };
            if let Ok(s) = serde_json::to_string(&(
                &clip.grade,
                &clip.fx,
                &clip.effects,
                clip.blend_mode,
                clip.enabled,
            )) {
                s.hash(&mut hasher);
            }
        }
        let content_hash = hasher.finish();
        drop(layers);
        let seq_fps = state.timeline.settings.rate.fps().max(1.0);
        let (sw, sh) = (
            state.timeline.settings.width.max(2),
            state.timeline.settings.height.max(2),
        );
        // Vorschau-Auflösung deckeln (Tempo des synchronen Blatt-Decodes),
        // Seitenverhältnis der Sequenz wahren.
        let s = (1280.0f32 / sw as f32).min(1.0);
        let w = ((sw as f32 * s).round() as usize).max(2);
        let h = ((sh as f32 * s).round() as usize).max(2);
        let frame_idx = (t * seq_fps).round() as i64;
        let sig = (
            state.timeline.revision,
            state.media.revision,
            frame_idx,
            content_hash,
            w as u32,
            h as u32,
        );
        if self.program_composite_sig == Some(sig) {
            return;
        }
        if let Some(data) = compose_program_preview(state, t, w, h) {
            upload_frame(
                rl,
                thread,
                textures,
                PROGRAM_COMPOSITE_KEY,
                w as i32,
                h as i32,
                &data,
                None,
            );
            self.program_composite_sig = Some(sig);
        }
    }

    /// Pro Frame im Mainloop (vor dem Zeichnen) aufrufen.
    pub fn tick(
        &mut self,
        rl: &mut RaylibHandle,
        thread: &RaylibThread,
        state: &mut AppState,
        textures: &mut TextureCache,
        now: f64,
    ) {
        // Cache-Budget aus den Einstellungen synchronisieren (live änderbar).
        let budget = state.settings.frame_cache_budget_bytes();
        if self.frame_cache.budget() != budget {
            self.frame_cache.set_budget(budget);
        }
        // Hardware-Decode-Methode für diesen Tick auflösen (gecachte Erkennung).
        let hw_base = resolve_hwaccel(
            state.settings.hwaccel,
            state.settings.hwaccel_method.as_deref(),
        );

        // Scrub-Anfragen koaleszieren: nur Playhead-Änderungen anmelden; das
        // Read-Ahead startet erst, wenn die Position `PREFETCH_SETTLE` lang
        // steht (Debounce — kein Prefetch während aktiven Scrubbens).
        let ph = state.timeline.playhead_sec;
        if self.last_playhead.is_nan() || (ph - self.last_playhead).abs() > 1e-9 {
            self.scrub_coalesce.request(ph, now);
        }
        self.last_playhead = ph;
        let settled = self
            .scrub_coalesce
            .take_settled(now, PREFETCH_SETTLE)
            .is_some();

        let program_targets = program_video_targets(state);
        let source_target = source_video_target(state);

        // Sequenz-Render-Cache: Gültigkeit auffrischen (billig bei
        // unveränderter Revision) und prüfen, ob ein gültiges Segment den
        // Playhead abdeckt — dann liefert EIN Cache-Decoder das Programmbild
        // statt N Layer-Decoder (nur während der Wiedergabe; beim Pausieren
        // bleibt Live-Compositing aktiv, damit Bearbeiten/Gizmo gehen und das
        // Scrubbing aus dem Frame-Cache kommt).
        state.render_cache.refresh(&state.timeline, &state.media);
        let cache_target = render_cache_target(state);
        let from_cache = cache_target.is_some();
        state.monitor.program_from_cache = from_cache;

        // Bei Cache-Wiedergabe keine Layer treiben (leere Zielliste → Sessions
        // + Texturen werden aufgeräumt, kein ffmpeg je Layer).
        let empty: Vec<(String, VideoTarget)> = Vec::new();
        let prog_targets: &[(String, VideoTarget)] = if from_cache { &empty } else { &program_targets };
        let mut drops = self.drive_videos(
            rl,
            thread,
            state,
            textures,
            prog_targets,
            source_target,
            &hw_base,
            now,
        );

        // Verschachtelte Sequenzen für die Vorschau rekursiv komponieren und
        // als Clip-Textur hochladen (nur im Live-Compositing, nicht bei
        // Cache-Wiedergabe). Identischer Compositing-Kern wie der Export.
        if !from_cache {
            self.render_nest_previews(rl, thread, state, textures);
            self.render_adjustment_preview(rl, thread, state, textures);
        } else {
            // Cache-Wiedergabe: das gecachte Programmbild enthält den Adjustment-
            // Pass bereits (Export-Pfad) — Live-CPU-Composite abräumen.
            state.monitor.program_adjustment = false;
            if self.program_composite_sig.is_some() {
                self.program_composite_sig = None;
                textures.remove(PROGRAM_COMPOSITE_KEY);
            }
        }

        // Programmbild aus dem Render-Cache (oder Cache-Decoder freigeben).
        if let Some(ct) = cache_target {
            let cache = &mut self.frame_cache;
            let hw_failed = &mut self.hw_failed;
            let hw = pick_hw(&hw_base, hw_failed, &ct.path);
            let mut ctx = DriveCtx {
                cache,
                hw,
                hw_failed,
                drops: &mut drops,
            };
            Self::drive_video(
                rl,
                thread,
                &mut self.rendercache_video,
                textures,
                RENDER_CACHE_KEY,
                Some(ct),
                &mut ctx,
                now,
                None,
            );
        } else if self.rendercache_video.is_some() {
            self.rendercache_video = None;
            textures.remove(RENDER_CACHE_KEY);
        }

        // Read-Ahead beim Pausieren (nur ohne Cache-Wiedergabe — sonst würde es
        // gerade abgeräumte Layer-Decoder neu starten).
        if from_cache {
            self.prefetch.clear();
            self.prefetched_at.clear();
        } else {
            self.drive_prefetch(&program_targets, &hw_base, settled);
        }

        // ---- Audio (Programm + Quelle) ----
        self.drive_audio(state, now);

        // Telemetrie zurück in den Store (Indikator/Overlay im Monitor).
        self.record_drops(drops, now);
        self.write_perf(state);
    }

    /// Treibt alle Video-Decoder (Programm-Layer + Quelle) für diesen Tick;
    /// liefert die Zahl der in diesem Tick verworfenen Frames.
    #[allow(clippy::too_many_arguments)]
    fn drive_videos(
        &mut self,
        rl: &mut RaylibHandle,
        thread: &RaylibThread,
        state: &mut AppState,
        textures: &mut TextureCache,
        program_targets: &[(String, VideoTarget)],
        source_target: Option<VideoTarget>,
        hw_base: &HwMethod,
        now: f64,
    ) -> u32 {
        // Nicht mehr sichtbare Layer beenden und ihre Ressourcen freigeben.
        let wanted: std::collections::HashSet<&str> =
            program_targets.iter().map(|(id, _)| id.as_str()).collect();
        {
            let minis = &mut state.monitor.preview_frames;
            let prefetch = &mut self.prefetch;
            let prefetched_at = &mut self.prefetched_at;
            self.program_videos.retain(|clip_id, _| {
                let keep = wanted.contains(clip_id.as_str());
                if !keep {
                    textures.remove(&clip_texture_key(clip_id));
                    minis.remove(clip_id);
                    prefetch.remove(clip_id);
                    prefetched_at.remove(clip_id);
                }
                keep
            });
        }

        let mut drops = 0u32;
        // Programm-Layer.
        {
            let minis = &mut state.monitor.preview_frames;
            let cache = &mut self.frame_cache;
            let program_videos = &mut self.program_videos;
            let hw_failed = &mut self.hw_failed;
            for (clip_id, target) in program_targets {
                let hw = pick_hw(hw_base, hw_failed, &target.path);
                let session = program_videos.entry(clip_id.clone()).or_default();
                let mut ctx = DriveCtx {
                    cache: &mut *cache,
                    hw,
                    hw_failed: &mut *hw_failed,
                    drops: &mut drops,
                };
                Self::drive_video(
                    rl,
                    thread,
                    session,
                    textures,
                    &clip_texture_key(clip_id),
                    Some(target.clone()),
                    &mut ctx,
                    now,
                    Some((&mut *minis, clip_id)),
                );
            }
        }

        // Quellmonitor.
        {
            let cache = &mut self.frame_cache;
            let hw_failed = &mut self.hw_failed;
            let hw = source_target
                .as_ref()
                .map(|t| pick_hw(hw_base, hw_failed, &t.path))
                .unwrap_or(None);
            let mut ctx = DriveCtx {
                cache,
                hw,
                hw_failed,
                drops: &mut drops,
            };
            Self::drive_video(
                rl,
                thread,
                &mut self.source_video,
                textures,
                SOURCE_KEY,
                source_target,
                &mut ctx,
                now,
            None,
            );
        }

        drops
    }

    #[allow(clippy::too_many_arguments)]
    fn drive_video(
        rl: &mut RaylibHandle,
        thread: &RaylibThread,
        session: &mut Option<Session>,
        textures: &mut TextureCache,
        key: &str,
        target: Option<VideoTarget>,
        ctx: &mut DriveCtx,
        now: f64,
        // Ziel für die downgesampelte Frame-Kopie (Scopes): (Map, clip_id).
        mini_out: Option<(&mut std::collections::HashMap<String, crate::stores::MiniFrame>, &str)>,
    ) {
        let Some(target) = target else {
            *session = None;
            textures.remove(key);
            return;
        };
        if target.media_step < 0.0 {
            Self::drive_video_reverse(rl, thread, session, textures, key, &target, ctx, now, mini_out);
            return;
        }
        let fwd_speed = target.media_step; // ≥ 0; 0 = Standbild
        let (dw, dh) = decode_dims(target.src_w, target.src_h, target.scale);
        // Scrubbing/Pausiert (keine laufende Vorwärtswiedergabe) → Cache zuerst.
        let scrubbing = !target.playing || target.rate == 0.0;

        // 1. Cache-Treffer beim Scrubben: Frame ohne Decode anzeigen. Der
        //    Decoder bleibt unangetastet — das Read-Ahead füllt die Nachbarn,
        //    auf einem gecachten Standbild läuft kein ffmpeg.
        if scrubbing {
            let fk = FrameKey::at_time(&target.path, dw, dh, target.fps, target.media_time);
            if let Some(data) = ctx.cache.get(&fk) {
                upload_frame(rl, thread, textures, key, dw, dh, &data, mini_out);
                return;
            }
        }

        // 2. Cache-Miss (oder Wiedergabe): Decoder sicherstellen. Die Reuse-vs-
        //    Restart-Entscheidung ist in `frame_cache::seek_decision` herausgelöst
        //    (kleine Vorwärtssprünge lesen weiter statt neu aufzusetzen).
        let needs_restart = match session.as_ref() {
            Some(Session::Forward(s)) => {
                let (expected_w, _) = decode_dims(target.src_w, target.src_h, target.scale);
                let decoded_time = s
                    .last_frame_index
                    .map(|i| s.media_time_of(i))
                    .unwrap_or(s.start_media_time);
                let decision = seek_decision(
                    decoded_time,
                    target.media_time,
                    s.fps,
                    fwd_speed,
                    target.playing,
                    target.rate,
                );
                s.path != target.path
                    || s.w != expected_w
                    || (s.speed - fwd_speed).abs() > 1e-6
                    || (decision == SeekAction::Restart
                        && now - s.started_at > SCRUB_RESTART_INTERVAL)
            }
            _ => true,
        };
        if needs_restart {
            *session = VideoSession::start(
                &target.path,
                target.media_time,
                target.src_w,
                target.src_h,
                target.fps,
                fwd_speed,
                target.scale,
                &ctx.hw,
                target.hdr,
                now,
            )
            .map(Session::Forward);
        }

        // 2b. Hardware-Decode-Fallback: HW-Session, deren Prozess ohne ein
        //     einziges Frame endet → als gescheitert vormerken und in Software
        //     neu starten (Frame kommt dann diesen oder nächsten Tick).
        if let Some(Session::Forward(s)) = session.as_mut() {
            if s.hw.is_some()
                && s.last_frame_index.is_none()
                && matches!(s.child.try_wait(), Ok(Some(_)))
            {
                ctx.hw_failed.insert(s.path.clone());
                let path = s.path.clone();
                let start = s.start_media_time;
                *session = VideoSession::start(
                    &path,
                    start,
                    target.src_w,
                    target.src_h,
                    target.fps,
                    fwd_speed,
                    target.scale,
                    &None,
                    target.hdr,
                    now,
                )
                .map(Session::Forward);
            }
        }

        let Some(Session::Forward(s)) = session.as_mut() else { return };

        // Frames bis zur Zielzeit konsumieren; jeden in den Cache legen; den
        // letzten hochladen.
        let mut latest: Option<Arc<Vec<u8>>> = None;
        let half_step = if s.speed > 0.0 { s.speed / s.fps * 0.5 } else { 0.0 };
        loop {
            let next_index = s.last_frame_index.map(|i| i + 1).unwrap_or(0);
            let next_time = s.media_time_of(next_index);
            // Pausiert: genau einen Frame anzeigen (den an der Position).
            let want = if latest.is_none() && s.last_frame_index.is_none() {
                true // immer den ersten Frame holen
            } else {
                next_time <= target.media_time + half_step
            };
            if !want {
                break;
            }
            match s.rx.try_recv() {
                Ok(frame) => {
                    s.last_frame_index = Some(frame.index);
                    // Dekodierten Frame in den LRU-Cache (auch wenn er gleich
                    // wieder vom nächsten überholt/„gedroppt“ wird — er ist beim
                    // Zurückscrubben sofort da statt verworfen).
                    let mt = s.media_time_of(frame.index);
                    let arc = Arc::new(frame.data);
                    ctx.cache.insert(
                        FrameKey::at_time(&s.path, s.w, s.h, s.fps, mt),
                        arc.clone(),
                    );
                    latest = Some(arc);
                }
                Err(_) => break,
            }
        }
        if let Some(arc) = &latest {
            upload_frame(rl, thread, textures, key, s.w, s.h, arc, mini_out);
        }

        // Drop-Telemetrie (nur Vorwärtswiedergabe): der Zuwachs des Decoder-
        // Rückstands gegenüber dem letzten Tick = in diesem Tick neu verworfene
        // Frames (Bild fällt unter den Playhead zurück = Stocken).
        if target.playing && fwd_speed > 0.0 && target.rate != 0.0 {
            let decoded_time = s
                .last_frame_index
                .map(|i| s.media_time_of(i))
                .unwrap_or(s.start_media_time);
            let lag = (((target.media_time - decoded_time) * s.fps).floor() as i64).max(0);
            *ctx.drops += (lag - s.prev_lag_frames).max(0) as u32;
            s.prev_lag_frames = lag;
        } else {
            s.prev_lag_frames = 0;
        }
    }

    /// Rückwärts laufende Clips: Chunk vor der Zielzeit dekodieren und aus
    /// dem Frame-Puffer absteigend ausliefern.
    #[allow(clippy::too_many_arguments)]
    fn drive_video_reverse(
        rl: &mut RaylibHandle,
        thread: &RaylibThread,
        session: &mut Option<Session>,
        textures: &mut TextureCache,
        key: &str,
        target: &VideoTarget,
        ctx: &mut DriveCtx,
        now: f64,
        mini_out: Option<(&mut std::collections::HashMap<String, crate::stores::MiniFrame>, &str)>,
    ) {
        let speed = -target.media_step;
        let needs_restart = match session.as_ref() {
            Some(Session::Reverse(s)) => {
                let (expected_w, _) = decode_dims(target.src_w, target.src_h, target.scale);
                let step = s.speed / s.fps.max(1.0);
                let below = target.media_time < s.chunk_start - 0.5 * step;
                let above = target.media_time > s.chunk_top() + 0.5 * step;
                s.path != target.path
                    || s.w != expected_w
                    || (s.speed - speed).abs() > 1e-6
                    // Unter den Chunk gelaufen: nahtlos früheren Chunk laden;
                    // Sprung nach oben (Scrub): gedrosselt neu aufsetzen.
                    || below
                    || (above && now - s.started_at > SCRUB_RESTART_INTERVAL)
            }
            _ => true,
        };
        if needs_restart {
            *session = ReverseSession::start(
                &target.path,
                target.media_time,
                target.src_w,
                target.src_h,
                target.fps,
                speed,
                target.scale,
                &ctx.hw,
                target.hdr,
                now,
            )
            .map(Session::Reverse);
        }
        let Some(Session::Reverse(s)) = session.as_mut() else { return };
        if let Some(idx) = s.frame_for(target.media_time) {
            if s.uploaded != Some(idx) {
                s.uploaded = Some(idx);
                upload_frame(rl, thread, textures, key, s.w, s.h, &s.frames[idx], mini_out);
            }
            // Angezeigten Frame auch in den gemeinsamen Cache (Vorwärts-Stepping
            // bzw. Scrubbing in dieser Region trifft ihn dann sofort).
            let mt = s.chunk_start + idx as f64 * s.speed / s.fps;
            let fk = FrameKey::at_time(&s.path, s.w, s.h, s.fps, mt);
            if !ctx.cache.contains(&fk) {
                ctx.cache.insert(fk, Arc::new(s.frames[idx].clone()));
            }
        }
    }

    /// Read-Ahead beim Pausieren: bereits laufende Prefetch-Fenster in den
    /// Cache leeren und — sobald der Playhead steht — für jeden pausierten
    /// Programm-Layer ein Fenster [Playhead−R, Playhead+R] vorausdekodieren,
    /// damit Frame-Stepping in beide Richtungen sofort aus dem Cache kommt.
    fn drive_prefetch(
        &mut self,
        targets: &[(String, VideoTarget)],
        hw_base: &HwMethod,
        settled: bool,
    ) {
        // Disjunkte Feld-Borrows.
        let Self {
            prefetch,
            frame_cache,
            prefetched_at,
            hw_failed,
            ..
        } = self;

        // Laufende Prefetches füttern; fertige verwerfen.
        prefetch.retain(|_, pf| !pf.drain_into(frame_cache));

        if !settled {
            return;
        }
        for (clip_id, target) in targets {
            // Nur pausiert und nur normale Vorwärts-Clips (Standbild/Reverse
            // brauchen kein Fenster — Standbild ist EIN Frame, Reverse puffert
            // bereits einen Chunk).
            let paused = !target.playing || target.rate == 0.0;
            if !paused || target.media_step <= 0.0 {
                continue;
            }
            if prefetch.contains_key(clip_id) {
                continue;
            }
            let (dw, dh) = decode_dims(target.src_w, target.src_h, target.scale);
            let fps = if target.fps > 0.0 { target.fps } else { 25.0 };
            let center = (target.media_time * fps).round() as i64;
            if prefetched_at.get(clip_id) == Some(&center) {
                continue; // an dieser Stelle bereits prefetcht
            }
            let start_frame = (center - PREFETCH_RADIUS).max(0);
            let end_frame = center + PREFETCH_RADIUS;
            let fps_milli = (fps * 1000.0).round() as u32;
            // Fenster schon vollständig im Cache? dann nur Marke setzen.
            let all_cached = (start_frame..=end_frame).all(|f| {
                frame_cache.contains(&FrameKey {
                    path: target.path.clone(),
                    w: dw,
                    h: dh,
                    frame: f,
                    fps_milli,
                })
            });
            if all_cached {
                prefetched_at.insert(clip_id.clone(), center);
                continue;
            }
            let hw = pick_hw(hw_base, hw_failed, &target.path);
            let count = (end_frame - start_frame + 1) as u64;
            let start_media = start_frame as f64 / fps;
            if let Some(pf) = Prefetch::start(&target.path, start_media, dw, dh, fps, count, &hw) {
                prefetch.insert(clip_id.clone(), pf);
                prefetched_at.insert(clip_id.clone(), center);
            }
        }
    }

    /// Drop-Zähler fortschreiben + Ringfenster der letzten ~2 s pflegen.
    fn record_drops(&mut self, n: u32, now: f64) {
        if n > 0 {
            self.drops_total += n as u64;
            self.drops_recent.push_back((now, n));
        }
        while let Some(&(t, _)) = self.drops_recent.front() {
            if now - t > 2.0 {
                self.drops_recent.pop_front();
            } else {
                break;
            }
        }
    }

    /// Player-seitige Performance-Telemetrie in den Monitor-Store spiegeln.
    /// Decode-/Upload-/Frame-Zeiten füllt der Mainloop (er misst sie).
    fn write_perf(&self, state: &mut AppState) {
        let p = &mut state.monitor.perf;
        p.dropped_total = self.drops_total;
        p.dropped_recent = self.drops_recent.iter().map(|(_, n)| *n).sum();
        p.cache_hits = self.frame_cache.hits();
        p.cache_misses = self.frame_cache.misses();
        p.cache_used_mb = self.frame_cache.used_bytes() as f32 / (1024.0 * 1024.0);
        p.cache_entries = self.frame_cache.len() as u64;
    }

    fn drive_audio(&mut self, state: &mut AppState, now: f64) {
        if self.master.is_none() {
            return;
        }

        // Manueller Lautheits-Reset (Knopf im Mixer): Integrated + True-Peak-
        // Max-Hold neu beginnen.
        if state.audio.loudness_reset {
            self.loudness.reset();
            self.loudness_last_pos = f64::NAN;
            state.audio.loudness_reset = false;
        }

        let rate_f = AUDIO_RATE as f64;
        // Proxy-Modus: Audio-Vorschau ebenfalls aus dem Proxy (durchgereichtes
        // PCM); der Export nutzt unverändert die Originale.
        let use_proxy = state.media.use_proxies;
        let mut wants: Vec<Want> = Vec::new();
        // Per-Spur-Zielgains (Spur-Gain/Pan inkl. Automation) und aktive Spuren.
        let mut track_targets: Vec<(String, (f32, f32))> = Vec::new();
        let mut active_track_ids: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        // ---- Programm-Uhr verankern / Seek erkennen -------------------------
        let prog_rate = state.playback.program_rate;
        let playhead = state.timeline.playhead_sec;
        let solo_any = state.timeline.tracks.iter().any(|tr| tr.solo);
        let timeline_has_audible = state.timeline.tracks.iter().any(|tr| {
            tr.kind == TrackKind::Audio
                && !tr.muted
                && (!solo_any || tr.solo)
                && state
                    .timeline
                    .clips
                    .iter()
                    .any(|c| c.track_id == tr.id && c.enabled && c.media_step() != 0.0)
        });
        let prog_audio = state.playback.program_playing
            && timeline_has_audible
            && prog_rate != 0.0
            && prog_rate.abs() <= MAX_SHUTTLE_AUDIO_RATE + 1e-6;
        if prog_audio {
            let reanchor = match self.prog_clock {
                None => true,
                Some(c) => {
                    (c.rate - prog_rate).abs() > 1e-9
                        || (playhead - c.heard_pos(self.master_out)).abs() > AUDIO_RESYNC_TOLERANCE
                }
            };
            if reanchor {
                // Bei echtem Positionssprung (Seek) die Integrated-Messung neu
                // beginnen; ein Fortsetzen nach Pause (neue Anker-Position ≈
                // zuletzt gemessen) behält sie.
                let seek = match self.prog_clock {
                    Some(c) => {
                        (playhead - c.heard_pos(self.master_out)).abs() > AUDIO_RESYNC_TOLERANCE
                    }
                    None => {
                        !self.loudness_last_pos.is_finite()
                            || (playhead - self.loudness_last_pos).abs() > LOUDNESS_RESET_TOLERANCE
                    }
                };
                if seek {
                    self.loudness.reset();
                }
                self.prog_clock = Some(TargetClock {
                    anchor_out: self.master_out,
                    anchor_pos: playhead,
                    rate: prog_rate,
                    primed: false,
                });
                // Programm-Decoder (oa im alten Anker-Raum) verwerfen.
                self.audio_clips.retain(|c| c.track_id.is_none());
            }
        } else if self.prog_clock.is_some() {
            self.prog_clock = None;
            self.audio_clips.retain(|c| c.track_id.is_none());
        }

        // ---- Programm-Wants (Clips im Render-/Vorlauf-Fenster) --------------
        if let Some(clock) = self.prog_clock {
            let render_seq = clock.render_pos(self.master_out);
            let block_lo = self.master_out as i64;
            let spawn_hi = block_lo + (AUDIO_PREROLL * rate_f) as i64;
            for track in state
                .timeline
                .tracks
                .iter()
                .filter(|tr| tr.kind == TrackKind::Audio && !tr.muted && (!solo_any || tr.solo))
            {
                active_track_ids.insert(track.id.clone());
                // Spur-Gain/Pan inkl. Automation (in Sequenzzeit der Render-Kante).
                let tg = db_to_linear(track.gain_db_at(render_seq));
                let (pl, pr) = pan_gains(track.pan_at(render_seq));
                track_targets.push((track.id.clone(), (tg * pl, tg * pr)));
                if track.has_audio_effects() {
                    let refs: Vec<&EffectInstance> = track.effects.iter().collect();
                    match self.track_fx.get_mut(&track.id) {
                        Some(chain) if chain.matches(&refs) => chain.retune(&refs, render_seq),
                        _ => match AudioFxChain::build(&refs, AUDIO_RATE, AUDIO_CHANNELS, render_seq)
                        {
                            Some(c) => {
                                self.track_fx.insert(track.id.clone(), c);
                            }
                            None => {
                                self.track_fx.remove(&track.id);
                            }
                        },
                    }
                } else {
                    self.track_fx.remove(&track.id);
                }
                for clip in state
                    .timeline
                    .clips
                    .iter()
                    .filter(|c| c.track_id == track.id && c.enabled)
                {
                    let ms = clip.media_step();
                    if ms == 0.0 {
                        continue; // Standbild stumm (beide Richtungen)
                    }
                    // Time-Remap (variables Tempo) ist stumm — Parität zum
                    // Export-Plan, der animierte Clips überspringt (eine
                    // konstante atempo-Kette bildet die Kurve nicht ab).
                    if clip.is_time_remapped() {
                        continue;
                    }
                    // Parität zum Export (mix_clips_into_wav schweigt für
                    // Reverse-Clips): bei VORWÄRTS-Wiedergabe sind als reverse
                    // markierte Clips stumm — identisch zum gerenderten Ergebnis.
                    // Bei RÜCKWÄRTS-Transport (reine Vorschau, kein Export-
                    // Äquivalent) klingt dagegen die ganze Spur rückwärts.
                    if prog_rate > 0.0 && clip.reverse {
                        continue;
                    }
                    let fades = state.timeline.audio_fades(clip);
                    let (a0, a1) = state.timeline.audio_extent(clip, &fades);
                    let net = ms.signum() * prog_rate.signum();
                    // Multicam: Audio-Winkel auflösen — synthetisches Blatt mit
                    // Winkel-Asset und um `pos` verschobenem `src_in` (damit
                    // `media_time_at` die Asset-Medienzeit liefert). Die Sequenz-
                    // zeiten (start/duration/fades) bleiben unverändert.
                    let mc_leaf: Option<crate::core::timeline::TimelineClip> =
                        if let Some(mc) = &clip.multicam {
                            let Some(src) = state
                                .timeline
                                .timeline_of(&mc.source)
                                .and_then(|t| t.multicam.as_ref())
                            else {
                                continue;
                            };
                            let aidx = src.audio_angle_idx(mc.angle);
                            let Some(angle) = src.angles.get(aidx).filter(|a| a.has_audio) else {
                                continue;
                            };
                            let mut leaf = clip.clone();
                            leaf.multicam = None;
                            leaf.asset_id = angle.asset_id.clone();
                            leaf.src_in = clip.src_in - angle.pos;
                            Some(leaf)
                        } else {
                            None
                        };
                    let want_clip = mc_leaf.as_ref().unwrap_or(clip);
                    let Some(asset) = state.media.asset(&want_clip.asset_id) else {
                        continue;
                    };
                    if !asset.preview_playable(use_proxy) {
                        continue;
                    }
                    if let Some(w) = program_clip_want(
                        clock,
                        block_lo,
                        spawn_hi,
                        want_clip,
                        track.id.clone(),
                        asset.decode_path(use_proxy),
                        &state.timeline,
                        &fades,
                        a0,
                        a1,
                        net < 0.0,
                        clip.eff_speed() * prog_rate.abs(),
                    ) {
                        wants.push(w);
                    }
                }
            }
        }
        // Stale Bus-FX-/Glättungszustände verwerfen.
        self.track_fx.retain(|id, _| active_track_ids.contains(id));
        self.track_gain_smooth
            .retain(|id, _| active_track_ids.contains(id));

        // ---- Quell-Uhr verankern + Want -----------------------------------
        let src_rate = state.playback.source.rate;
        let src_pos = state.playback.source.position;
        let src_asset = state
            .playback
            .source_asset_id
            .as_ref()
            .and_then(|id| state.media.asset(id));
        let src_audio = state.playback.source.playing
            && src_rate != 0.0
            && src_rate.abs() <= MAX_SHUTTLE_AUDIO_RATE + 1e-6
            && src_asset.is_some_and(|a| !a.info.audio.is_empty() && a.preview_playable(use_proxy));
        if src_audio {
            let reanchor = match self.src_clock {
                None => true,
                Some(c) => {
                    (c.rate - src_rate).abs() > 1e-9
                        || (src_pos - c.heard_pos(self.master_out)).abs() > AUDIO_RESYNC_TOLERANCE
                }
            };
            if reanchor {
                self.src_clock = Some(TargetClock {
                    anchor_out: self.master_out,
                    anchor_pos: src_pos,
                    rate: src_rate,
                    primed: false,
                });
                self.audio_clips.retain(|c| c.track_id.is_some());
            }
        } else if self.src_clock.is_some() {
            self.src_clock = None;
            self.audio_clips.retain(|c| c.track_id.is_some());
        }
        if let (Some(clock), Some(asset)) = (self.src_clock, src_asset) {
            let dur = asset.info.duration_sec.max(0.0);
            let block_lo = self.master_out as i64;
            let spawn_hi = block_lo + (AUDIO_PREROLL * rate_f) as i64;
            let (o_lo, o_hi) = clock_out_range(clock, 0.0, dur);
            let cut_lo = o_lo;
            let ob = o_hi;
            if ob > block_lo && cut_lo <= spawn_hi {
                let oa = cut_lo.max(block_lo);
                let enter_seq = clock.anchor_pos
                    + (oa - clock.anchor_out as i64) as f64 * clock.rate / rate_f;
                let media_enter = enter_seq.clamp(0.0, dur);
                let backward = src_rate < 0.0;
                let tempo = src_rate.abs();
                wants.push(Want {
                    clip_id: "source".into(),
                    track_id: None,
                    path: asset.decode_path(use_proxy).to_string(),
                    tempo,
                    media_backward: backward,
                    oa,
                    ob,
                    enter_seq,
                    seq_per_out: clock.rate / rate_f,
                    media_enter,
                    media_per_out: if backward { -tempo / rate_f } else { tempo / rate_f },
                    base_gain_db: 0.0,
                    vol: crate::core::animation::AnimatedParam::fixed(0.0),
                    fades: Vec::new(),
                    ramp_start: true,
                    ramp_end: true,
                    effects: Vec::new(),
                });
            }
        }

        // ---- Decoder abgleichen: passende behalten, neue starten ----------
        self.audio_clips.retain_mut(|c| {
            let Some(w) = wants.iter().find(|w| w.clip_id == c.clip_id) else {
                return false;
            };
            // Strukturwechsel (Pfad/Tempo/Richtung) ⇒ neu aufsetzen; sonst
            // läuft der Decoder weiter (oa/emitted bleiben stabil).
            if w.path != c.path
                || (w.tempo - c.tempo).abs() > 1e-6
                || w.media_backward != c.media_backward
            {
                return false;
            }
            c.ob = w.ob;
            c.base_gain_db = w.base_gain_db;
            c.vol = w.vol.clone();
            c.fades = w.fades.clone();
            c.sync_effects(&w.effects, c.media_enter);
            true
        });
        for w in &wants {
            if self.audio_clips.iter().any(|c| c.clip_id == w.clip_id) {
                continue;
            }
            if let Some(clip) = build_clip_audio(w) {
                self.audio_clips.push(clip);
            }
        }

        // ---- Audio-Scrubbing: kurzes Grain am gezogenen Playhead -----------
        let scrub_now = state.playback.audio_scrub_enabled
            && state.playback.scrub_active
            && !state.playback.program_playing
            && !state.playback.source.playing;
        if scrub_now {
            let pos = state.timeline.playhead_sec;
            // Neues Grain, wenn sich der Playhead seit dem letzten Grain um ≥
            // ein halbes Grain bewegt hat (kontinuierliches Scrubben); ein
            // ruhend gehaltener Playhead spielt das Grain genau einmal.
            let need = if self.scrub.is_none() {
                !self.scrub_last_pos.is_finite() || (pos - self.scrub_last_pos).abs() >= 1e-4
            } else {
                (pos - self.scrub_last_pos).abs() >= SCRUB_GRAIN_SEC * 0.5
            };
            if need {
                if let Some(v) = build_scrub_voice(state, pos, self.master_out) {
                    self.scrub = Some(v);
                    self.scrub_last_pos = pos;
                }
            }
        } else if self.scrub.is_some() {
            self.scrub = None;
            self.scrub_last_pos = f64::NAN;
        }

        let has_scrub = self.scrub.is_some();
        let master = self.master.as_mut().unwrap();
        let has_voices = !self.audio_clips.is_empty() || has_scrub;
        if !has_voices {
            master.flush_stop();
            self.prog_clock = None;
            self.src_clock = None;
            state.audio.track_levels.clear();
            state.audio.master_level = [0.0, 0.0];
            // Momentary/Short-Term auf Stille fallen lassen, Integrated +
            // True-Peak als Messergebnis halten.
            self.loudness.pause();
            state.audio.loudness = self.loudness.snapshot();
            return;
        }

        // ---- Mixdown: sample-genaue Platzierung je freiem Sub-Buffer -------
        let master_gain = db_to_linear(state.timeline.master_gain_db);
        let mut wrote_any = false;
        let mut prog_any_tick = false;
        let mut src_any_tick = false;
        let mut tick_tracks: std::collections::HashMap<String, [f32; 2]> =
            std::collections::HashMap::new();
        let mut tick_master = [0f32; 2];
        // Braucht eine Spur-Bus-Kette einen Sidechain-Key (Auto-Ducking)? Dann
        // müssen ALLE Spuren des Blocks zuerst roh vorliegen, um den Key (Summe
        // der ANDEREN Spuren) zu bilden — sonst läuft der billige Einzelpass.
        let any_ducking = self.track_fx.values().any(|c| c.needs_sidechain());
        while master.is_processed() {
            let block_out = self.master_out as i64;
            self.mix_buf.clear();
            self.mix_buf.resize(AUDIO_CHUNK_FRAMES * AUDIO_CHANNELS, 0.0);
            let mut any_frames = false;

            if any_ducking {
                // ---- Sidechain-Pfad: Roh-Puffer je Spur, dann Bus-FX(+Key) ----
                let nsamp = AUDIO_CHUNK_FRAMES * AUDIO_CHANNELS;
                // Pass 1: Clips je Spur summieren (vor Bus-FX/Gain) + Gesamtsumme.
                let mut raws: Vec<(bool, Vec<f32>)> = Vec::with_capacity(track_targets.len());
                let mut total = vec![0f32; nsamp];
                for (track_id, _target) in &track_targets {
                    let mut buf = vec![0f32; nsamp];
                    let mut track_any = false;
                    for clip in self.audio_clips.iter_mut() {
                        if clip.track_id.as_deref() != Some(track_id.as_str()) {
                            continue;
                        }
                        let (frames, _, _) = clip.mix_block(&mut buf, block_out);
                        if frames > 0 {
                            track_any = true;
                        }
                    }
                    if track_any {
                        for (t, s) in total.iter_mut().zip(&buf) {
                            *t += *s;
                        }
                    }
                    raws.push((track_any, buf));
                }
                // Pass 2: Bus-FX (Key = Gesamtsumme − eigene Spur) → Gain → Master.
                let mut key = vec![0f32; nsamp];
                for (ti, (track_id, target)) in track_targets.iter().enumerate() {
                    let (track_any, buf) = &mut raws[ti];
                    if !*track_any {
                        continue;
                    }
                    any_frames = true;
                    prog_any_tick = true;
                    if let Some(chain) = self.track_fx.get_mut(track_id) {
                        if chain.needs_sidechain() {
                            for ((k, t), s) in key.iter_mut().zip(&total).zip(buf.iter()) {
                                *k = *t - *s;
                            }
                            chain.process_with_sidechain(buf, Some(&key));
                        } else {
                            chain.process(buf);
                        }
                    }
                    let prev = self
                        .track_gain_smooth
                        .get(track_id)
                        .copied()
                        .unwrap_or(*target);
                    let (peak_l, peak_r) = apply_stereo_ramp(buf, prev, *target);
                    self.track_gain_smooth.insert(track_id.clone(), *target);
                    let entry = tick_tracks.entry(track_id.clone()).or_insert([0.0, 0.0]);
                    entry[0] = entry[0].max(peak_l);
                    entry[1] = entry[1].max(peak_r);
                    for (m, s) in self.mix_buf.iter_mut().zip(buf.iter()) {
                        *m += *s;
                    }
                }
            } else {
                // Programm: Clips je Spur sammeln → Bus-FX → Spur-Gain/Pan → Master.
                for (track_id, target) in &track_targets {
                    self.track_buf.clear();
                    self.track_buf.resize(AUDIO_CHUNK_FRAMES * AUDIO_CHANNELS, 0.0);
                    let mut track_any = false;
                    for clip in self.audio_clips.iter_mut() {
                        if clip.track_id.as_deref() != Some(track_id.as_str()) {
                            continue;
                        }
                        let (frames, _, _) = clip.mix_block(&mut self.track_buf, block_out);
                        if frames > 0 {
                            track_any = true;
                        }
                    }
                    if !track_any {
                        continue;
                    }
                    any_frames = true;
                    prog_any_tick = true;
                    if let Some(chain) = self.track_fx.get_mut(track_id) {
                        chain.process(&mut self.track_buf);
                    }
                    let prev = self
                        .track_gain_smooth
                        .get(track_id)
                        .copied()
                        .unwrap_or(*target);
                    let (peak_l, peak_r) = apply_stereo_ramp(&mut self.track_buf, prev, *target);
                    self.track_gain_smooth.insert(track_id.clone(), *target);
                    let entry = tick_tracks.entry(track_id.clone()).or_insert([0.0, 0.0]);
                    entry[0] = entry[0].max(peak_l);
                    entry[1] = entry[1].max(peak_r);
                    for i in 0..self.mix_buf.len() {
                        self.mix_buf[i] += self.track_buf[i];
                    }
                }
            }

            // Quelle (track_id None) + Scrubbing: ungeregelt in den Master.
            for clip in self.audio_clips.iter_mut() {
                if clip.track_id.is_some() {
                    continue;
                }
                let (frames, _, _) = clip.mix_block(&mut self.mix_buf, block_out);
                if frames > 0 {
                    any_frames = true;
                    src_any_tick = true;
                }
            }
            if let Some(scrub) = self.scrub.as_mut() {
                let (frames, _, _) = scrub.mix_block(&mut self.mix_buf, block_out);
                if frames > 0 {
                    any_frames = true;
                }
            }

            if !any_frames {
                // Kein Sample lieferbar (ffmpeg startet noch) — nicht mit Stille
                // füllen, damit der Stream exakt mit dem Ton beginnt.
                self.debug_starved += 1;
                break;
            }
            for s in self.mix_buf.iter_mut() {
                *s *= master_gain;
            }
            for pair in self.mix_buf.chunks_exact(AUDIO_CHANNELS) {
                tick_master[0] = tick_master[0].max(pair[0].abs());
                tick_master[1] = tick_master[1].max(pair[1].abs());
            }
            // BS.1770-Lautheit aus dem Master-Mixblock messen — vor dem
            // Hard-Clip (echte Pegel/Inter-Sample-Peaks) und nur während
            // laufender Programmwiedergabe (das, was der Export liefert; reine
            // Quell-/Scrub-Vorschau soll die Messung nicht verfälschen).
            if self.prog_clock.is_some() {
                self.loudness.feed(&self.mix_buf);
            }
            for s in self.mix_buf.iter_mut() {
                *s = s.clamp(-1.0, 1.0);
            }
            master.write(&self.mix_buf);
            self.master_out += AUDIO_CHUNK_FRAMES as u64;
            wrote_any = true;
            self.debug_blocks += 1;
        }

        // Verbrauchte Scrub-Stimme entfernen (Grain abgespielt).
        if self.scrub.as_ref().is_some_and(|s| {
            s.eof && s.buf.is_empty() && (s.oa + s.emitted) >= s.ob
        }) {
            self.scrub = None;
        }

        // ---- Drift-Korrektur: Playhead/Quelle an die Audio-Uhr slewen -----
        let mut slew_dbg = 0.0;
        if wrote_any {
            if prog_any_tick {
                if let Some(c) = self.prog_clock.as_mut() {
                    c.primed = true;
                }
                if let Some(c) = self.prog_clock {
                    let heard = c.heard_pos(self.master_out);
                    let step = slew_step(heard - state.timeline.playhead_sec);
                    state.timeline.playhead_sec = (state.timeline.playhead_sec + step).max(0.0);
                    slew_dbg = step;
                }
            }
            if src_any_tick {
                if let Some(c) = self.src_clock.as_mut() {
                    c.primed = true;
                }
                if let Some(c) = self.src_clock {
                    let heard = c.heard_pos(self.master_out);
                    let step = slew_step(heard - state.playback.source.position);
                    state.playback.source.position =
                        (state.playback.source.position + step).max(0.0);
                }
            }
        }

        // ---- Meter / GR aktualisieren (nur bei gemischten Blöcken) --------
        if wrote_any {
            state.audio.track_levels = tick_tracks;
            state.audio.master_level = tick_master;
            let mut fx_gr: std::collections::HashMap<String, f32> =
                std::collections::HashMap::new();
            for clip in &self.audio_clips {
                if let Some(chain) = &clip.fx_chain {
                    for (id, gr) in chain.dynamic_gain_reductions() {
                        fx_gr.insert(id, gr);
                    }
                }
            }
            for chain in self.track_fx.values() {
                for (id, gr) in chain.dynamic_gain_reductions() {
                    fx_gr.insert(id, gr);
                }
            }
            state.audio.fx_gain_reduction = fx_gr;
        }

        // ---- Lautheits-Snapshot für den Mixer ----------------------------
        // Bei reiner Quell-/Scrub-Wiedergabe (kein Programm) die gleitenden
        // Fenster verwerfen, damit Momentary/Short-Term nicht eingefroren
        // stehen bleiben; sonst die zuletzt gemessene Position merken.
        if self.prog_clock.is_some() {
            if wrote_any && prog_any_tick {
                self.loudness_last_pos = state.timeline.playhead_sec;
            }
        } else {
            self.loudness.pause();
        }
        state.audio.loudness = self.loudness.snapshot();

        self.debug_ticks += 1;
        if self.debug && now - self.debug_last >= 1.0 {
            self.debug_last = now;
            self.debug_slew = slew_dbg;
            eprintln!(
                "[audio] {} Blöcke/s | {} Ticks/s | {} verhungert | {} Stimmen | rate {:.2} | Slew {:+.1} ms | Master-Peak {:.3}/{:.3}",
                self.debug_blocks,
                self.debug_ticks,
                self.debug_starved,
                self.audio_clips.len(),
                prog_rate,
                self.debug_slew * 1000.0,
                state.audio.master_level[0],
                state.audio.master_level[1],
            );
            self.debug_blocks = 0;
            self.debug_ticks = 0;
            self.debug_starved = 0;
        }
    }
}

/// Frame als Textur hochladen + downgesampelte Scope-Kopie (≤ 192 px).
#[allow(clippy::too_many_arguments)]
fn upload_frame(
    rl: &mut RaylibHandle,
    thread: &RaylibThread,
    textures: &mut TextureCache,
    key: &str,
    w: i32,
    h: i32,
    data: &[u8],
    mini_out: Option<(&mut std::collections::HashMap<String, crate::stores::MiniFrame>, &str)>,
) {
    let existing = textures.get(key).map(|t| (t.width, t.height));
    if existing != Some((w, h)) {
        // Texture in passender Größe anlegen — gen_image_color liefert
        // bereits UNCOMPRESSED_R8G8B8A8, passend zu rawvideo/rgba.
        let image = Image::gen_image_color(w, h, raylib::color::Color::BLACK);
        if let Ok(tex) = rl.load_texture_from_image(thread, &image) {
            tex.set_texture_filter(
                thread,
                raylib::consts::TextureFilter::TEXTURE_FILTER_BILINEAR,
            );
            textures.put(key, tex);
        }
    }
    if let Some(tex) = textures.get_mut(key) {
        let _ = tex.update_texture(data);
    }
    if let Some((minis, clip_id)) = mini_out {
        let (sw, sh) = (w as usize, h as usize);
        let mw = sw.min(192).max(1);
        let mh = (sh * mw / sw.max(1)).max(1);
        let mut rgba = vec![0u8; mw * mh * 4];
        for y in 0..mh {
            let sy = y * sh / mh;
            for x in 0..mw {
                let sx = x * sw / mw;
                let src = (sy * sw + sx) * 4;
                let dst = (y * mw + x) * 4;
                rgba[dst..dst + 4].copy_from_slice(&data[src..src + 4]);
            }
        }
        minis.insert(
            clip_id.to_string(),
            crate::stores::MiniFrame { w: mw, h: mh, rgba },
        );
    }
}

/// Vorlauf, mit dem Decoder eingehender Übergangs-Clips gestartet werden,
/// damit der erste Frame beim Übergangsbeginn bereits anliegt (Sekunden).
const TRANSITION_PREROLL: f64 = 0.6;

/// Decoder-Ziele für alle sichtbaren Video-Layer am Playhead (Bilder zeigt
/// der Monitor direkt aus dem TextureCache — kein Decoder nötig). Während
/// eines Übergangs laufen ZWEI Decoder auf derselben Spur; bald beginnende
/// Übergänge starten den Decoder des eingehenden Clips vor (Pre-Roll).
fn program_video_targets(state: &AppState) -> Vec<(String, VideoTarget)> {
    let t = state.timeline.playhead_sec;
    // Programm-Decoder laufen gegen die SEQUENZRATE: ffmpeg wiederholt/
    // verwirft Quellframes (fps-Kette), abweichende Quellraten werden so
    // sauber auf die Sequenz abgebildet — wie im Export.
    let seq_fps = state.timeline.settings.rate.fps();
    // Proxy-Modus: Vorschau dekodiert aus der Proxy-Datei (Export bleibt
    // Original). Offline-Originale sind dank gültigem Proxy weiter abspielbar.
    let use_proxy = state.media.use_proxies;
    // Decoder-Ziel aus einem konkreten Asset bauen. `map_id` = Decoder-Map-ID
    // (= erstes Tupelelement); der Player lädt die Textur unter
    // `clip_texture_key(map_id)`. Skalierung pro Ziel frei.
    let target_for = |asset: &crate::core::types::MediaAsset,
                      map_id: String,
                      media_time: f64,
                      media_step: f64,
                      scale: f64|
     -> Option<(String, VideoTarget)> {
        if !asset.preview_playable(use_proxy)
            || asset.kind != crate::core::types::MediaKind::Video
        {
            return None;
        }
        let video = asset.info.video.first()?;
        // HDR-Tonemap nur beim Decode aus dem ORIGINAL (Proxys sind bereits SDR
        // bzw. ihr Farbraum ist hier nicht bekannt).
        let from_proxy = use_proxy && asset.has_valid_proxy();
        let hdr = !from_proxy && crate::core::export::OutputColor::from_stream(video).is_hdr();
        Some((
            map_id,
            VideoTarget {
                path: asset.decode_path(use_proxy).to_string(),
                media_time,
                src_w: video.width.max(2),
                src_h: video.height.max(2),
                fps: seq_fps,
                playing: state.playback.program_playing,
                rate: state.playback.program_rate,
                scale,
                media_step,
                hdr,
            },
        ))
    };
    // Aktiven Winkel eines Multicam-Clips zu (Asset, Medienzeit) auflösen.
    let resolve_mc = |clip: &crate::core::timeline::TimelineClip,
                      angle: u32,
                      t_at: f64|
     -> Option<(&crate::core::types::MediaAsset, f64)> {
        let mc = clip.multicam.as_ref()?;
        let src = state.timeline.timeline_of(&mc.source)?.multicam.as_ref()?;
        let a = src.angle(angle)?;
        let asset = state.media.asset(&a.asset_id)?;
        let media_time = (compose::clip_media_time(clip, t_at) - a.pos).max(0.0);
        Some((asset, media_time))
    };
    let make_target = |clip: &crate::core::timeline::TimelineClip, media_time: f64| {
        let asset = state.media.asset(&clip.asset_id)?;
        target_for(
            asset,
            clip.id.clone(),
            media_time,
            // Konstante Decoder-Rate (bei Time-Remap das mittlere Tempo): eine
            // durchlaufende Session statt eines Neustarts pro Frame (der bei
            // variabler Rate die Wiedergabe einfrieren ließe). Das GEPARKTE Bild
            // bleibt trotzdem frame-genau — `media_time` ist exakt das Integral
            // der Kurve, und der Cache-/Seek-Pfad zieht beim Scrubbing genau
            // diesen Frame.
            clip.media_step(),
            state.monitor.program_scale,
        )
    };

    // Im Multicam-Raster: für den aktiven Multicam-Clip KEIN Programm-Decoder
    // (clip.id), stattdessen ein Decoder je Winkel weiter unten.
    let grid_clip_id: Option<String> = if state.monitor.view == crate::stores::MonitorView::Multicam
    {
        active_multicam_clip(state).map(|c| c.id.clone())
    } else {
        None
    };

    let mut targets: Vec<(String, VideoTarget)> = compose::visible_program_layers(&state.timeline, t)
        .into_iter()
        .filter_map(|layer| match layer {
            compose::ProgramLayer::Clip { clip, .. } => {
                if let Some(mc) = &clip.multicam {
                    if grid_clip_id.as_deref() == Some(clip.id.as_str()) {
                        return None; // Raster liefert die Winkel-Kacheln
                    }
                    let (asset, media_time) = resolve_mc(clip, mc.angle, t)?;
                    target_for(
                        asset,
                        clip.id.clone(),
                        media_time,
                        clip.media_step(),
                        state.monitor.program_scale,
                    )
                } else {
                    make_target(clip, compose::clip_media_time(clip, t).max(0.0))
                }
            }
            compose::ProgramLayer::Solid { .. } | compose::ProgramLayer::Adjustment { .. } => None,
        })
        .collect();

    // Multicam-Raster: ein reduziert aufgelöster Decoder je Winkel des aktiven
    // Multicam-Clips (alle gegen denselben Playhead/Clock ⇒ synchron).
    if let Some(grid_id) = &grid_clip_id {
        if let Some(clip) = state.timeline.clip(grid_id) {
            if let Some(mc) = &clip.multicam {
                let n = state
                    .timeline
                    .timeline_of(&mc.source)
                    .and_then(|t| t.multicam.as_ref())
                    .map(|s| s.angle_count())
                    .unwrap_or(0);
                let cols = crate::core::multicam::grid_cols(n);
                // Kachel-Skalierung: Programm-Scale geteilt durch Spaltenzahl
                // (kleinere Offscreen-Render je Winkel), Untergrenze 1/8.
                let tile_scale =
                    (state.monitor.program_scale / cols.max(1) as f64).max(0.125);
                for i in 0..n as u32 {
                    if let Some((asset, media_time)) = resolve_mc(clip, i, t) {
                        if let Some(tgt) = target_for(
                            asset,
                            mc_angle_id(grid_id, i),
                            media_time,
                            clip.media_step(),
                            tile_scale,
                        ) {
                            targets.push(tgt);
                        }
                    }
                }
            }
        }
    }

    // Pre-Roll: eingehende Clips kurz bevorstehender Übergänge schon decodieren.
    for tr in &state.timeline.transitions {
        if tr.kind.is_audio() {
            continue;
        }
        let Some((w0, _)) = state.timeline.transition_window(tr) else {
            continue;
        };
        if w0 <= t || w0 > t + TRANSITION_PREROLL {
            continue;
        }
        let incoming = tr
            .to_clip_id
            .as_deref()
            .and_then(|id| state.timeline.clip(id))
            .filter(|c| c.enabled);
        if let Some(clip) = incoming {
            if !targets.iter().any(|(id, _)| id == &clip.id) {
                if let Some(target) = make_target(clip, compose::clip_media_time(clip, w0).max(0.0))
                {
                    targets.push(target);
                }
            }
        }
    }
    targets
}

/// Das gesamte Programmbild der AKTIVEN Sequenz CPU-komponieren (w×h), inkl.
/// der Adjustment-Layer-Korrektur-Pässe — über denselben Compositing-Kern wie
/// der Export (`composite_sequence_frame`), damit Vorschau und Export
/// formelgleich sind. Blatt-Frames kommen per Einzelbild-Extraktion (Proxy-
/// bewusst). Synchron/blockierend; nur aufgerufen, wenn ein Adjustment Layer
/// am Playhead liegt (signaturgecacht im Player).
fn compose_program_preview(state: &AppState, t: f64, w: usize, h: usize) -> Option<Vec<u8>> {
    let use_proxy = state.media.use_proxies;
    let media = &state.media;
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(8);
    // LUT-Cache über die ganze Komposition (lädt jede .cube nur einmal).
    let mut lut_cache = crate::core::lut::LutCache::default();
    let mut fetch = |clip: &crate::core::timeline::TimelineClip,
                     media_t: f64,
                     lw: usize,
                     lh: usize|
     -> Option<Vec<f32>> {
        if clip.is_generator() {
            return None;
        }
        let asset = media.asset(&clip.asset_id)?;
        if !asset.preview_playable(use_proxy)
            || asset.kind == crate::core::types::MediaKind::Audio
        {
            return None;
        }
        let image = asset.kind == crate::core::types::MediaKind::Image;
        let raw = extract_leaf_frame_sync(asset.decode_path(use_proxy), media_t, image, lw, lh)?;
        let mut frame = crate::core::pixbuf::rgba8_to_f32(&raw);
        // Sichtbarer Inhalt im transparent gepolsterten Puffer (contain-fit der
        // Quelle, zentriert) — Bezugsrahmen für Effekte/Vignette, exakt wie der
        // Export-Pfad (`render_segment_composited`).
        let content = match asset.info.video.first() {
            Some(v) if v.width > 0 && v.height > 0 => {
                let (nw, nh) = (v.width as f64, v.height as f64);
                let fit = (lw as f64 / nw).min(lh as f64 / nh);
                let cw = ((nw * fit).round() as usize).clamp(1, lw);
                let ch = ((nh * fit).round() as usize).clamp(1, lh);
                ((lw - cw) / 2, (lh - ch) / 2, cw, ch)
            }
            _ => (0, 0, lw, lh),
        };
        // DIE Blatt-Verarbeitung der darunterliegenden Clips (Effekte → Grade),
        // damit ihre Einzel-Korrekturen unter dem Adjustment-Pass erhalten
        // bleiben — formelgleich zum Export und zur GPU-Vorschau.
        let resolved = crate::core::effects::resolve_video_effects(&clip.effects, media_t);
        if !resolved.is_empty() {
            crate::core::effects::apply_effects_buffer(&mut frame, lw, lh, content, &resolved, threads);
        }
        let gp = crate::core::grade::precompute(&clip.grade);
        let luts = crate::core::grade::resolve_luts(&clip.grade, &mut lut_cache);
        if !gp.is_identity() || luts.is_active() {
            crate::core::grade::grade_buffer(&mut frame, lw, lh, content, &gp, &luts.borrow(), threads);
        }
        Some(frame)
    };
    // Aktive Timeline (Deref) als Compositing-Wurzel; Resolver = SequenceStore.
    let active: &crate::core::timeline::TimelineStore = &state.timeline;
    let f = compose::composite_sequence_frame(active, &state.timeline, t, w, h, threads, &mut fetch, 0);
    Some(crate::core::pixbuf::f32_to_rgba8_dithered(&f, w, h))
}

/// Ein Vorschau-Frame einer verschachtelten Sequenz rekursiv komponieren
/// (innere Auflösung w×h). Nutzt den geteilten Compositing-Kern; die Blatt-
/// Frames kommen per Einzelbild-Extraktion (Proxy-bewusst wie die übrige
/// Vorschau). Der Resolver ist die SequenceStore (über `state.timeline`).
fn compose_nest_preview(
    state: &AppState,
    inner_id: &str,
    inner_t: f64,
    w: usize,
    h: usize,
) -> Option<Vec<u8>> {
    let inner = state.timeline.timeline_of(inner_id)?;
    let use_proxy = state.media.use_proxies;
    let media = &state.media;
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(8);
    let mut fetch = |clip: &crate::core::timeline::TimelineClip,
                     media_t: f64,
                     lw: usize,
                     lh: usize|
     -> Option<Vec<f32>> {
        if clip.is_generator() {
            return None;
        }
        let asset = media.asset(&clip.asset_id)?;
        if !asset.preview_playable(use_proxy)
            || asset.kind == crate::core::types::MediaKind::Audio
        {
            return None;
        }
        let image = asset.kind == crate::core::types::MediaKind::Image;
        extract_leaf_frame_sync(asset.decode_path(use_proxy), media_t, image, lw, lh)
            .map(|b| crate::core::pixbuf::rgba8_to_f32(&b))
    };
    // Rekursive f32-Komposition → 8-Bit-RGBA (dithered) für den Textur-Upload
    // der Nest-Vorschau (Vorschau-Texturen sind 8 Bit; der Export bleibt f32).
    let f = compose::composite_sequence_frame(
        inner,
        &state.timeline,
        inner_t,
        w,
        h,
        threads,
        &mut fetch,
        1,
    );
    Some(crate::core::pixbuf::f32_to_rgba8_dithered(&f, w, h))
}

/// Ein Blatt-Frame synchron per ffmpeg extrahieren (contain-fit + transparent
/// gepolstert, w×h RGBA). Blockierend — nur für die Nest-Vorschau (gecacht).
fn extract_leaf_frame_sync(
    path: &str,
    media_t: f64,
    image: bool,
    w: usize,
    h: usize,
) -> Option<Vec<u8>> {
    let filter = format!(
        "scale={w}:{h}:force_original_aspect_ratio=decrease:flags=bilinear,format=rgba,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:color=black@0.0"
    );
    let mut cmd = std::process::Command::new(crate::services::ffmpeg_bin());
    cmd.args(["-v", "error"]);
    if image {
        cmd.args(["-loop", "1", "-framerate", "1"]);
    } else {
        cmd.args(["-ss", &format!("{:.4}", media_t.max(0.0))]);
    }
    cmd.args(["-i", path])
        .args(["-an", "-sn"])
        .args(["-vf", &filter])
        .args(["-frames:v", "1"])
        .args(["-f", "rawvideo", "-pix_fmt", "rgba"])
        .arg("pipe:1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let out = cmd.output().ok()?;
    if out.stdout.len() == w * h * 4 {
        Some(out.stdout)
    } else {
        None
    }
}

/// Programm-Ziel aus dem Sequenz-Render-Cache, falls ein gültiges Segment den
/// Playhead abdeckt UND gerade abgespielt wird. Liefert einen `VideoTarget`,
/// der die Cache-Datei (1:1 zur Sequenz) als Vollbild dekodiert. `refresh`
/// muss vorher gelaufen sein.
fn render_cache_target(state: &AppState) -> Option<VideoTarget> {
    if !state.playback.program_playing {
        return None;
    }
    let rate = state.timeline.settings.rate;
    let fps = rate.fps();
    let frame = rate.frame_round(state.timeline.playhead_sec);
    let (file, local) = state.render_cache.valid_file_at(frame)?;
    let seg_start_sec = (frame - local) as f64 / fps.max(1.0);
    let local_time = (state.timeline.playhead_sec - seg_start_sec).max(0.0);
    let prog_rate = state.playback.program_rate;
    Some(VideoTarget {
        path: file.to_string_lossy().into_owned(),
        media_time: local_time,
        src_w: state.timeline.settings.width.max(2),
        src_h: state.timeline.settings.height.max(2),
        fps,
        playing: true,
        rate: prog_rate,
        scale: state.monitor.program_scale,
        // Der Cache läuft 1:1 zur Sequenz; Rückwärtswiedergabe = Reverse-Pfad.
        media_step: if prog_rate < 0.0 { -1.0 } else { 1.0 },
        // Der Render-Cache ist bereits SDR (BT.709) — kein Tonemap.
        hdr: false,
    })
}

fn source_video_target(state: &AppState) -> Option<VideoTarget> {
    let asset = state
        .playback
        .source_asset_id
        .as_ref()
        .and_then(|id| state.media.asset(id))?;
    if asset.kind != crate::core::types::MediaKind::Video {
        return None;
    }
    let use_proxy = state.media.use_proxies;
    if !asset.preview_playable(use_proxy) {
        return None;
    }
    let video = asset.info.video.first()?;
    let from_proxy = use_proxy && asset.has_valid_proxy();
    let hdr = !from_proxy && crate::core::export::OutputColor::from_stream(video).is_hdr();
    Some(VideoTarget {
        path: asset.decode_path(use_proxy).to_string(),
        media_time: state.playback.source.position,
        src_w: video.width.max(2),
        src_h: video.height.max(2),
        fps: video.fps,
        playing: state.playback.source.playing,
        rate: state.playback.source.rate,
        scale: state.monitor.source_scale,
        media_step: 1.0,
        hdr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test-Clip aus konstanten 1.0-Stereo-Samples (In-Memory-Quelle).
    fn mem_clip(frames: usize, oa: i64, ob: i64) -> ClipAudio {
        ClipAudio {
            clip_id: "t".into(),
            track_id: None,
            path: String::new(),
            tempo: 1.0,
            media_backward: false,
            oa,
            ob,
            emitted: 0,
            enter_seq: 0.0,
            seq_per_out: 1.0 / AUDIO_RATE as f64,
            media_enter: 0.0,
            media_per_out: 1.0 / AUDIO_RATE as f64,
            base_gain_db: 0.0,
            vol: crate::core::animation::AnimatedParam::fixed(0.0),
            fades: Vec::new(),
            ramp_start: true,
            ramp_end: true,
            src: AudioSrc::Mem {
                data: vec![1.0f32; frames * AUDIO_CHANNELS],
                pos: 0,
            },
            buf: Vec::new(),
            eof: false,
            fx_chain: None,
            fx_effects: Vec::new(),
            fx_pos: 0,
        }
    }

    /// Ein Clip an einer KRUMMEN Sub-Block-Position muss exakt am richtigen
    /// Sample beginnen/enden — nicht block-granular (der behobene ≤85-ms-Bug).
    #[test]
    fn placement_is_sample_accurate() {
        let oa = 100i64;
        let len = 1000i64;
        let mut clip = mem_clip(len as usize, oa, oa + len);
        let mut out = vec![0f32; AUDIO_CHUNK_FRAMES * AUDIO_CHANNELS];
        let (frames, _, _) = clip.mix_block(&mut out, 0);
        assert_eq!(frames, len as usize, "alle Clip-Frames platziert");
        let at = |f: i64| out[(f as usize) * AUDIO_CHANNELS];
        assert_eq!(at(0), 0.0, "kein Ton am Blockanfang (sample-genau, nicht block)");
        assert_eq!(at(99), 0.0, "vor oa still");
        assert!(at(100) > 0.0, "an oa exakt hörbar");
        assert!(
            (at(oa + CLICK_RAMP_FRAMES as i64) - 1.0).abs() < 1e-6,
            "volle Verstärkung nach der Anti-Klick-Rampe"
        );
        assert!((at(oa + 500) - 1.0).abs() < 1e-6);
        assert!(at(oa + len - 1) > 0.0, "letztes Frame hörbar");
        assert_eq!(at(oa + len), 0.0, "nach ob exakt still");
    }

    /// Die Anti-Klick-Rampe steigt klickfrei von ~0 auf volle Verstärkung.
    #[test]
    fn anti_click_ramp_rises_to_unity() {
        let mut clip = mem_clip(1000, 0, 1000);
        let mut out = vec![0f32; AUDIO_CHUNK_FRAMES * AUDIO_CHANNELS];
        clip.mix_block(&mut out, 0);
        let v = |f: usize| out[f * AUDIO_CHANNELS];
        assert!(v(0) < 0.02, "Start nahe null (klickfrei)");
        assert!(v(0) < v(120) && v(120) < v(CLICK_RAMP_FRAMES - 1), "steigend");
        assert!((v(CLICK_RAMP_FRAMES) - 1.0).abs() < 1e-6, "danach volle Gain");
    }

    /// Lückenlose Schnittkante (ramp_start=false): KEINE Rampe → erstes Sample
    /// schon mit voller Verstärkung (keine Lautstärke-Delle am Razor-Schnitt).
    #[test]
    fn seamless_cut_has_no_ramp() {
        let mut clip = mem_clip(1000, 0, 1000);
        clip.ramp_start = false;
        let mut out = vec![0f32; AUDIO_CHUNK_FRAMES * AUDIO_CHANNELS];
        clip.mix_block(&mut out, 0);
        assert!((out[0] - 1.0).abs() < 1e-6, "kein Fade-In an lückenloser Kante");
    }

    /// Krumme Sekundenposition → exakter Sample-Offset (keine Block-Rundung).
    #[test]
    fn clock_maps_fractional_position_to_exact_sample() {
        let clock = TargetClock {
            anchor_out: 0,
            anchor_pos: 0.0,
            rate: 1.0,
            primed: true,
        };
        let (lo, hi) = clock_out_range(clock, 0.5001, 2.0);
        assert_eq!(lo, (0.5001 * AUDIO_RATE as f64).round() as i64);
        assert_eq!(lo, 24005);
        assert_eq!(hi, 96000);
        assert_ne!(lo % AUDIO_CHUNK_FRAMES as i64, 0, "nicht block-aligned");
    }

    /// Rückwärts: die Output-Achse läuft mit fallender Sequenzzeit — der
    /// Clip wird an seinem ENDE betreten (niedrigster Output-Frame).
    #[test]
    fn reverse_clock_enters_at_clip_end() {
        let clock = TargetClock {
            anchor_out: 1000,
            anchor_pos: 5.0,
            rate: -1.0,
            primed: true,
        };
        let (lo, hi) = clock_out_range(clock, 4.0, 5.0);
        assert_eq!(lo, 1000);
        assert_eq!(hi, 1000 + AUDIO_RATE as i64);
        assert_eq!(clock.out_of(5.0).round() as i64, 1000, "Enter an a1 (Clip-Ende)");
    }

    #[test]
    fn reverse_stereo_swaps_frames() {
        let mut b = vec![1.0, -1.0, 2.0, -2.0, 3.0, -3.0]; // 3 Frames L/R
        reverse_stereo(&mut b);
        assert_eq!(b, vec![3.0, -3.0, 2.0, -2.0, 1.0, -1.0]);
    }

    /// Drift-Korrektur: über simulierte 30 min mit 0,05 % Quarz-Fehler bleibt
    /// der Playhead an die GERÄTE-Uhr verriegelt (lippensynchron), nicht an
    /// den Wall-Clock.
    #[test]
    fn slew_locks_playhead_to_device_clock_over_30min() {
        let err = 0.0005f64; // Gerät läuft 0,05 % schneller als der Wall-Clock
        let block_real = AUDIO_CHUNK_FRAMES as f64 / (AUDIO_RATE as f64 * (1.0 + err));
        let clock = TargetClock {
            anchor_out: 0,
            anchor_pos: 0.0,
            rate: 1.0,
            primed: true,
        };
        let mut playhead = 0.0f64;
        let ticks = (1800.0 / block_real) as u64;
        let mut master_out = 0u64;
        for _ in 0..ticks {
            master_out += AUDIO_CHUNK_FRAMES as u64; // ein gerätegetakteter Block
            playhead += block_real; // Wall-Clock-Vorschub (rate 1)
            let heard = clock.heard_pos(master_out);
            playhead += slew_step(heard - playhead);
        }
        let heard = clock.heard_pos(master_out);
        assert!((playhead - heard).abs() < 0.005, "lock: {playhead} vs {heard}");
        let wall = ticks as f64 * block_real;
        assert!(
            playhead - wall > 0.5,
            "folgt der Geräte-Uhr (+{:.3}s), nicht dem Wall-Clock",
            playhead - wall
        );
    }
}
