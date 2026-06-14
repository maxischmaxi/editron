pub mod animation;
pub mod audio_fx;
pub mod autosave;
pub mod bin;
pub mod commands;
pub mod compose;
pub mod dock;
pub mod edit;
pub mod effects;
pub mod export;
pub mod export_preset;
pub mod frame_cache;
pub mod grade;
pub mod interop;
pub mod keyboard;
pub mod marker;
pub mod multicam;
pub mod pixbuf;
pub mod playback;
pub mod player;
pub mod project;
pub mod proxy;
pub mod render_cache;
pub mod render_queue;
pub mod sequence;
pub mod sequences;
pub mod settings;
pub mod subtitle;
pub mod text_raster;
pub mod timecode;
pub mod timeline;
pub mod title;
pub mod title_engine;
pub mod transitions;
pub mod types;

/// Globaler, monoton steigender Operationszähler. Timeline- und Medien-Undo
/// führen je eine eigene History; damit `Rückgängig`/`Wiederholen` über beide
/// Stores hinweg in der richtigen zeitlichen Reihenfolge wirken, markiert jeder
/// History-Snapshot die Operation mit diesem Zähler (höchste Sequenz = jüngste
/// Operation). Prozessweit (wie `types::new_id`), daher ohne State-Threading.
pub fn next_op_seq() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}
