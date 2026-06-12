# Editron — Architektur

Editron ist ein natives Videoschnittprogramm in **Rust** mit **raylib** als
Render-/Fenster-Schicht und **FFmpeg/ffprobe** als Medien-Engine. Das gesamte
UI ist ein eigenes, komponentenbasiertes Immediate-Mode-Framework — kein
Webview, keine Fremd-GUI-Bibliothek. Alles läuft in einem Prozess; Threads
gibt es nur für Medien-Arbeit (Probe, Thumbnails, Waveforms, Decode, Transcode).

## Leitprinzipien

1. **Alles ist ein Command.** Jede Aktion (Button, Shortcut, Menü, Palette)
   läuft über die `CommandRegistry` (`src/core/commands.rs`). Dadurch ist jede
   Funktion mit einem eigenen Shortcut belegbar.
2. **Kontext statt Fokus-Hacks.** `when`-Klauseln (wie in VS Code) entscheiden,
   welcher Command in welchem Kontext greift. Kontext-Keys werden direkt aus
   dem `AppState` abgeleitet (`context_value()`): `panel` (fokussiertes Panel),
   `workspace`, `dialogOpen`, `commandPaletteOpen`, `tool`, `mediaSelected`,
   `timelineHasClips`, `timelineClipSelected`, `timelineClipboard`,
   `timelineCanUndo`, `timelineCanRedo`, `timelineInOutSet`.
3. **Panels sind Module.** Jedes Panel ist eine Komponente mit eigenem
   UI-State im `PanelHost` (`src/panels/mod.rs`) und damit in jedem Workspace
   andockbar — wie in Premiere.
4. **Workspaces = Layouts.** Ein Workspace (Medien, Schnitt, Farbe, Effekte,
   Audio, Grafik) ist ein benanntes Dock-Layout. Nutzeränderungen werden pro
   Workspace persistiert; „Layout zurücksetzen“ stellt das Default wieder her.
5. **UI deutsch, Code englisch.** Nutzersichtbare Strings sind deutsch;
   Bezeichner, Dateien und Kommentare folgen dem Code-Stil des Repos.

## Verzeichnisstruktur

```
src/
├── main.rs              # Mainloop: Input sammeln → Ticks (Services/Playback/
│                        # Player) → UI-Pass → Command-Dispatch → Texture-Uploads
├── theme.rs             # Design-Tokens (surface-/text-/accent-Farben, Maße)
├── state.rs             # AppState: alle Stores + Workspace-Wechsel
├── stores.rs            # AppStore/MediaStore/PlaybackStore/MonitorStore
├── services.rs          # Medien-Engine: Import-Dialog, ffprobe, Thumbnails,
│                        # Waveforms, Transcode-Jobs (Threads → Event-Kanal)
├── ui/                  # UI-Framework (app-unabhängig)
│   ├── mod.rs           # Ui-Kontext: hot/active, Clip-Stack, Tooltips,
│   │                    # In-App-DnD, Dispatch-Queue, Zeichen-Helfer
│   ├── geom.rs          # Rect + RectCut-Layout
│   ├── input.rs         # Maus/Tastatur/Wheel/File-Drop; Browser-Key-Namen
│   ├── text.rs          # fontconfig-Discovery, 2×-Supersampling-Atlanten
│   ├── icons.rs         # SVG-Pfad-Parser/Tessellation für Lucide-Icons
│   ├── icons_data.rs    # generiert (tools/extract_icons.mjs)
│   ├── textures.rs      # Lazy-Texture-Cache (Thumbnails, Player-Frames)
│   └── widgets/         # Buttons, TextInput, ScrollArea, Slider, Select
├── core/
│   ├── commands.rs      # Registry, when-Evaluator, alle Builtin-Commands
│   ├── keyboard.rs      # Keybinding-Parser, Resolver (Sequenz-Puffer),
│   │                    # Presets editron/premiere/resolve, KeymapStore
│   ├── timeline.rs      # Sequenz-Modell + Store (Undo/Redo, alle Operationen)
│   ├── project.rs       # Projektdateien (.etron): Format, Save/Load,
│   │                    # ProjectStore (dirty/recent), Autosave
│   ├── dock.rs          # Dock-Modell: Split-Baum, Gruppen, Default-Layouts,
│   │                    # Persistenz (~/.local/share/editron/layouts/)
│   ├── export.rs        # Sequenz-Export: Codec-Katalog, Presets, Validierung,
│   │                    # Renderplan + Render-Worker (Mixdown → WAV, Video-
│   │                    # Segmente → ffmpeg-Encoder, atomares Finalisieren)
│   ├── playback.rs      # Transport-Routing Quelle/Programm, JKL, Loop, Tick
│   ├── player.rs        # Decode-Engine (ffmpeg → Texturen/AudioStreams)
│   ├── timecode.rs      # HH:MM:SS:FF + Dauer-Formatierung
│   └── types.rs         # Medien-Typen (MediaAsset, MediaInfo, …)
├── platform/            # OS-Integration: macOS-Apple-Event (odoc) für
│                        # „Öffnen mit“ (Linux/Windows liefern argv)
├── shell/               # TitleBar (inkl. Datei-Menü), StatusBar, DockHost
├── panels/              # media_browser, monitor (Quelle+Programm), timeline,
│                        # effects, effect_controls, audio_mixer, color,
│                        # scopes, graphics, info (+history)
└── overlays/            # context_menu, command_palette, shortcut_editor,
                         # export_dialog, relink_dialog
```
Dazu `packaging/` (außerhalb von `src/`): Doppelklick-Integration für
`.etron`-Dateien je OS (Linux .desktop+MIME, Windows-Registry-Skript,
macOS-App-Bundle mit Info.plist) — siehe `packaging/README.md`.

## UI-Framework

Immediate-Mode mit persistenten Komponenten: Jeder Frame baut die UI neu auf,
Komponenten-Structs (Panels, Overlays) halten ihren UI-State (Scroll-Offsets,
Suchtexte, Drag-Zustände) zwischen den Frames. Der `Ui`-Kontext bündelt
Zeichen-Handle, Input, Fonts, Icons, Texture-Cache und Interaktions-State:

- **hot/active-IDs:** klassisches IMGUI-Modell — `interact(id, rect)` liefert
  hovered/held/clicked/double/right_clicked; ein aktives Widget monopolisiert
  die Maus bis zum Release.
- **Schichten:** Ist ein Overlay offen (Menü, Palette, Dialog, Select-Popup),
  sieht die Hauptschicht keine Maus (`begin_main_layer(overlay_open)`).
- **Clipping:** `push_clip`/`pop_clip` als Scissor-Stack; Maus-Hit-Tests
  respektieren den aktuellen Clip (gescrollte Inhalte).
- **In-App-Drag&Drop:** `start_drag(payload)` (Assets, Dock-Tabs) aktiviert
  sich nach 4 px; Drop-Ziele prüfen `drag_over`/`accept_drop`; der Mainloop
  zeichnet den Ghost. Natives File-Drop (raylib) importiert direkt.
- **Commands aus der UI:** `ui.run_command(...)` landet in einer
  Dispatch-Queue, die der Mainloop nach dem UI-Pass ausführt — Mutationen am
  `AppState` passieren nie mitten im Zeichnen eines anderen Panels.
- **Texturen:** GPU-Uploads brauchen den raylib-Handle, der während des
  Zeichnens geborgt ist — Panels fordern Bilder über `ui.texture_requests`
  an, der Mainloop lädt sie vor dem nächsten Frame (1 Frame Latenz).

## Shortcut-System

- **Format:** `"Mod+Shift+K"`, Sequenzen `"Mod+K Mod+S"`; `Mod` = Cmd (macOS) /
  Ctrl (sonst).
- **Auflösung:** Resolver mit Sequenz-Puffer (Timeout 1,2 s); effektive
  Bindings = aktives Preset + Nutzer-Overrides (Overrides ersetzen die
  Preset-Bindings eines Commands vollständig). Binding-`when` UND
  Command-`when` müssen passen.
- **Presets:** `editron` (Default), `premiere`, `resolve`. Persistenz als
  `keymap.json` im XDG-Config-Verzeichnis.
- **UI:** Shortcut-Editor (durchsuchbar, Aufnahme-Modal, Konfliktanzeige,
  Preset-Wahl) + Befehlspalette.

## Timeline (Sequenz-Editing)

- **Modell** in `src/core/timeline.rs`: `TimelineTrack` (video/audio,
  mute/solo/lock) und `TimelineClip` (start/duration/src_in/src_duration,
  `link_id` für Video↔Audio-Paare). Alle Zeiten in Sekunden, Sequenz-Framerate
  aktuell fix 25 fps (`SEQUENCE_FPS`).
- **Alle Editier-Operationen laufen über den `TimelineStore`** und sind damit
  undo-fähig (Snapshot-History): Einfügen mit Overwrite-Semantik, Move (inkl.
  Spurwechsel), Duplizieren, Trim/Ripple-Trim/Roll/Slip/Slide, Razor-Split,
  (Ripple-)Delete, Link/Unlink, Enable/Disable, Spuren anlegen/entfernen,
  Copy/Paste. Paste ist nicht-destruktiv: Ist der Zielbereich belegt, weicht
  der Clip auf die nächste freie Spur aus (Video nach oben, Audio nach unten);
  existiert keine, wird eine neue Spur angelegt.
- **Drop aus dem Medien-Browser:** Video- und Audio-Anteil eines Assets landen
  verknüpft auf Video-/Audiospur; fehlende Spuren werden automatisch angelegt
  (`plan_asset_placements` liefert dieselbe Planung für Drop-Vorschau und
  Einfügen).
- **Interaktion im Panel:** Snapping (Clipkanten, Playhead, 0) mit Hilfslinie,
  Marquee-Auswahl, Shift/Mod+Mausrad-Zoom um den Cursor, Alt+Rad horizontal,
  Mittelmaus-/Hand-Pan, Werkzeuge entscheiden die Drag-Semantik. Während eines
  Drags rendert die Timeline eine Vorschau; der Store wird erst beim Loslassen
  mutiert (ein Undo-Schritt pro Geste). Alt+Drag dupliziert die gezogenen
  Clips (Premiere-Konvention; der Alt-Zustand beim Loslassen entscheidet),
  die Rasierklinge zeichnet eine rote Schnittvorschau über dem Clip unter der
  Maus und seinen Link-Partnern, Tool-Tooltips zeigen den Shortcut der
  aktiven Keymap.
- **In/Out-Punkte der Sequenz** definieren den Loop-Bereich der
  Programm-Wiedergabe. Im Lineal: Alt+Ziehen zieht den Bereich auf, Ziehen an
  den Bereichskanten verschiebt In/Out, Kontextmenü und `I`/`O`/`Mod+Shift+X`
  setzen bzw. löschen die Punkte.

## Animation (Keyframes)

- **Modell** in `src/core/animation.rs`: `ClipFx` hängt an jedem
  `TimelineClip` (`#[serde(default)]`, wird nur bei Abweichung vom Standard
  serialisiert) und bündelt die animierbaren Parameter — Position X/Y,
  Skalierung X/Y (mit „Seitenverhältnis beibehalten“), Rotation, Deckkraft,
  Lautstärke. Jeder Parameter ist ein `AnimatedParam`: statischer Wert oder
  Keyframe-Kurve (`Vec<Keyframe>`, sortiert) mit Interpolation pro Segment
  (Linear, Halten, Ease In/Out/InOut). **Keyframe-Zeiten sind Medienzeit**
  (Sekunden in der Quelle): Clips verschieben ändert nichts, Kopf-Trimmen
  verschiebt die Animation relativ zum Clipanfang (Premiere-Semantik).
- **Einheiten sind auflösungsunabhängig** (Position in % der Framemaße als
  Offset vom Zentrum, Skalierung in % der Contain-Fit-Größe), damit Vorschau
  und Export bei jeder Auflösung identisch aussehen. Die gemeinsame Mathematik
  (sichtbare Layer am Playhead, `eval_fx`, Transform-Quad, CPU-Compositor)
  liegt in `src/core/compose.rs`.
- **Alle fx-Edits laufen über den `TimelineStore`** (`fx_*`-Methoden) und sind
  undo-fähig; Drag-Gesten legen über `begin_fx_edit()` genau einen Snapshot an
  und schreiben dann live (`fx_set_value_live`, `fx_replace_keys_live`).
  Ist die Stopwatch eines Parameters an, schreiben Wertänderungen Keyframes am
  Playhead, sonst den statischen Wert.
- **UI:** Das Panel **Effekteinstellungen** (`src/panels/effect_controls.rs`)
  ist der Keyframe-Editor — links Parameter-Zeilen (Stopwatch, Wert-Scrubbing
  mit Doppelklick-Eingabe, ◀ ◆ ▶ Keyframe-Navigation, Abschnitts-Reset),
  rechts je Parameter eine Keyframe-Spur über die Clipdauer mit Playhead-
  Lineal: Keyframes ziehen (auch Mehrfachauswahl), Strg/Shift-Klick,
  Box-Auswahl, Doppelklick legt Keyframes an, Entf löscht, Rechtsklick öffnet
  das Interpolations-Menü. Im **Programmmonitor** manipuliert das
  Transform-Gizmo (`src/panels/transform_gizmo.rs`) den ausgewählten Clip
  direkt: Ziehen verschiebt (Shift rastet die Achse), 8 Handles skalieren
  (Ecken proportional, Kanten je Achse — entkoppelt die Skalierung
  automatisch, Shift verzerrt), der Griff über der Oberkante rotiert
  (Shift: 15°-Raster); Klick auf einen Layer wählt dessen Clip. Clips mit
  Keyframes tragen in der Timeline eine Rauten-Badge;
  `clip.prevKeyframe`/`clip.nextKeyframe` (Shift+↑/↓) springen den Playhead,
  `clip.resetMotion` setzt die Bewegung zurück.

## Wiedergabe (Monitore)

- **Programmmonitor = Sequenz-Wiedergabe.** `src/core/playback.rs` führt die
  Master-Clock (Playhead läuft pro Frame um `dt × rate` weiter, Loop über die
  Sequenz-In/Out-Punkte); `src/core/player.rs` startet **einen ffmpeg-Decoder
  je sichtbarem Video-Clip** am Playhead (`rawvideo/rgba`, feste Framerate)
  in je eine Texture (`player://clip/<id>` im TextureCache) — Frames werden
  gedroppt, wenn ein Decoder hinterherhinkt, Sessions werden bei
  Seeks/Clip-Wechseln neu aufgesetzt, nicht mehr sichtbare Layer geben ihre
  Texture frei. Der Monitor (`src/panels/monitor.rs`) komponiert die Layer
  von unten nach oben auf das Programm-Canvas (Seitenverhältnis aus
  `suggested_resolution`): Transform-Quad per `compose::layer_quad`,
  gezeichnet über `draw_texture_pro` mit Rotation und Deckkraft-Tint
  (Standbilder direkt aus dem TextureCache, ohne Decoder). Ton kommt aus den
  aktiven Audio-Clips über einen eigenen Mixdown: je Clip ein
  ffmpeg-`f32le`-Decoder, die Engine summiert blockweise (Spur-Gain/Pan,
  Clip-Gain inkl. Lautstärke-Keyframes — pro Tick ausgewertet —,
  Master-Fader, Mute/Solo) in einen einzelnen raylib-AudioStream und misst
  dabei Spitzenpegel pro Spur und Summe (`state.audio`, Anzeige im
  Audio-Mixer). Decoder werden bei Drift > 0,35 s (Seek/Loop während der
  Wiedergabe) neu positioniert. Achtung, zwei raylib-Fallen:
  `AudioStream::update` aus raylib-rs übergibt Bytes statt Frames (deshalb
  direkter FFI-Aufruf in `MasterStream::write`), und raylib hebt die
  Sub-Buffer-Größe still auf die Geräte-Periode an — der Mix-Block
  (`AUDIO_CHUNK_FRAMES`, 4096) muss darüber liegen, sonst wird jeder Block mit
  Stille aufgefüllt und der Ton zerhackt.
- **Quellmonitor = Einzel-Asset-Player:** explizit geladenes Asset (Doppelklick
  im Medien-Browser, „In Quellmonitor laden“, Doppelklick auf Timeline-Clip),
  lokale In/Out-Marken, optionaler Loop.
- **Controller-Routing:** Wiedergabe-Commands (Space, JKL, I/O, Frame-Stepping)
  wirken auf den aktiven Monitor: Fokus auf dem Quellmonitor-Panel steuert die
  Quelle, alles andere das Programm.
- **Wiedergabeauflösung** (Voll, 1/2, 1/4, 1/8 je Monitor): reduzierte Stufen
  dekodieren direkt in kleinere Frames — spart Decode- und Upload-Kosten.

## Projektdateien (.etron)

- **Format** (`src/core/project.rs`): versioniertes JSON mit Magic
  (`{"format":"editron-project","version":1,…}`) — enthält aktiven Workspace,
  alle Medien-Assets (inkl. Metadaten/Thumbnail-Pfad), die komplette Timeline
  (Tracks, Clips, Playhead, In/Out, Zoom, Snapping, Auswahl) und den
  Quellmonitor-Zustand. `src_duration = INFINITY` (Standbilder) wird als
  `null` serialisiert (JSON kennt kein Infinity). Neuere Formatversionen
  werden mit klarer Fehlermeldung abgelehnt.
- **Atomar speichern:** Temp-Datei im Zielordner → bestehende Datei als
  `.bak` kopieren → `rename`. Eine korrupte/halbe Projektdatei kann so nicht
  entstehen.
- **Dirty-Tracking über Revisionen:** `TimelineStore.revision` zählt
  strukturelle Edits (push_history/undo/redo), `MediaStore.revision`
  Bestandsänderungen; der Mainloop vergleicht pro Frame und pflegt
  Fenstertitel (`Name • — Editron`) und TitleBar. Playhead/Zoom/Auswahl
  machen das Projekt bewusst nicht dirty (werden aber mitgespeichert).
- **Lebenszyklus:** `project.new/open/save/saveAs/openRecent` (Datei-Menü in
  der TitleBar, Mod+N/O/S/Shift+S in allen Presets). Vor Projektwechseln und
  beim Beenden sichert `safeguard_unsaved`: mit Pfad → normales Speichern,
  ohne Pfad → Sitzungs-Autosave (`~/.local/share/editron/autosave.etron`,
  Menü „Letzte Sitzung wiederherstellen“). Zuletzt-geöffnet-Liste im
  XDG-Config (`recent_projects.json`).
- **Öffnen über OS:** CLI-Argument (`editron projekt.etron`; Medienpfade
  werden stattdessen importiert), Drag&Drop einer .etron ins Fenster,
  Doppelklick je OS via `packaging/` (Linux MIME+desktop, Windows HKCU,
  macOS-Bundle; Apple-Event-Handler in `src/platform/macos.rs`).
- **Fehlende Medien (Relink):** Beim Laden wird die Existenz jeder Quelldatei
  geprüft (`MediaAsset.offline`). Offline-Medien: Badge im Browser, rote
  Clips in der Timeline, automatisch geöffneter **Relink-Wizard**
  (`overlays/relink_dialog.rs`, auch über Datei-Menü). Der Wizard sucht
  rekursiv in einem gewählten Ordner (Worker-Thread, Fortschritt, Abbruch;
  Match per Dateiname, Gleichstand entscheidet die Dateigröße, jeder Fund
  wird nur einmal vergeben) oder weist einzelne Dateien manuell zu — beides
  re-probt via ffprobe und erneuert Thumbnail/Waveform; der Player erkennt
  den Pfadwechsel und startet die Session selbst neu.

## Medien-Engine (FFmpeg)

`src/services.rs` kapselt alle FFmpeg-Aufrufe in Worker-Threads; Ergebnisse
kommen als `ServiceEvent` über einen Kanal in den UI-Thread:

| Funktion | Zweck |
| --- | --- |
| `ffmpeg_info` | Binary-Discovery + Version (PATH bzw. `EDITRON_FFMPEG_PATH`) |
| `request_encoder_list` | verfügbare Encoder (`ffmpeg -encoders`) für die Export-Validierung |
| Import-Pipeline | Datei-Dialog (rfd) → ffprobe → Thumbnail → `MediaAsset` |
| `extract_waveform` | Peaks 0..1 (PCM-Streaming, Bucket-Faltung) |
| `start_sequence_export`/`cancel_job`/`cancel_all_jobs` | Render-Worker des Sequenz-Exports (Abbruch-Flag + Kill der ffmpeg-Kinder; `cancel_all_jobs` beim App-Ende gegen Waisenprozesse) |
| `reveal_in_file_manager` | Finder/Explorer/FileManager1-D-Bus/xdg-open |

## Sequenz-Export

`src/core/export.rs` + `src/overlays/export_dialog.rs` — Export der Timeline
in eine Datei, semantisch identisch zur Wiedergabe (alle sichtbaren
Video-Layer mit animierten Transformationen, unten → oben komponiert;
Audio-Summe mit Spur-Gain/Pan, Clip-Gain inkl. Lautstärke-Keyframes,
Master-Fader, Mute/Solo, `enabled`):

- **Katalog:** Container (MP4, MOV, MKV, WebM, WAV, MP3, FLAC, M4A) ×
  Video-Codecs (H.264, H.265 — `hvc1`-Tag, ProRes mit Profilen, DNxHR mit
  Profilen, VP9 — CRF braucht `-b:v 0`, SVT-AV1) × Audio-Codecs (AAC, MP3,
  Opus — erzwingt 48 kHz, FLAC, PCM 16/24/32f). Render-Presets (YouTube,
  Vimeo, Master, ProRes/DNxHR, Audio-only) als Settings-Fabriken.
- **Renderplan** (`build_render_plan`, pur + getestet): Zeitachse in
  Ziel-Frames quantisiert, an Clip-Grenzen geschnitten; jeder Abschnitt trägt
  seinen **Layer-Stapel** (`VideoLayerPlan` mit Pfad, Medienzeit und
  `ClipFx`-Kurven, unten → oben). Lücken werden Schwarzbild, Standbilder
  rendern über `-loop 1`. Audio-Clips mit vorgerechneten Links/Rechts-Gains
  und der Lautstärke-Kurve des Clips.
- **Validierung** (`validate`): FFmpeg/Encoder vorhanden, Timeline/Bereich
  nicht leer, Offline-Medien (Warnung), Auflösung gerade/in Grenzen,
  Zielordner existiert, Ziel ≠ Quelldatei (Fehler!), Überschreib-Warnung,
  WAV-4-GB-Grenze. Fehler blockieren den Start, Warnungen nicht.
- **Worker** (`run_export_worker`, eigener Thread, `catch_unwind`):
  Phase A mischt Audio clipweise per ffmpeg-`f32le`-Decoder via
  Read-Modify-Write in eine temporäre Float-WAV (konstanter RAM, Stille via
  `set_len`; Lautstärke-Keyframes als Hüllkurve in 256-Frame-Blöcken).
  Phase B rendert Video segmentweise in einen ffmpeg-Encoder-Prozess
  (BT.709-Matrix + Tags, expliziter Muxer): untransformierte Einzel-Layer
  laufen als **Schnellpfad** direkt durch eine Decoder-Pipe
  (`scale`+`pad`+`fps`, exakte Frame-Zahl, fehlende Frames wiederholen den
  letzten); Stapel oder transformierte Layer gehen durch den
  **CPU-Compositor** (`compose::composite_frame`): ein Decoder je Layer
  liefert transparent gepolsterte RGBA-Frames (Decode-Auflösung wächst mit
  der maximalen Skalierung im Segment, gedeckelt bei 2×/4096), pro Frame
  werden die Kurven ausgewertet und per inverser Abbildung (Rotation/
  Skalierung/Position, bilinear, Src-over) in Zeilenbändern parallel auf das
  Canvas gemischt. Geschrieben wird nach `<ziel>.part`, bei Erfolg atomar
  umbenannt; Abbruch/Fehler räumen `.part` + WAV auf. Fortschritt als Events
  (% über Phasen-Gewichte, Frame x/y, fps, ETA über geglättete Rate).
- **Dialog:** modal (Overlay blockiert Hauptschicht auf allen OS), links
  Presets, rechts Einstellungen mit Live-Validierung und Größenschätzung;
  während des Renderns sind Schließen/Esc gesperrt, „Abbrechen“ killt die
  ffmpeg-Kinder über die Job-Registry. Erfolgsansicht mit „Im Dateimanager
  zeigen“, Fehleransicht mit ffmpeg-stderr-Auszug.

## Bewusste Entscheidungen

- **FFmpeg als externes Binary** (System-PATH, später bündelbar) statt
  libav-Bindings: lizenzfreundlich, robust, Updates unabhängig vom App-Build.
- **Layout-Persistenz als JSON-Dateien** (`~/.local/share/editron/layouts/`),
  Keymaps als `keymap.json` im XDG-Config-Verzeichnis: Layouts sind
  fensterspezifisch und unkritisch, Keymaps sind Nutzerdaten.
- **Icons werden zur Buildzeit nicht generiert:** `src/ui/icons_data.rs` ist
  eingecheckt; `tools/extract_icons.mjs` erzeugt die Datei neu, wenn Icons
  dazukommen (braucht einmalig das npm-Paket `lucide-react`).
- **Store-Mutationen erst am Gestenende:** Drags rendern Vorschauen aus einer
  Kopie; so bleibt die Undo-History sauber (ein Eintrag pro Geste).
- **Frame-Budget:** Die App rendert mit 60 FPS immediate-mode; teure Arbeit
  (Decode, Probe, Waveforms) läuft nie im UI-Thread.

## Bekannte Lücken / nächste Ausbaustufen

- HiDPI-Skalierung (aktuell 1 Logikpixel = 1 Fensterpixel).
- Audio beim Shuttle (Rate ≠ 1) und bei Rückwärtswiedergabe (frameweise Sprünge).
- Sample-genaues Einsetzen neuer Clips im Audio-Mixdown (aktuell
  Block-Granularität, ≤ 85 ms) und Audio-Drift-Korrektur gegen die
  Hardware-Clock über lange Wiedergaben.
- Scopes analysieren bis zum Engine-Ausbau das Thumbnail des Clips am Playhead.
- Effekt-Anwendung (FFmpeg-Filtergraph), Marker, Titel-Rendering.
- Export: Hardware-Encoder (NVENC/VideoToolbox/QSV), Render-Queue mit
  mehreren Jobs, eigene Presets speichern.
