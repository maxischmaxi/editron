# Editron

Videoschnittprogramm mit dem Anspruch, mit DaVinci Resolve und Adobe Premiere Pro mitzuhalten.
Stack: Rust + raylib (eigenes komponentenbasiertes Immediate-Mode-UI-Framework, kein Webview), FFmpeg/ffprobe als Medien-Engine.

## Befehle

- `cargo run` — App im Dev-Modus starten; `cargo run -- projekt.etron` öffnet eine Projektdatei
- `cargo check` — Typen/Borrows prüfen (Standard-Verifikation)
- `cargo test` — Unit-Tests (Projekt-Roundtrip, Relink-Matching)
- `cargo build --release` — Release-Build
- Visueller Smoke-Test ohne Interaktion: `EDITRON_SHOT=shot.png EDITRON_SHOT_FRAME=300 EDITRON_TEST_IMPORT=a.mp4 EDITRON_TEST_TIMELINE=1 EDITRON_TEST_PLAY=1 ./target/debug/editron` (raylib speichert den Screenshot relativ zum CWD). Für Hover-Zustände zusätzlich `EDITRON_TEST_TOOL=razor` (Werkzeug vorwählen) und `EDITRON_TEST_MOUSE=x,y` (synthetische Mausposition in Fensterkoordinaten — Achtung, der WM kann das 1440×900-Startfenster umskalieren; Faktor aus der Screenshot-Größe ableiten).

## Architektur

Siehe `docs/ARCHITECTURE.md`. Kurzfassung:

- **Alles ist ein Command** (`src/core/commands.rs`): jede Aktion läuft über die Registry und ist dadurch frei mit Shortcuts belegbar. when-Klauseln steuern den Kontext; Kontextwerte werden direkt aus dem `AppState` abgeleitet.
- **UI-Framework** (`src/ui/`): Immediate-Mode mit persistenten Komponenten-Structs — `Ui`-Kontext (hot/active-IDs, Scissor-Clip-Stack, Tooltips, In-App-Drag&Drop, Command-Dispatch-Queue), Widgets in `src/ui/widgets/`, Lucide-Icons als tessellierte SVG-Pfade (`icons_data.rs`, generiert via `tools/extract_icons.mjs`), Fonts über fontconfig (Inter → Noto Sans) mit 2×-Supersampling.
- **Timeline-Sequenz** (`src/core/timeline.rs`): Tracks/Clips-Modell mit verknüpften A/V-Paaren, Undo/Redo-History und allen Editier-Operationen im Store; UI in `src/panels/timeline.rs`. Kontextmenüs über `src/overlays/context_menu.rs` (commandfähig, zeigt Shortcuts an).
- **Shortcut-System** (`src/core/keyboard.rs`): `"Mod+K Mod+S"`-Format, Mod = Cmd/Ctrl je OS, Presets `editron`/`premiere`/`resolve`, Nutzer-Overrides persistiert als `keymap.json` im XDG-Config-Verzeichnis.
- **Panels** (`src/panels/`) registrieren sich im `PanelHost` (Instanzen mit eigenem UI-State); **Workspaces** (Medien/Schnitt/Farbe/Effekte/Audio/Grafik) sind Dock-Layouts (`src/core/dock.rs` + `src/shell/dock_host.rs`) mit Persistenz pro Workspace unter `~/.local/share/editron/layouts/`.
- **Projektdateien** (`src/core/project.rs`): `.etron` = versioniertes JSON (Workspace, Medien, Timeline, Quellmonitor); atomares Speichern (tmp+rename+`.bak`), Dirty-Tracking über Revision-Zähler in Timeline-/MediaStore, Datei-Menü in der TitleBar, Mod+N/O/S/Shift+S. Öffnen per CLI-Arg/Drag&Drop/OS-Doppelklick (`packaging/`, macOS-odoc in `src/platform/`). Fehlende Medien → `MediaAsset.offline` + Relink-Wizard (`overlays/relink_dialog.rs`): Ordner-Scan im Worker (Name+Größe-Match) oder manuelle Zuweisung, beides re-probt.
- **Medien-Engine** (`src/services.rs`): FFmpeg als externes Binary (PATH bzw. `EDITRON_FFMPEG_PATH`), Probe/Thumbnail/Waveform/Transcode in Worker-Threads, Ergebnisse als Events in den UI-Thread.
- **Wiedergabe** (`src/core/player.rs`): dekodiert Video über ffmpeg-Pipes (rawvideo/rgba → Texture im `TextureCache` unter `player://program|source`) und Audio über einen eigenen Mixdown (je Clip ein ffmpeg-`f32le`-Decoder; Engine summiert mit Spur-Gain/Pan, Clip-Gain, Master-Fader in EINEN raylib-AudioStream und schreibt Spitzenpegel nach `state.audio` für den Mixer). Transport-Routing in `src/core/playback.rs` (Fokus auf Quellmonitor steuert die Quelle, sonst das Programm). Audio-Fallen: raylib-rs' `AudioStream::update` übergibt Bytes statt Frames (FFI direkt nutzen, siehe `MasterStream`), und der Mix-Block muss ≥ der Geräte-Periode sein (sonst Stille-Padding → zerhackter Ton).

## Konventionen

- UI-Texte deutsch (korrekte Umlaute), Code/Bezeichner englisch.
- Design-Tokens aus `src/theme.rs` verwenden (`SURFACE_0..4`, `LINE`, `TEXT_1..3`, `ACCENT`, …); Maße folgen der 4-px-Skala.
- Neue Aktionen IMMER als Command registrieren (nie nackte Klick-Logik für app-weite Aktionen) und im Default-Preset verdrahten.
- Texture-Uploads nur zwischen den Frames (Panels fordern Bilder über `ui.texture_requests` an; der Mainloop lädt sie vor dem nächsten Frame).
- raylib-rs-Eigenheiten: `RaylibFont`-Trait für `measure_text`, `RaylibTexture2D` für `update_texture`/`set_texture_filter`, `get_mouse_wheel_move_v()` liefert `ffi::Vector2` (`.into()`).
