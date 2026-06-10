# Editron — Architektur

Editron ist ein Videoschnittprogramm auf Basis von **Tauri 2 (Rust) + React 19 + TypeScript + Vite + Tailwind CSS v4**, mit **FFmpeg/ffprobe** als Medien-Engine und **dockview** für das andockbare Panel-System.

## Leitprinzipien

1. **Alles ist ein Command.** Jede Aktion (Menü, Button, Shortcut, Palette) läuft über die `CommandRegistry` (`src/core/commands/registry.ts`). Dadurch ist jede Funktion mit einem eigenen Shortcut belegbar.
2. **Kontext statt Fokus-Hacks.** `when`-Klauseln (wie in VS Code) entscheiden, welcher Command in welchem Kontext greift (`src/core/commands/context.ts`). Kontext-Keys: `panel` (fokussiertes Panel), `workspace`, `dialogOpen`, `commandPaletteOpen`, `tool`, `mediaSelected`, `timelineHasClips`, `timelineClipSelected`, `timelineClipboard`, `timelineCanUndo`, `timelineCanRedo`, `timelineInOutSet`.
3. **Panels sind Module.** Jedes Panel registriert sich in der `panelRegistry` (`src/core/workspace/panelRegistry.ts`) und ist damit in jedem Workspace andockbar — wie in Premiere.
4. **Workspaces = Layouts.** Ein Workspace (Medien, Schnitt, Farbe, Effekte, Audio, Grafik) ist ein benanntes dockview-Layout. Nutzeränderungen werden pro Workspace persistiert; „Layout zurücksetzen" stellt das Default wieder her.
5. **UI deutsch, Code englisch.** Nutzersichtbare Strings sind deutsch; Bezeichner, Dateien und Kommentare folgen dem Code-Stil des Repos.

## Verzeichnisstruktur (Frontend)

```
src/
├── main.tsx               # registriert Panels + Builtin-Commands, mountet App
├── App.tsx                # Shell: TitleBar / WorkspaceHost / StatusBar + Overlays
├── styles/
│   ├── globals.css        # Tailwind v4 @theme (surface-/text-/accent-Tokens)
│   └── dockview-theme.css # .dockview-theme-editron (--dv-*-Variablen)
├── core/
│   ├── types.ts           # Medien-/IPC-Typen (Spiegel der Rust-Structs, camelCase)
│   ├── dnd.ts             # In-App-Drag&Drop (Asset-IDs für dragover lesbar)
│   ├── commands/          # Registry, Kontext/when-Evaluator, Builtins
│   ├── keyboard/          # Keybinding-Parser, Resolver, Presets, Manager-Hook
│   ├── playback/          # SequencePlayer: Timeline-Wiedergabe-Engine
│   ├── timeline/          # Sequenz-Modell: Tracks/Clips, Store mit Undo/Redo,
│   │                      # Editier-Operationen, Waveform-Cache
│   └── workspace/         # Panel-Registry, Workspace-Definitionen, Layouts
├── stores/                # zustand: app / media / playback / monitor
├── panels/                # je Panel eine Datei; index.ts → registerAllPanels()
│   └── timeline/          # TimelinePanel + ClipView + TrackHeaderCell
├── components/
│   ├── shell/             # TitleBar, WorkspaceHost (dockview), StatusBar
│   ├── keyboard/          # CommandPalette, ShortcutEditorDialog
│   ├── ui/                # ContextMenu (commandfähig, Shortcut-Anzeige)
│   └── dialogs/           # ExportDialog
└── lib/ipc.ts             # typisierte invoke-Wrapper (Vertrag mit src-tauri)
```

## IPC-Vertrag (Frontend ⇄ Rust)

Rust-Structs serialisieren mit `serde(rename_all = "camelCase")`, damit sie zu `src/core/types.ts` passen. Commands (snake_case):

| Command | Signatur | Zweck |
| --- | --- | --- |
| `ffmpeg_info` | `() -> FfmpegInfo` | Binary-Discovery + Version |
| `probe_media` | `(path) -> MediaInfo` | ffprobe-JSON, strukturiert |
| `generate_thumbnail` | `(path, time_sec, max_width) -> String` | Thumbnail in App-Cache, liefert Pfad |
| `extract_waveform` | `(path, samples) -> Vec<f32>` | Peaks 0..1 |
| `start_transcode` | `(options: TranscodeOptions) -> String` | startet Job, liefert Job-ID |
| `cancel_job` | `(job_id)` | bricht Job ab |
| `reveal_in_file_manager` | `(path)` | zeigt Datei im Dateimanager (Finder/Explorer/xdg) |

Events: `transcode://progress` (`TranscodeProgress`), `transcode://done` (`TranscodeDone`).

Mediendateien werden im Webview über das Asset-Protokoll geladen: `mediaSrc(path)` in `src/lib/ipc.ts` (CSP erlaubt `asset:`-Quellen, `assetProtocol.scope = ["**"]`).

## Shortcut-System

- **Format:** `"Mod+Shift+K"`, Sequenzen `"Mod+K Mod+S"`; `Mod` = Cmd (macOS) / Ctrl (sonst). Typen in `src/core/keyboard/types.ts`.
- **Auflösung:** Resolver mit Sequenz-Puffer; effektive Bindings = aktives Preset + Nutzer-Overrides (Overrides gewinnen). Binding-`when` UND Command-`when` müssen passen.
- **Presets:** `editron` (Default), `premiere`, `resolve` — damit Umsteiger ihre gewohnten Shortcuts behalten. Persistenz über `@tauri-apps/plugin-store`.
- **UI:** Shortcut-Editor (durchsuchbar, Aufnahme-Widget, Konfliktanzeige, Preset-Wahl) + Command Palette.

## Timeline (Sequenz-Editing)

- **Modell** in `src/core/timeline/`: `TimelineTrack` (video/audio, mute/solo/lock) und `TimelineClip` (start/duration/srcIn/srcDuration, `linkId` für Video↔Audio-Paare). Alle Zeiten in Sekunden, Sequenz-Framerate aktuell fix 25 fps (`SEQUENCE_FPS`).
- **Alle Editier-Operationen laufen über den `timelineStore`** und sind damit undo-fähig (Snapshot-History, `edit.undo`/`edit.redo`): Einfügen mit Overwrite-Semantik, Move (inkl. Spurwechsel), Trim/Ripple-Trim/Roll/Slip/Slide, Razor-Split, (Ripple-)Delete, Link/Unlink, Enable/Disable, Spuren anlegen/entfernen, Copy/Paste.
- **Drop aus dem Medien-Browser**: Video- und Audio-Anteil eines Assets landen verknüpft auf Video-/Audiospur; fehlende Spuren werden automatisch angelegt (`planAssetPlacements` liefert dieselbe Planung für Drop-Vorschau und Einfügen). HTML5-DnD: Asset-IDs zusätzlich in `src/core/dnd.ts`, weil `dataTransfer` während `dragover` nicht lesbar ist.
- **Interaktion im Panel** (`src/panels/timeline/`): Snapping (Clipkanten, Playhead, 0) mit Hilfslinie, Marquee-Auswahl, Shift/Mod+Mausrad-Zoom um den Cursor, Alt+Rad horizontal, Mittelmaus-/Hand-Pan, Werkzeuge entscheiden die Drag-Semantik. Clip-Drags nutzen window-Listener statt Pointer-Capture, weil Clips beim Spurwechsel re-mounten.
- **In/Out-Punkte der Sequenz** (`inPoint`/`outPoint` im timelineStore) definieren den Loop-Bereich der Programm-Wiedergabe. Im Lineal: Alt+Ziehen zieht den Bereich auf, Ziehen an den Bereichskanten verschiebt In/Out, Kontextmenü und `I`/`O`/`Mod+Shift+X` setzen bzw. löschen die Punkte.

## Wiedergabe (Monitore)

- **Programmmonitor = Sequenz-Wiedergabe.** Der `SequencePlayer` (`src/core/playback/sequencePlayer.ts`) ist eine UI-unabhängige Engine: Pool unsichtbarer HTMLMedia-Elemente (eines je Clip in Playhead-Nähe, inkl. Preload des nächsten Schnitts), Master-Clock über `performance.now()`, Drift-Korrektur, Loop über die Sequenz-In/Out-Punkte. Sie schreibt den Timeline-Playhead zurück und zeichnet das Bild der obersten aktiven Videospur in das Canvas des Programmmonitor-Panels (mute/solo der Spuren greifen). Ton kommt aus den Audio-Clips (eigene Audio-Elemente), Videoelemente bleiben stumm.
- **Quellmonitor = Einzel-Asset-Player** (`MonitorView`): explizit geladenes Asset (Doppelklick im Medien-Browser, „In Quellmonitor laden“, Doppelklick auf Timeline-Clip → `sourceAssetId` im playbackStore), lokale In/Out-Marken, optionaler Loop.
- **Controller-Routing:** Beide Monitore registrieren ein `PlaybackController`-Interface im `playbackStore` (`source`/`program`). Wiedergabe-Commands (Space, JKL, I/O, Frame-Stepping) wirken über `activePlayback()` auf den aktiven Monitor: Fokus auf dem Quellmonitor-Panel steuert die Quelle, alles andere das Programm. Die Engine ist ab App-Start registriert (`initSequencePlayer()` in `main.tsx`) — Timeline-Wiedergabe funktioniert auch ohne gemountetes Panel.
- **Wiedergabeauflösung** (Voll, 1/2, 1/4, 1/8; `scale` im `monitorStore`): reduzierte Stufen rendern in einen entsprechend kleineren Canvas (CSS skaliert hoch), das Video dekodiert unsichtbar weiter — spart Render-/Filterkosten bei aufwendigem Material.

## Bewusste Entscheidungen

- **`dragDropEnabled: false`** im Tauri-Fenster: natives File-Drop bricht HTML5-Drag&Drop, das dockview fürs Tab-Ziehen benötigt. Medienimport läuft über Datei-Dialog (`Mod+I`). Später ggf. dynamisch umschaltbar.
- **FFmpeg als externes Binary** (System-PATH, später bündelbar als Sidecar) statt libav-Bindings: lizenzfreundlich, robust, Updates unabhängig vom App-Build.
- **Layout-Persistenz in `localStorage`**, Keymaps im Tauri-Store: Layouts sind fensterspezifisch und unkritisch, Keymaps sind Nutzerdaten.
- **`React.StrictMode` ist aktiv:** Effekte laufen im Dev doppelt — dockview-Initialisierung und Event-Listener müssen idempotent sein bzw. sauber aufräumen.
