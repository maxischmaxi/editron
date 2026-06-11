# Editron

Ein modernes, modulares Videoschnittprogramm — komplett in **Rust mit raylib
und FFmpeg** gebaut, mit dem Anspruch, mit DaVinci Resolve und Adobe Premiere
Pro mitzuhalten. Kein Webview, kein Electron: das gesamte UI ist ein eigenes
komponentenbasiertes Immediate-Mode-Framework auf raylib, alles läuft in einem
einzigen nativen Prozess.

## Highlights

- **Modulares Panel-System wie in Premiere:** alle Panels (Timeline, Monitore,
  Medien-Browser, Effekte, …) sind andockbare Tabs und frei anordenbar —
  inklusive Tab-Drag&Drop mit Drop-Zonen und Sash-Resize.
- **Workspaces:** Medien · Schnitt · Farbe · Effekte · Audio · Grafik — ein
  Klick wechselt den Kontext, Layout-Änderungen werden pro Workspace gespeichert.
- **Shortcuts für alles:** Jede Funktion ist ein Command und frei belegbar
  (inkl. Mehrfach-Sequenzen wie `Strg+K Strg+S`). Presets für **Adobe Premiere
  Pro** und **DaVinci Resolve** erleichtern den Umstieg.
- **Timeline-Editing:** verknüpfte A/V-Clips, Undo/Redo, Overwrite-Insert,
  Trim/Ripple/Roll/Slip/Slide/Razor, Snapping mit Hilfslinie, Marquee,
  Drag&Drop aus dem Medien-Browser mit Platzierungs-Vorschau.
- **FFmpeg-Engine:** Medien-Analyse (ffprobe), Thumbnails, Waveforms und
  Export/Transcode mit Live-Fortschritt; die Wiedergabe dekodiert Video über
  ffmpeg-Pipes direkt in GPU-Texturen und Audio in raylib-AudioStreams.

## Installation

Fertige Binaries gibt es für **Linux (x86_64)** und **macOS (Apple Silicon)** —
der Installer erkennt die Plattform und wählt automatisch die passende:

```sh
curl -fsSL https://raw.githubusercontent.com/maxischmaxi/editron/main/install.sh | sh
```

Installiert nach `~/.local/bin` (Zielverzeichnis über `EDITRON_INSTALL_DIR`,
bestimmte Version über `EDITRON_VERSION=0.1.0` wählbar). Zur Laufzeit braucht
Editron **FFmpeg/ffprobe** im PATH (`apt install ffmpeg` · `pacman -S ffmpeg` ·
`brew install ffmpeg`). Alle Artefakte samt `SHA256SUMS` liegen auf der
[Releases-Seite](https://github.com/maxischmaxi/editron/releases); andere
Plattformen bauen aus dem Quelltext (siehe unten).

## Entwicklung

Voraussetzungen: Rust (stable), FFmpeg/ffprobe im PATH (alternativ
`EDITRON_FFMPEG_PATH`/`EDITRON_FFPROBE_PATH` setzen). Unter Linux zusätzlich
`cmake` und X11-/Wayland-Dev-Pakete (raylib wird aus dem Quelltext mitgebaut)
sowie `fontconfig` für die Schrift-Auflösung (Inter → Noto Sans, JetBrains Mono).

```bash
cargo run             # App starten (Dev-Profil)
cargo check           # Typen/Borrows prüfen
cargo build --release # Release-Build
```

### Release veröffentlichen

Version in `Cargo.toml` anheben, committen, Versions-Tag pushen — die
[Release-Pipeline](.github/workflows/release.yml) baut die Linux- und
macOS-Binaries und veröffentlicht sie mitsamt `SHA256SUMS` als
GitHub-Release (Windows ist bewusst noch ausgespart):

```bash
git tag v0.1.0 && git push origin v0.1.0
```

## Test- & Debug-Flags

| Variable | Wirkung |
| --- | --- |
| `EDITRON_SHOT=pfad.png` | Screenshot nach N Frames, dann beenden |
| `EDITRON_SHOT_FRAME=300` | Frame-Nummer für den Screenshot (Default 30) |
| `EDITRON_TEST_IMPORT=a.mp4:b.mp3` | Dateien beim Start importieren |
| `EDITRON_TEST_TIMELINE=1` | Importierte Medien ans Sequenzende einfügen |
| `EDITRON_TEST_PLAY=1` | Programm-Wiedergabe automatisch starten |
| `EDITRON_TEST_WORKSPACE=media` | Workspace beim Start wechseln |

Beispiel für einen visuellen Smoke-Test ohne Interaktion:

```bash
EDITRON_SHOT=shot.png EDITRON_SHOT_FRAME=300 \
EDITRON_TEST_IMPORT=clip.mp4 EDITRON_TEST_TIMELINE=1 EDITRON_TEST_PLAY=1 \
./target/debug/editron
```

## Architektur

Siehe [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md). Kurzfassung: Alles ist
ein Command (Registry + when-Klauseln), Panels sind Module mit eigenem
UI-State, Workspaces sind persistierte Dock-Layouts, FFmpeg ist bewusst ein
externes Binary.

## Lizenz

Editron ist freie Software unter der **GPL-3.0-or-later** — siehe
[`LICENSE`](LICENSE). Die eingebetteten Lucide-Icons stehen unter ISC/MIT,
Details in [`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md). FFmpeg wird
nicht mitgeliefert, sondern zur Laufzeit als externes Programm aufgerufen.
