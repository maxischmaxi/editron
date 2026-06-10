# Editron

Videoschnittprogramm mit dem Anspruch, mit DaVinci Resolve und Adobe Premiere Pro mitzuhalten.
Stack: Tauri 2 (Rust) + React 19 + TypeScript + Vite + Tailwind CSS v4 + dockview + zustand, FFmpeg/ffprobe als Medien-Engine.

## Befehle

- `pnpm dev` — Vite-Devserver (nur Frontend, ohne Tauri-IPC)
- `pnpm tauri dev` — komplette App im Dev-Modus
- `pnpm typecheck` — TypeScript prüfen
- `pnpm build` — Frontend-Build (inkl. Typecheck)
- `cargo check` (in `src-tauri/`) — Rust prüfen

## Architektur

Siehe `docs/ARCHITECTURE.md`. Kurzfassung:

- **Alles ist ein Command** (`src/core/commands/`): jede Aktion läuft über die Registry und ist dadurch frei mit Shortcuts belegbar. when-Klauseln steuern den Kontext.
- **Timeline-Sequenz** (`src/core/timeline/`): Tracks/Clips-Modell mit verknüpften A/V-Paaren, Undo/Redo-History und allen Editier-Operationen im Store; UI in `src/panels/timeline/`. Kontextmenüs über `src/components/ui/ContextMenu.tsx` (commandfähig, zeigt Shortcuts an).
- **Shortcut-System** (`src/core/keyboard/`): `"Mod+K Mod+S"`-Format, Mod = Cmd/Ctrl je OS, Presets `editron`/`premiere`/`resolve`, Nutzer-Overrides persistiert via Tauri-Store.
- **Panels** (`src/panels/`) registrieren sich in der `panelRegistry`; **Workspaces** (Medien/Schnitt/Farbe/Effekte/Audio/Grafik) sind dockview-Layouts mit Persistenz pro Workspace (`src/components/shell/`).
- **FFmpeg-Backend** (`src-tauri/src/ffmpeg/`): externes Binary (PATH bzw. `EDITRON_FFMPEG_PATH`), Commands für Probe/Thumbnail/Waveform/Transcode; IPC-Vertrag gespiegelt in `src/core/types.ts` + `src/lib/ipc.ts` (camelCase via serde).

## Konventionen

- UI-Texte deutsch (korrekte Umlaute), Code/Bezeichner englisch.
- Tailwind-Tokens aus `src/styles/globals.css` verwenden (`surface-0..4`, `line`, `text-1..3`, `accent`, …); keine arbitrary values, wenn die Scale reicht.
- Neue Aktionen IMMER als Command registrieren (nie nackte onClick-Logik für app-weite Aktionen) und im Default-Preset verdrahten.
- `React.StrictMode` ist aktiv: Effekte müssen idempotent sein und sauber aufräumen.
