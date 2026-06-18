pub mod adjustment;
pub mod animation;
pub mod audio_fx;
pub mod autosave;
pub mod bin;
pub mod commands;
pub mod compose;
pub mod consolidate;
pub mod dock;
pub mod edit;
pub mod effects;
pub mod export;
pub mod export_preset;
pub mod frame_cache;
pub mod grade;
pub mod interop;
pub mod keyboard;
pub mod loudness;
pub mod lut;
pub mod marker;
pub mod mask;
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

/// Bytes durabel UND atomar nach `path` schreiben. Schreibt in eine Temp-Datei
/// im selben Verzeichnis, erzwingt deren Daten per `fsync` auf die Platte, setzt
/// sie per `rename` ein (atomarer Verzeichniseintrag) und synct danach das
/// Verzeichnis selbst. Damit hinterlässt ein Stromausfall/Kernel-Panic nie eine
/// halbe oder 0-Byte-Datei: entweder die alte Datei bleibt vollständig stehen
/// oder die neue ist komplett da. `std::fs::rename` allein garantiert nur die
/// Atomizität des Eintrags, NICHT die Durabilität der Datenblöcke.
pub fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(d) = dir {
        std::fs::create_dir_all(d)?;
    }
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "tmp".into());
    // Temp-Datei MUSS im Zielverzeichnis liegen — rename ist nur innerhalb eines
    // Dateisystems atomar.
    let tmp_name = format!(".{file_name}.tmp-{}", std::process::id());
    let tmp = match dir {
        Some(d) => d.join(tmp_name),
        None => std::path::PathBuf::from(tmp_name),
    };
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    // Verzeichniseintrag durabel machen (best effort — nicht jede Plattform
    // erlaubt das Syncen eines Verzeichnis-Handles).
    if let Some(d) = dir {
        if let Ok(dh) = std::fs::File::open(d) {
            let _ = dh.sync_all();
        }
    }
    Ok(())
}

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
