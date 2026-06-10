# Editron

Ein modernes, modulares Videoschnittprogramm — gebaut mit **Rust, Tauri 2 und FFmpeg**, mit dem Anspruch, mit DaVinci Resolve und Adobe Premiere Pro mitzuhalten.

## Highlights

- **Modulares Panel-System wie in Premiere:** alle Panels (Timeline, Monitore, Medien-Browser, Effekte, …) sind andockbare Tabs und frei anordenbar (dockview).
- **Workspaces:** Medien · Schnitt · Farbe · Effekte · Audio · Grafik — ein Klick wechselt den Kontext, Layout-Änderungen werden pro Workspace gespeichert.
- **Shortcuts für alles:** Jede Funktion ist ein Command und frei belegbar (inkl. Mehrfach-Sequenzen wie `Strg+K Strg+S`). Presets für **Adobe Premiere Pro** und **DaVinci Resolve** erleichtern den Umstieg. Funktioniert auf Windows, macOS und Linux.
- **FFmpeg-Engine:** Medien-Analyse (ffprobe), Thumbnails, Waveforms und Export/Transcode mit Live-Fortschritt laufen über FFmpeg im Rust-Backend.

## Entwicklung

Voraussetzungen: Rust (stable), Node 20+, pnpm, FFmpeg/ffprobe im PATH (alternativ `EDITRON_FFMPEG_PATH`/`EDITRON_FFPROBE_PATH` setzen). Unter Linux zusätzlich webkit2gtk 4.1.

```bash
pnpm install
pnpm tauri dev    # App starten
pnpm typecheck    # TypeScript prüfen
```

### Bekannte Stolperfalle: NVIDIA + Wayland

webkit2gtk crasht mit dem proprietären NVIDIA-Treiber unter Wayland (GDK „Error 71"). Editron setzt deshalb auf solchen Systemen automatisch `WEBKIT_DISABLE_DMABUF_RENDERER=1` (siehe `src-tauri/src/main.rs`). Wer das Verhalten selbst steuern will, kann die Variable vor dem Start explizit setzen.

Architektur-Details: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)
