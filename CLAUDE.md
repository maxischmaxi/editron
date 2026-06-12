# Editron

Videoschnittprogramm mit dem Anspruch, mit DaVinci Resolve und Adobe Premiere Pro mitzuhalten.
Stack: Rust + raylib (eigenes komponentenbasiertes Immediate-Mode-UI-Framework, kein Webview), FFmpeg/ffprobe als Medien-Engine.

## Befehle

- `cargo run` — App im Dev-Modus starten; `cargo run -- projekt.etron` öffnet eine Projektdatei
- `cargo check` — Typen/Borrows prüfen (Standard-Verifikation)
- `cargo test` — Unit-Tests (Projekt-Roundtrip, Relink-Matching)
- `cargo build --release` — Release-Build
- Visueller Smoke-Test ohne Interaktion: `EDITRON_SHOT=shot.png EDITRON_SHOT_FRAME=300 EDITRON_TEST_IMPORT=a.mp4 EDITRON_TEST_TIMELINE=1 EDITRON_TEST_PLAY=1 ./target/debug/editron` (raylib speichert den Screenshot relativ zum CWD). Für Hover-Zustände zusätzlich `EDITRON_TEST_TOOL=razor` (Werkzeug vorwählen) und `EDITRON_TEST_MOUSE=x,y` (synthetische Mausposition in Fensterkoordinaten — Achtung, der WM kann das 1440×900-Startfenster umskalieren; Faktor aus der Screenshot-Größe ableiten).
- Weitere Test-Flags: `EDITRON_TEST_DIALOG=export|shortcuts|relink` öffnet den Dialog beim Start; `EDITRON_TEST_EXPORT=/tmp/out.mp4` startet im Export-Dialog automatisch einen Export auf dieses Ziel, sobald die Validierung durchgeht (App-End-to-End, Running-/Done-Screenshots).

## Architektur

Siehe `docs/ARCHITECTURE.md`. Kurzfassung:

- **Alles ist ein Command** (`src/core/commands.rs`): jede Aktion läuft über die Registry und ist dadurch frei mit Shortcuts belegbar. when-Klauseln steuern den Kontext; Kontextwerte werden direkt aus dem `AppState` abgeleitet.
- **UI-Framework** (`src/ui/`): Immediate-Mode mit persistenten Komponenten-Structs — `Ui`-Kontext (hot/active-IDs, Scissor-Clip-Stack, Tooltips, In-App-Drag&Drop, Command-Dispatch-Queue), Widgets in `src/ui/widgets/`, Lucide-Icons als tessellierte SVG-Pfade (`icons_data.rs`, generiert via `tools/extract_icons.mjs`), Fonts über fontconfig (Inter → Noto Sans) mit 2×-Supersampling.
- **Timeline-Sequenz** (`src/core/timeline.rs`): Tracks/Clips-Modell mit verknüpften A/V-Paaren, Undo/Redo-History und allen Editier-Operationen im Store; UI in `src/panels/timeline.rs`. Kontextmenüs über `src/overlays/context_menu.rs` (commandfähig, zeigt Shortcuts an).
- **Keyframe-Animation** (`src/core/animation.rs` + `src/core/compose.rs`): `TimelineClip.fx` = animierbare Parameter (Position/Skalierung/Rotation/Deckkraft/Lautstärke) als `AnimatedParam` (statisch oder Keyframe-Kurve mit Linear/Halten/Ease-Interpolation). Keyframe-Zeiten in MEDIENZEIT (kleben am Material), Werte auflösungsunabhängig (% der Framemaße bzw. Contain-Fit-Größe). Edits über `TimelineStore::fx_*` (undo-fähig, Gesten via `begin_fx_edit`). Keyframe-Editor im Panel Effekteinstellungen (`src/panels/effect_controls.rs`: Stopwatch, Wert-Scrubbing, ◀ ◆ ▶, Keyframe-Spuren mit Drag/Box-Auswahl/Interpolations-Kontextmenü), direkte Manipulation per Transform-Gizmo im Programmmonitor (`src/panels/transform_gizmo.rs`: Move/Scale-Handles/Rotations-Griff). Player und Export komponieren ALLE sichtbaren Layer unten → oben (Monitor via `draw_texture_pro`, Export via CPU-Compositor `compose::composite_frame` mit Schnellpfad für untransformierte Einzel-Layer).
- **Shortcut-System** (`src/core/keyboard.rs`): `"Mod+K Mod+S"`-Format, Mod = Cmd/Ctrl je OS, Presets `editron`/`premiere`/`resolve`, Nutzer-Overrides persistiert als `keymap.json` im XDG-Config-Verzeichnis.
- **Panels** (`src/panels/`) registrieren sich im `PanelHost` (Instanzen mit eigenem UI-State); **Workspaces** (Medien/Schnitt/Farbe/Effekte/Audio/Grafik) sind Dock-Layouts (`src/core/dock.rs` + `src/shell/dock_host.rs`) mit Persistenz pro Workspace unter `~/.local/share/editron/layouts/`.
- **Projektdateien** (`src/core/project.rs`): `.etron` = versioniertes JSON (Workspace, Medien, Timeline, Quellmonitor); atomares Speichern (tmp+rename+`.bak`), Dirty-Tracking über Revision-Zähler in Timeline-/MediaStore, Datei-Menü in der TitleBar, Mod+N/O/S/Shift+S. Öffnen per CLI-Arg/Drag&Drop/OS-Doppelklick (`packaging/`, macOS-odoc in `src/platform/`). Fehlende Medien → `MediaAsset.offline` + Relink-Wizard (`overlays/relink_dialog.rs`): Ordner-Scan im Worker (Name+Größe-Match) oder manuelle Zuweisung, beides re-probt.
- **Medien-Engine** (`src/services.rs`): FFmpeg als externes Binary (PATH bzw. `EDITRON_FFMPEG_PATH`), Probe/Thumbnail/Waveform/Encoder-Liste in Worker-Threads, Ergebnisse als Events in den UI-Thread.
- **Sequenz-Export** (`src/core/export.rs` + `src/overlays/export_dialog.rs`): Container-/Codec-Katalog (MP4/MOV/MKV/WebM/WAV/MP3/FLAC/M4A; H.264/H.265/ProRes/DNxHR/VP9/AV1; AAC/MP3/Opus/FLAC/PCM) mit Render-Presets, Live-Validierung (Fehler blockieren den Start) und Renderplan in Player-Semantik (Layer-Stapel je Segment mit ClipFx-Kurven, Audio-Gains wie Mixdown inkl. Lautstärke-Hüllkurve). Worker rendert zweiphasig (Audio-RMW-Mix in tmp-WAV → Video segmentweise: Schnellpfad-Pipe oder CPU-Compositor mit einem Decoder je Layer, rawvideo/rgba in einen ffmpeg-Encoder), schreibt nach `<ziel>.part` + atomarem Rename, meldet %/Frames/fps/ETA als Events; Abbruch killt die ffmpeg-Kinder über die Job-Registry (`cancel_job`, `cancel_all_jobs` beim App-Ende). Dialog ist modal, Schließen während des Renderns gesperrt.
- **Wiedergabe** (`src/core/player.rs`): dekodiert Video über ffmpeg-Pipes (rawvideo/rgba → Texturen im `TextureCache`; Programm = EIN Decoder je sichtbarem Video-Layer unter `player://clip/<id>`, Quelle unter `player://source`; der Programmmonitor komponiert die Layer mit ihren Transformationen) und Audio über einen eigenen Mixdown (je Clip ein ffmpeg-`f32le`-Decoder; Engine summiert mit Spur-Gain/Pan, Clip-Gain inkl. Lautstärke-Keyframes, Master-Fader in EINEN raylib-AudioStream und schreibt Spitzenpegel nach `state.audio` für den Mixer). Transport-Routing in `src/core/playback.rs` (Fokus auf Quellmonitor steuert die Quelle, sonst das Programm). Audio-Fallen: raylib-rs' `AudioStream::update` übergibt Bytes statt Frames (FFI direkt nutzen, siehe `MasterStream`), und der Mix-Block muss ≥ der Geräte-Periode sein (sonst Stille-Padding → zerhackter Ton).

## Konventionen

- UI-Texte deutsch (korrekte Umlaute), Code/Bezeichner englisch.
- Design-Tokens aus `src/theme.rs` verwenden (`SURFACE_0..4`, `LINE`, `TEXT_1..3`, `ACCENT`, …); Maße folgen der 4-px-Skala.
- Neue Aktionen IMMER als Command registrieren (nie nackte Klick-Logik für app-weite Aktionen) und im Default-Preset verdrahten.
- Texture-Uploads nur zwischen den Frames (Panels fordern Bilder über `ui.texture_requests` an; der Mainloop lädt sie vor dem nächsten Frame).
- raylib-rs-Eigenheiten: `RaylibFont`-Trait für `measure_text`, `RaylibTexture2D` für `update_texture`/`set_texture_filter`, `get_mouse_wheel_move_v()` liefert `ffi::Vector2` (`.into()`).
