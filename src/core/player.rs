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

use crate::core::audio_fx::AudioFxChain;
use crate::core::compose;
use crate::core::effects::EffectInstance;
use crate::core::timeline::TrackKind;
use crate::state::AppState;
use crate::ui::textures::TextureCache;
use raylib::audio::RaylibAudio;
use raylib::core::texture::{Image, RaylibTexture2D};
use raylib::{RaylibHandle, RaylibThread};
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{sync_channel, Receiver, TryRecvError};

pub const SOURCE_KEY: &str = "player://source";

/// Texture-Schlüssel eines Programm-Layers (Decoder je Clip).
pub fn clip_texture_key(clip_id: &str) -> String {
    format!("player://clip/{clip_id}")
}

const MAX_DECODE_WIDTH: f64 = 1920.0;
/// Toleranz, ab der ein laufender Decoder neu positioniert wird (Sekunden).
const RESYNC_TOLERANCE: f64 = 0.25;
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
/// Drift zwischen Playhead und geliefertem Audio, ab der ein Decoder neu
/// positioniert wird (Sekunden) — fängt Seeks und Loop-Sprünge während der
/// Wiedergabe ab; der konstante Puffer-Vorlauf (~2 Blöcke) bleibt darunter.
const AUDIO_RESYNC_TOLERANCE: f64 = 0.35;

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
        cmd.args(["-v", "error", "-ss", &format!("{media_time:.4}")])
            .args(["-i", path])
            .args(["-an", "-sn"])
            .args(["-f", "rawvideo", "-pix_fmt", "rgba"]);
        if speed == 0.0 {
            cmd.args(["-vf", &format!("scale={w}:{h}")])
                .args(["-frames:v", "1"]);
        } else {
            cmd.args(["-vf", &format!("{setpts}scale={w}:{h}")])
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
        let mut child = Command::new(crate::services::ffmpeg_bin())
            .args(["-v", "error", "-ss", &format!("{chunk_start:.4}")])
            .args(["-i", path])
            .args(["-an", "-sn"])
            .args(["-f", "rawvideo", "-pix_fmt", "rgba"])
            .args(["-vf", &format!("{setpts}scale={w}:{h}")])
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

/// Decoder-Session eines Programm-Layers: vorwärts (Pipe folgt dem
/// Playhead) oder rückwärts (Chunk + Frame-Puffer).
enum Session {
    Forward(VideoSession),
    Reverse(ReverseSession),
}

// ---------------------------------------------------------------- Audio

/// dB → linearer Faktor; ≤ −60 dB gilt als −∞ (stumm).
fn db_to_linear(db: f64) -> f32 {
    if db <= -60.0 {
        0.0
    } else {
        10f32.powf(db as f32 / 20.0)
    }
}

/// Stereo-Balance: dämpft die abgewandte Seite, Mitte bleibt bei 1.0
/// (kein −3-dB-Center-Drop wie beim Constant-Power-Pan).
fn pan_gains(pan: f64) -> (f32, f32) {
    let p = pan.clamp(-1.0, 1.0) as f32;
    (1.0 - p.max(0.0), 1.0 + p.min(0.0))
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

/// Ein ffmpeg-Decoder pro hörbarem Clip; liefert f32le-Stereo-Chunks,
/// die der Mixdown additiv in den Master-Block mischt.
struct AudioDecoder {
    clip_id: String,
    /// Spur des Clips (None = Quellmonitor) — Ziel der Pegelmessung.
    track_id: Option<String>,
    path: String,
    /// Clip-Geschwindigkeit (pitch-korrigierte atempo-Kette im Decoder).
    speed: f64,
    /// Medienzeit, ab der dekodiert wurde.
    start_media_time: f64,
    /// An den Mix gelieferte Frames seit Start (Drift-Erkennung).
    consumed_frames: u64,
    rx: Receiver<Vec<f32>>,
    child: Child,
    buf: Vec<f32>,
    eof: bool,
    /// Wirksamer Faktor je Seite (Spur-Gain × Clip-Gain × Balance),
    /// wird pro Tick aus dem App-Zustand aktualisiert.
    gain_l: f32,
    gain_r: f32,
    /// Audio-Effekt-Kette des Clips (DSP vor Gain) — identischer Code wie
    /// im Export-Mix. Wird pro Tick mit dem Clip-Zustand synchronisiert.
    fx_chain: Option<AudioFxChain>,
}

impl AudioDecoder {
    fn start(
        clip_id: &str,
        track_id: Option<String>,
        path: &str,
        media_time: f64,
        speed: f64,
    ) -> Option<AudioDecoder> {
        let media_time = media_time.max(0.0);
        let mut cmd = Command::new(crate::services::ffmpeg_bin());
        cmd.args(["-v", "error", "-ss", &format!("{media_time:.4}")])
            .args(["-i", path])
            .args(["-vn", "-sn"]);
        // Pitch-korrigiertes Tempo — identische atempo-Kette wie der Export.
        if let Some(chain) = crate::core::export::atempo_chain(speed) {
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
                                let samples = bytes_to_f32(&buf[..filled]);
                                let _ = tx.send(samples);
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
        Some(AudioDecoder {
            clip_id: clip_id.to_string(),
            track_id,
            path: path.to_string(),
            speed,
            start_media_time: media_time,
            consumed_frames: 0,
            rx,
            child,
            buf: Vec::new(),
            eof: false,
            gain_l: 1.0,
            gain_r: 1.0,
            fx_chain: None,
        })
    }

    /// Effekt-Kette mit dem Clip-Zustand abgleichen: gleiche Struktur ⇒
    /// Parameter nachstimmen (Filterzustände bleiben, Regler wirken live);
    /// geänderte Struktur ⇒ neu aufbauen.
    fn sync_effects(&mut self, effects: &[EffectInstance], media_t: f64) {
        let refs: Vec<&EffectInstance> = effects.iter().collect();
        let active = refs.iter().any(|e| e.enabled && e.kind.is_audio());
        if !active {
            self.fx_chain = None;
            return;
        }
        match self.fx_chain.as_mut() {
            Some(chain) if chain.matches(&refs) => chain.retune(&refs, media_t),
            _ => {
                self.fx_chain =
                    AudioFxChain::build(&refs, AUDIO_RATE, AUDIO_CHANNELS, media_t)
            }
        }
    }

    /// Erwartete Medienzeit der nächsten gelieferten Samples (die
    /// Ausgabesamples laufen in Sequenzzeit — Medienzeit skaliert mit speed).
    fn expected_media_time(&self) -> f64 {
        self.start_media_time + self.consumed_frames as f64 / AUDIO_RATE as f64 * self.speed
    }

    /// Mischt bis zu `out.len()/2` Frames additiv nach `out` (interleaved)
    /// und liefert (gelieferte Frames, Peak L, Peak R) des Beitrags.
    fn mix_into(&mut self, out: &mut [f32]) -> (usize, f32, f32) {
        while self.buf.len() < out.len() && !self.eof {
            match self.rx.try_recv() {
                Ok(mut chunk) => {
                    // Effekte beim Einreihen anwenden — jedes Sample läuft
                    // genau einmal durch die Kette (Zustände bleiben stetig).
                    if let Some(chain) = self.fx_chain.as_mut() {
                        chain.process(&mut chunk);
                    }
                    self.buf.extend(chunk);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => self.eof = true,
            }
        }
        let n = out.len().min(self.buf.len());
        let n = n - (n % AUDIO_CHANNELS);
        let (mut peak_l, mut peak_r) = (0f32, 0f32);
        for i in (0..n).step_by(AUDIO_CHANNELS) {
            let l = self.buf[i] * self.gain_l;
            let r = self.buf[i + 1] * self.gain_r;
            out[i] += l;
            out[i + 1] += r;
            peak_l = peak_l.max(l.abs());
            peak_r = peak_r.max(r.abs());
        }
        self.buf.drain(..n);
        self.consumed_frames += (n / AUDIO_CHANNELS) as u64;
        (n / AUDIO_CHANNELS, peak_l, peak_r)
    }
}

impl Drop for AudioDecoder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn bytes_to_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

// ---------------------------------------------------------------- Engine

/// Zielzustand eines Monitors in diesem Tick.
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
}

pub struct PlayerEngine {
    master: Option<MasterStream>,
    /// Ein Decoder je sichtbarem Programm-Layer (Schlüssel = Clip-ID).
    program_videos: std::collections::HashMap<String, Option<Session>>,
    source_video: Option<Session>,
    audio_decoders: Vec<AudioDecoder>,
    /// Wiederverwendeter Mix-Block (interleaved L/R).
    mix_buf: Vec<f32>,
    /// EDITRON_AUDIO_DEBUG=1: einmal pro Sekunde Mix-Statistik auf stderr.
    debug: bool,
    debug_last: f64,
    debug_blocks: u64,
    /// Ticks, in denen ein freier Sub-Buffer mangels Decoder-Daten leer blieb.
    debug_starved: u64,
    debug_ticks: u64,
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
            audio_decoders: Vec::new(),
            mix_buf: Vec::new(),
            debug: std::env::var("EDITRON_AUDIO_DEBUG").is_ok(),
            debug_last: 0.0,
            debug_blocks: 0,
            debug_starved: 0,
            debug_ticks: 0,
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
        // ---- Programmmonitor: ein Decoder je sichtbarem Video-Layer ----
        let program_targets = program_video_targets(state);
        // Nicht mehr sichtbare Layer beenden und ihre Texturen freigeben.
        let wanted: std::collections::HashSet<&str> =
            program_targets.iter().map(|(id, _)| id.as_str()).collect();
        let minis = &mut state.monitor.preview_frames;
        self.program_videos.retain(|clip_id, _| {
            let keep = wanted.contains(clip_id.as_str());
            if !keep {
                textures.remove(&clip_texture_key(clip_id));
                minis.remove(clip_id);
            }
            keep
        });
        for (clip_id, target) in program_targets {
            let session = self.program_videos.entry(clip_id.clone()).or_default();
            Self::drive_video(
                rl,
                thread,
                session,
                textures,
                &clip_texture_key(&clip_id),
                Some(target),
                now,
                Some((minis, &clip_id)),
            );
        }

        // ---- Quellmonitor ----
        let source_target = source_video_target(state);
        Self::drive_video(
            rl,
            thread,
            &mut self.source_video,
            textures,
            SOURCE_KEY,
            source_target,
            now,
            None,
        );

        // ---- Audio (Programm + Quelle) ----
        self.drive_audio(state, now);
    }

    #[allow(clippy::too_many_arguments)]
    fn drive_video(
        rl: &mut RaylibHandle,
        thread: &RaylibThread,
        session: &mut Option<Session>,
        textures: &mut TextureCache,
        key: &str,
        target: Option<VideoTarget>,
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
            Self::drive_video_reverse(rl, thread, session, textures, key, &target, now, mini_out);
            return;
        }
        let fwd_speed = target.media_step; // ≥ 0; 0 = Standbild

        // Session neu starten, wenn Quelle/Größe/Tempo wechselt oder die
        // Zeit zu weit wegläuft (Seek/Scrub/Loop/negative Rate). Während der
        // Wiedergabe darf der Decoder etwas hinterherhinken (Frames werden
        // gedroppt), sonst würde jeder Restart das Bild stocken lassen.
        let needs_restart = match session.as_ref() {
            Some(Session::Forward(s)) => {
                let (expected_w, _) = decode_dims(target.src_w, target.src_h, target.scale);
                let decoded_time = s
                    .last_frame_index
                    .map(|i| s.media_time_of(i))
                    .unwrap_or(s.start_media_time);
                let lead = decoded_time - target.media_time; // > 0: Decoder voraus (Rücksprung)
                let lag = target.media_time - decoded_time; // > 0: Decoder hinterher
                // Toleranzen in MEDIEN-Sekunden skalieren mit dem Tempo;
                // Standbilder erneuern frame-genau (Slip am eingefrorenen Frame).
                let tempo = fwd_speed.max(1.0);
                let (lead_tol, lag_limit) = if fwd_speed == 0.0 {
                    let t = 0.5 / s.fps.max(1.0);
                    (t, t)
                } else if target.playing && target.rate > 0.0 {
                    (0.15 * tempo, 1.5 * tempo)
                } else {
                    (0.15 * tempo, RESYNC_TOLERANCE * tempo)
                };
                s.path != target.path
                    || s.w != expected_w
                    || (s.speed - fwd_speed).abs() > 1e-6
                    || ((lead > lead_tol || lag > lag_limit)
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
                now,
            )
            .map(Session::Forward);
        }
        let Some(Session::Forward(s)) = session.as_mut() else { return };

        // Frames bis zur Zielzeit konsumieren; den letzten hochladen.
        let mut latest: Option<VideoFrame> = None;
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
                    latest = Some(frame);
                }
                Err(_) => break,
            }
        }
        if let Some(frame) = latest {
            upload_frame(rl, thread, textures, key, s.w, s.h, &frame.data, mini_out);
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
        }
    }

    fn drive_audio(&mut self, state: &mut AppState, now: f64) {
        // Gewünschte Audio-Quellen bestimmen (nur bei Vorwärts-Wiedergabe 1×).
        struct Want {
            clip_id: String,
            track_id: Option<String>,
            path: String,
            media_time: f64,
            speed: f64,
            gain_l: f32,
            gain_r: f32,
            effects: Vec<EffectInstance>,
        }
        let mut wants: Vec<Want> = Vec::new();

        // Programm: alle hörbaren Audio-Clips am Playhead.
        if state.playback.program_playing && (state.playback.program_rate - 1.0).abs() < 0.01 {
            let t = state.timeline.playhead_sec;
            let solo_any = state.timeline.tracks.iter().any(|tr| tr.solo);
            for track in state
                .timeline
                .tracks
                .iter()
                .filter(|tr| tr.kind == TrackKind::Audio && !tr.muted && (!solo_any || tr.solo))
            {
                let track_gain = db_to_linear(track.gain_db);
                let (pan_l, pan_r) = pan_gains(track.pan);
                for clip in state
                    .timeline
                    .clips
                    .iter()
                    .filter(|c| c.track_id == track.id && c.enabled)
                {
                    // Rückwärts-Clips sind (vorerst) stumm, Standbilder
                    // ohnehin — identisch zum Export-Mix.
                    if clip.reverse || clip.freeze {
                        continue;
                    }
                    // Übergangs-Crossfades verlängern den hörbaren Bereich
                    // über die Clipkanten hinaus (Handles im Modell geklemmt).
                    let fades = state.timeline.audio_fades(clip);
                    let (a0, a1) = state.timeline.audio_extent(clip, &fades);
                    if t < a0 || t >= a1 {
                        continue;
                    }
                    if let Some(asset) = state.media.asset(&clip.asset_id) {
                        if asset.offline {
                            continue;
                        }
                        let media_time = compose::clip_media_time(clip, t);
                        // Crossfade-Hüllkurve (pro Tick ausgewertet, wie die
                        // Lautstärke-Keyframes).
                        let mut fade_gain = 1.0f64;
                        for (w0, w1, fade_in, equal_power) in &fades {
                            if t >= *w0 && t < *w1 {
                                let p = (t - w0) / (w1 - w0);
                                fade_gain *=
                                    crate::core::transitions::audio_gain(*equal_power, *fade_in, p);
                            }
                        }
                        // Clip-Gain + animierte Lautstärke (Keyframes) — wird
                        // pro Tick neu ausgewertet, Fades laufen also live.
                        let gain = track_gain
                            * db_to_linear(clip.gain_db + clip.fx.volume_db.eval(media_time))
                            * fade_gain as f32;
                        wants.push(Want {
                            clip_id: clip.id.clone(),
                            track_id: Some(track.id.clone()),
                            path: asset.path.clone(),
                            media_time,
                            speed: clip.eff_speed(),
                            gain_l: gain * pan_l,
                            gain_r: gain * pan_r,
                            effects: clip
                                .effects
                                .iter()
                                .filter(|e| e.kind.is_audio())
                                .cloned()
                                .collect(),
                        });
                    }
                }
            }
        }
        // Quelle: ungeregelt (Spur-/Master-Fader gelten nur fürs Programm).
        if state.playback.source.playing && (state.playback.source.rate - 1.0).abs() < 0.01 {
            if let Some(asset) = state
                .playback
                .source_asset_id
                .as_ref()
                .and_then(|id| state.media.asset(id))
            {
                if !asset.info.audio.is_empty() && !asset.offline {
                    wants.push(Want {
                        clip_id: "source".into(),
                        track_id: None,
                        path: asset.path.clone(),
                        media_time: state.playback.source.position,
                        speed: 1.0,
                        gain_l: 1.0,
                        gain_r: 1.0,
                        effects: Vec::new(),
                    });
                }
            }
        }

        // Nicht mehr gewünschte Decoder beenden; abgedriftete (Seek/Loop
        // während der Wiedergabe) neu starten; Gains live übernehmen.
        self.audio_decoders.retain_mut(|dec| {
            let Some(want) = wants.iter().find(|w| w.clip_id == dec.clip_id) else {
                return false;
            };
            if want.path != dec.path {
                return false;
            }
            if (dec.speed - want.speed).abs() > 1e-6 {
                return false;
            }
            // Drift in MEDIEN-Sekunden — der Puffer-Vorlauf skaliert mit
            // dem Tempo, die Toleranz also auch.
            if (dec.expected_media_time() - want.media_time).abs()
                > AUDIO_RESYNC_TOLERANCE * dec.speed.max(1.0)
            {
                return false;
            }
            dec.gain_l = want.gain_l;
            dec.gain_r = want.gain_r;
            dec.sync_effects(&want.effects, want.media_time);
            true
        });
        for want in &wants {
            if self
                .audio_decoders
                .iter()
                .any(|d| d.clip_id == want.clip_id)
            {
                continue;
            }
            if let Some(mut dec) = AudioDecoder::start(
                &want.clip_id,
                want.track_id.clone(),
                &want.path,
                want.media_time,
                want.speed,
            ) {
                dec.gain_l = want.gain_l;
                dec.gain_r = want.gain_r;
                dec.sync_effects(&want.effects, want.media_time);
                self.audio_decoders.push(dec);
            }
        }

        let Some(master) = self.master.as_mut() else { return };

        if self.audio_decoders.is_empty() {
            master.flush_stop();
            state.audio.track_levels.clear();
            state.audio.master_level = [0.0, 0.0];
            return;
        }

        // Mixdown: solange raylib einen Sub-Buffer frei hat und mindestens
        // ein Decoder Daten liefert. Blöcke ohne jegliche Daten (ffmpeg
        // startet noch) werden nicht geschrieben — so beginnt der Stream
        // exakt mit den ersten gelieferten Samples statt mit Stille.
        let master_gain = db_to_linear(state.timeline.master_gain_db);
        let mut wrote_any = false;
        let mut tick_tracks: std::collections::HashMap<String, [f32; 2]> =
            std::collections::HashMap::new();
        let mut tick_master = [0f32; 2];
        while master.is_processed() {
            self.mix_buf.clear();
            self.mix_buf.resize(AUDIO_CHUNK_FRAMES * AUDIO_CHANNELS, 0.0);
            let mut any_frames = false;
            for dec in self.audio_decoders.iter_mut() {
                let (frames, peak_l, peak_r) = dec.mix_into(&mut self.mix_buf);
                if frames == 0 {
                    continue;
                }
                any_frames = true;
                if let Some(track_id) = dec.track_id.as_ref() {
                    let entry = tick_tracks.entry(track_id.clone()).or_insert([0.0, 0.0]);
                    entry[0] = entry[0].max(peak_l);
                    entry[1] = entry[1].max(peak_r);
                }
            }
            if !any_frames {
                self.debug_starved += 1;
                break;
            }
            // Master-Fader anwenden, Spitzen vor dem Hard-Clip messen.
            for s in self.mix_buf.iter_mut() {
                *s *= master_gain;
            }
            for pair in self.mix_buf.chunks_exact(AUDIO_CHANNELS) {
                tick_master[0] = tick_master[0].max(pair[0].abs());
                tick_master[1] = tick_master[1].max(pair[1].abs());
            }
            for s in self.mix_buf.iter_mut() {
                *s = s.clamp(-1.0, 1.0);
            }
            master.write(&self.mix_buf);
            wrote_any = true;
            self.debug_blocks += 1;
        }

        // Pegel nur bei tatsächlich gemischten Blöcken aktualisieren —
        // sonst behalten die Meter den letzten Stand (kein Flackern,
        // wenn der Puffer in einem UI-Frame mal nicht frei wird).
        if wrote_any {
            state.audio.track_levels = tick_tracks;
            state.audio.master_level = tick_master;
        }

        self.debug_ticks += 1;
        if self.debug && now - self.debug_last >= 1.0 {
            self.debug_last = now;
            eprintln!(
                "[audio] {} Blöcke/s | {} Ticks/s | {} verhungert | {} Decoder | Master-Peak {:.3}/{:.3}",
                self.debug_blocks,
                self.debug_ticks,
                self.debug_starved,
                self.audio_decoders.len(),
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
    let make_target = |clip: &crate::core::timeline::TimelineClip, media_time: f64| {
        let asset = state.media.asset(&clip.asset_id)?;
        if asset.offline || asset.kind != crate::core::types::MediaKind::Video {
            return None;
        }
        let video = asset.info.video.first()?;
        Some((
            clip.id.clone(),
            VideoTarget {
                path: asset.path.clone(),
                media_time,
                src_w: video.width.max(2),
                src_h: video.height.max(2),
                fps: seq_fps,
                playing: state.playback.program_playing,
                rate: state.playback.program_rate,
                scale: state.monitor.program_scale,
                media_step: clip.media_step(),
            },
        ))
    };

    let mut targets: Vec<(String, VideoTarget)> = compose::visible_program_layers(&state.timeline, t)
        .into_iter()
        .filter_map(|layer| match layer {
            compose::ProgramLayer::Clip { clip, .. } => {
                make_target(clip, compose::clip_media_time(clip, t).max(0.0))
            }
            compose::ProgramLayer::Solid { .. } => None,
        })
        .collect();

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

fn source_video_target(state: &AppState) -> Option<VideoTarget> {
    let asset = state
        .playback
        .source_asset_id
        .as_ref()
        .and_then(|id| state.media.asset(id))?;
    if asset.kind != crate::core::types::MediaKind::Video {
        return None;
    }
    let video = asset.info.video.first()?;
    Some(VideoTarget {
        path: asset.path.clone(),
        media_time: state.playback.source.position,
        src_w: video.width.max(2),
        src_h: video.height.max(2),
        fps: video.fps,
        playing: state.playback.source.playing,
        rate: state.playback.source.rate,
        scale: state.monitor.source_scale,
        media_step: 1.0,
    })
}
