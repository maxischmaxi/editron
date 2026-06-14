# Editron — Architektur: Transport-Audio-Engine

Dieses Dokument beschreibt die Wiedergabe-Audio-Engine (Programm- und
Quellmonitor) in `src/core/player.rs` und `src/core/playback.rs`. Der
**Export**-Mixdown (`src/core/export.rs`) ist ein eigener, offline laufender
Pfad; beide teilen sich bewusst die DSP-Bausteine (`core/audio_fx.rs`) und die
Tempo-/Fade-Mathematik (`atempo_chain`, `transitions::audio_gain`), damit
**Vorschau und Export gleich klingen**.

## Überblick

- Ein ffmpeg-Prozess je hörbarem Clip dekodiert f32le-Stereo (48 kHz). Der
  Mixdown summiert die Clips sample-genau in einen einzelnen
  raylib-`AudioStream` (`MasterStream`, direkter FFI-Aufruf, weil
  `AudioStream::update` von raylib-rs fälschlich die Byte- statt Frame-Zahl
  übergibt).
- `playback::tick` schiebt Playhead/Quellposition pro Frame per Wall-Clock vor
  (inkl. Loop-/Grenzlogik). Die Audio-Engine korrigiert das danach gegen die
  **gerätegetaktete** Audio-Uhr (Drift-Korrektur, siehe unten).

## Sample-genaue Platzierung (Render-Kante + Output-Frame-Achse)

Der Mixdown rechnet auf einer **globalen Output-Frame-Achse** (`master_out` =
geschriebene Frames seit Streamstart). Jede Wiedergabequelle (Programm,
Quelle) hat eine `TargetClock`, die Sequenz-/Quellzeit linear auf diese Achse
abbildet:

```
out_of(pos) = anchor_out + (pos - anchor_pos) * RATE / rate     (rate signiert)
```

Ein Clip wird dadurch nicht block-, sondern **sample-genau** platziert: sein
erstes Sample sitzt am exakten Output-Frame `oa = round(out_of(enter))`, das
letzte vor `ob`. `ClipAudio::mix_block` mischt den Clip in den Sub-Block
`[block_out, block_out + AUDIO_CHUNK_FRAMES)` an genau diesem Offset. Damit ist
der frühere ≤85-ms-Versatz beim Einsetzen neuer Clips beseitigt
(`AUDIO_CHUNK_FRAMES = 4096` ist nur noch Block-/Sub-Buffer-Größe, nicht mehr
die Einsetz-Granularität).

- **Anti-Klick-Rampe:** harte Schnittkanten (Clip-Start/-Ende, Seek-Einstieg)
  erhalten eine lineare ~5-ms-Rampe (`CLICK_RAMP_FRAMES = 240`, `edge_ramp`).
- **Hüllkurve:** Lautstärke-Keyframes (Medienzeit) und Übergangs-Crossfades
  (Sequenzzeit) werden je `ENV_BLOCK_FRAMES = 256` (~5 ms) ausgewertet — wie im
  Export.
- **Spur-Bus:** Clips einer Spur werden in einen Per-Spur-Buffer gemischt, dann
  Bus-FX → Spur-Gain/Pan (über den Block gerampt) → Master. Die Spur-Effekte
  wirken auf die SUMME der Spur, exakt wie im Export.

## Drift-Korrektur (Bild folgt Ton)

`master_out` wächst nur, wenn raylib einen Sub-Buffer freigibt
(`is_processed`), läuft also **mit der Hardware-Audio-Clock**. Die gehörte
Position ist `heard_pos = render_pos − NOMINAL_LATENCY` (eine konstante,
gerätenahe Latenz; `NOMINAL_LATENCY_FRAMES = 2 · Block`, gemessen wird direkt
nach dem Schreiben, wenn der raylib-Doppelpuffer am vollsten ist).

Der Playhead wird per **Delay-Locked-Loop** sanft an `heard_pos` gezogen
(`slew_step`: Proportionalterm `SLEW_GAIN`, pro Tick auf `MAX_SLEW_PER_TICK`
begrenzt). So bleibt die Wall-Clock-Bewegung flüssig, aber ohne akkumulierende
Drift: Eine 30-min-Wiedergabe folgt der Geräte-Uhr (Unit-Test
`slew_locks_playhead_to_device_clock_over_30min`). Seeks/Loop-Sprünge
(> `AUDIO_RESYNC_TOLERANCE`) verankern die Uhr neu und verwerfen die Decoder.

## Shuttle (JKL, Rate ≠ 1)

Bei Shuttle-Raten wird `|rate|` in die atempo-Kette des Decoders eingerechnet
(`tempo = clip.eff_speed() × |rate|`), Audio bleibt also pitch-korrigiert. Die
Render-Kante läuft mit `rate` über die Output-Achse. Ab `|rate| >
MAX_SHUTTLE_AUDIO_RATE` (4×) ist der Ton bewusst stumm (Bild shuttlet weiter).

## Rückwärtswiedergabe (Ton)

`AudioRev` dekodiert Medien-Chunks **vorwärts** (ab fallender Obergrenze),
kehrt die Samples um (`reverse_stereo`) und liefert sie absteigend aus —
segmentiertes Vorwärts-Decoding mit Umkehr-Puffer, doppelt gepuffert (kein
Aussetzer an Chunk-Grenzen, Scrubbing-Charakter wie in Profi-NLEs). Greift bei
Rückwärts-Transport (`program_rate < 0`); die Netto-Medienrichtung
(`media_step.signum() × rate.signum()`) wählt pro Clip Vorwärts- oder
Reverse-Quelle. **Parität:** Bei Vorwärts-Wiedergabe sind als `reverse`
markierte Clips stumm (der Export rendert sie ebenfalls ohne Ton).

## Audio-Scrubbing

Beim Ziehen des Playheads (Lineal/Programm-Scrubber, `scrub_active`) spielt die
Engine kurze Vorwärts-Grains (`SCRUB_GRAIN_SEC`) aus dem obersten hörbaren
Audio-Clip am Playhead (Anti-Klick-gefenstert). Abschaltbar über den Command
`playback.toggleAudioScrub` (`audio_scrub_enabled`).

## Verifikation

- Unit-Tests in `src/core/player.rs` (`cargo test player::`): sample-genaue
  Platzierung an krummer Position, Anti-Klick-Rampe, Uhr-Mapping vorwärts/
  rückwärts, Slew-Verriegelung über 30 min.
- Export-Parität: bestehende End-to-End-Tests in `src/core/export.rs`.
- Manuell: `EDITRON_AUDIO_DEBUG=1` loggt je Sekunde Blöcke/Ticks/verhungerte
  Sub-Buffer/Stimmen/Rate/Slew/Master-Peak. `EDITRON_TEST_PLAY` +
  `EDITRON_TEST_RATE=2|-1|0.5` startet Wiedergabe mit Shuttle-/Reverse-Rate.

## Status der früher dokumentierten Lücken

| Lücke | Status |
| --- | --- |
| Kein Ton bei Shuttle (Rate ≠ 1) | behoben (atempo, ≤ 4× hörbar) |
| Kein Ton bei Rückwärtswiedergabe | behoben (`AudioRev`) |
| Block- statt sample-genaues Einsetzen (≤ 85 ms) | behoben (Output-Frame-Achse) |
| Keine Drift-Korrektur gegen die Hardware-Clock | behoben (DLL-Slewing) |
| Kein Audio-Scrubbing | ergänzt (Grains am Playhead) |

---

# Editron — Architektur: Wiedergabe-Performance & Render-Cache

Ergänzt den obigen Audio-Pfad um die **Video-Wiedergabe-Performance** in
`src/core/player.rs`, `src/core/frame_cache.rs`, `src/core/render_cache.rs`
und `src/core/settings.rs`. Ziel: „klebrig-direktes“ Scrubbing und stabile
Wiedergabe mehrlagiger Sequenzen — vergleichbar mit DaVinci Resolve / Premiere.

## Ausgangslage (vorher)

Pro sichtbarem Video-Layer lief EIN ffmpeg-Decoder (rawvideo/rgba über Pipe).
Jeder Seek/Rücksprung **killte den Prozess und startete ihn neu** (`-ss`),
dekodierte Frames wurden nach dem Texture-Upload **verworfen** (kein Cache).
Bei Überlast wurden Frames still gedroppt (kein Zähler). Kein Hardware-Decode,
kein Sequenz-Prerender.

## 1. Frame-Cache (`core/frame_cache.rs`)

RAM-begrenzter LRU-Cache dekodierter RGBA-Frames, Schlüssel
`FrameKey { path, w, h, frame, fps_milli }` (Pixel sind durch Decode-Pfad,
-maße und Medien-Frame eindeutig → speed-/proxy-unabhängig). Eviction in
O(log n) über eine nach Zugriffssequenz sortierte `BTreeMap`. Budget aus
`PerfSettings.frame_cache_budget_mb` (Standard 2 GB), live umschaltbar.

- **Scrubben/Pausiert:** `drive_video` trifft zuerst den Cache und lädt den
  Frame **ohne Decode** hoch (auf einem gecachten Standbild läuft kein ffmpeg).
- **Wiedergabe:** jeder dekodierte Frame (auch die zum Aufholen übersprungenen)
  wandert in den Cache — Zurückscrubben über eben Abgespieltes trifft sofort.
- **Read-Ahead beim Pausieren** (`Prefetch`): nach Stillstand des Playheads
  wird ein Fenster `[Playhead−R, Playhead+R]` (`PREFETCH_RADIUS`) im Hintergrund
  dekodiert und in den Cache gelegt → Frame-Stepping (←/→) reagiert sofort.

## 2. Smarteres Seeking

Die Reuse-vs-Restart-Entscheidung ist als reine Funktion
`frame_cache::seek_decision` herausgelöst (unit-getestet): kleine
Vorwärtssprünge **lesen weiter** statt neu aufzusetzen; Rücksprünge und große
Sprünge starten neu (gedrosselt über `SCRUB_RESTART_INTERVAL`). `-ss` steht vor
`-i` (schnelles Input-Seek). `ScrubCoalescer` koalesziert eine Folge von
Scrub-Anfragen auf die zuletzt angefragte Position (Debounce des Read-Ahead).

## 3. Hardware-Decode (optional, mit Fallback)

`available_hwaccels()` erkennt einmalig per `ffmpeg -hwaccels`
(videotoolbox/cuda/nvdec/vaapi/qsv/d3d11va/dxva2). Bei aktivem Hardware-Decode
wird `-hwaccel <methode>` **vor `-i`** ergänzt (ohne `-hwaccel_output_format`,
damit die `scale,format=rgba`-Kette unverändert greift). Scheitert ein
HW-Prozess ohne ein einziges Frame, wird der Pfad in `hw_failed` vermerkt und in
**Software** neu gestartet. Schalter: Command `hwaccel.toggle` oder Env
`EDITRON_HWACCEL=off|auto|vaapi|cuda|videotoolbox|…`.

## 4. Sequenz-Render-Cache (`core/render_cache.rs`, „Render In to Out“)

Bereiche der Sequenz werden im Hintergrund über den **geteilten Compositing-
Kern** des Exports gerendert (`export::render_segments` — aus `render_video`
herausgelöst, sodass Voll-Export UND Cache exakt denselben Pfad nutzen) und in
eine **Intra-Frame-Cache-Datei** (ProRes Proxy / DNxHR LB / H.264-Keyframes,
`RenderCacheCodec`) geschrieben. Ablauf:

- Command `render.inToOut` (`Mod+Shift+R`): baut auf dem Main-Thread einen
  Renderplan für `[in, out]` (`export::build_cache_plan`), berechnet die
  Inhalts-Signatur und startet `Services::start_render_cache`
  (Hintergrund-Thread, abbrechbar wie ein Export).
- **Wiedergabe bevorzugt den Cache:** deckt ein gültiges Segment den Playhead
  ab und es wird abgespielt, liefert EIN Cache-Decoder das Programmbild
  (`render_cache_target` → `player://rendercache`), statt N Layer zu
  komponieren. Beim Pausieren bleibt Live-Compositing aktiv (Bearbeiten/Gizmo
  funktionieren, Scrubbing kommt aus dem Frame-Cache).
- **Inhaltsbasierte Invalidierung:** jedes Segment merkt sich eine Signatur
  (`range_signature`) der visuell relevanten Zustände seines Frame-Bereichs.
  Ein Edit erhöht global `TimelineStore::revision` (billiger Trigger für
  `refresh`); ob ein Segment *wirklich* veraltet ist, entscheidet der
  Signaturvergleich — ein Schnitt am Sequenzende lässt gecachte Bereiche am
  Anfang gültig.
- **Render-Leiste** im Timeline-Lineal (`draw_ruler`): rot = vorrender-relevant
  (`complex_spans`: überlappende Layer, Effekte/Grade/Transform/Speed/Titel,
  Übergänge) aber nicht gecacht; grün = gültig gecacht; gelb = wird gerendert.

## 5. Messbarkeit

`MonitorStore.perf` (`PerfStats`) wird vom Mainloop/Player gefüllt: Decode-/
Upload-/Frame-Zeiten (EMA-geglättet), FPS, verworfene Frames (Zähler +
2-s-Ringfenster), Frame-Cache-Trefferquote/Belegung. Im Programmmonitor:

- **Dropped-Frame-Indikator** (Resolve-artig, rot, oben rechts) bei kürzlich
  verworfenen Frames; roter Punkt am Overlay-Button.
- **Performance-Overlay** (Command `monitor.togglePerfOverlay`,
  `Mod+Alt+Shift+P`, Button im Programmmonitor): Decode-/Upload-/Frame-ms, FPS,
  Frame-Cache-MB/Hitrate, verworfene Frames gesamt, Render-Cache-Segmente.

## Verifikation

- `cargo test`: **Cache-Invalidierung** (`render_cache::tests` — Edit
  invalidiert nur den überlappenden Bereich; `refresh` markiert das richtige
  Segment), **Seek-Koaleszenz** (`frame_cache::tests::scrub_coalescer_*`),
  Reuse-vs-Restart (`frame_cache::tests::seek_*`), LRU-Eviction/Budget,
  `complex_spans`. Die Export-End-to-End-Tests laufen über den nun geteilten
  `render_segments`-Pfad und sichern die Cache-/Export-Parität ab.
- Manuell: `EDITRON_TEST_PERF=1` blendet das Overlay ein. Scrub-Latenz vorher
  (jeder Tick ein ffmpeg-Restart, spürbare Verzögerung) vs. nachher (gecachte
  Region: Upload ohne Decode, kein Restart) ist im Overlay an Decode-ms ≈ 0 und
  steigender Cache-Hitrate ablesbar.

## Status der früheren Lücken

| Lücke | Status |
| --- | --- |
| Kein Frame-Cache (Frames nach Upload verworfen) | behoben (LRU `frame_cache`) |
| Seek = ffmpeg-Neustart, spürbare Latenz | behoben (`seek_decision`, Reuse) |
| Kein Read-Ahead fürs Frame-Stepping | behoben (`Prefetch` beim Pausieren) |
| Kein Hardware-Decode | ergänzt (`-hwaccel`, SW-Fallback) |
| Kein Sequenz-Prerender | ergänzt (`render.inToOut` + Render-Leiste) |
| Kein Dropped-Frame-Indikator/Perf-Overlay | ergänzt (`PerfStats`, Monitor) |

---

# Editron — Architektur: Render-System (Hintergrund-Export + Warteschlange)

Der Sequenz-Export ist von einem einzelnen, modalen Job zu einem
Hintergrund-Render-System auf Media-Encoder-Niveau ausgebaut (`core/export.rs`,
`core/render_queue.rs`, `core/export_preset.rs`, `overlays/export_dialog.rs`).

## Entkopplung (heilig: spätere Edits ändern laufende Jobs nie)

Jeder Job hält einen vollständigen Snapshot: der `RenderPlan` wird beim
Einreihen aus Timeline/Medien gebaut und trägt **owned** Dateipfade
(`VideoLayerPlan::path`, `AudioClipPlan::path`). Der Worker erhält Plan +
Settings by-value. Spätere Edits oder ein Medien-Relink (ändert
`asset.path` im Store) berühren den laufenden Export also nicht — der Plan ist
seine eigene Wahrheit. Abgesichert durch `export_always_uses_original_never_proxy`
(Plan referenziert immer das Original, nie den Proxy).

## Warteschlange (`core/render_queue.rs`)

Zustandsmaschine je Job: `Waiting → Running → {Done | Failed | Cancelled}`.
Der „Pump" (`RenderQueue::next_to_start`, in `main.rs::pump_render_queue` je
Frame) startet genau einen wartenden Job, sobald keiner mehr läuft
(sequentiell). Fortschritt/Abschluss-Events werden über die `services`-Job-Id
dem richtigen Job zugeordnet. Reorder (`move_up/down`), `cancel` (wartend =
sofort, laufend = Worker killen), `restart` (gleicher Snapshot), `remove`,
`clear_finished`, `paused`. Beim App-Beenden mit aktiven Jobs warnt
`DialogId::ConfirmQuitRender`; `quit_requested` bestätigt (raylib
`WindowShouldClose()` setzt sein Flag jeden Aufruf zurück ⇒ abfangbar).

## Dialog-Tabs + Statusleiste

Der Export-Dialog ist nicht mehr blockierend: „Exportieren"/„Hinzufügen" legen
einen Job an und der Dialog ist frei schließbar (Render läuft weiter). Tab
„Warteschlange" zeigt alle Jobs mit Status/Fortschritt/Phase/Aktionen. Die
Statusleiste trägt einen klickbaren Export-Chip (Prozent + ETA + Anzahl
wartend → `queue.open`).

## Eigene Presets (`core/export_preset.rs`)

`PresetData` = serialisierbare Form der `ExportSettings` (Codec-/Encoder-Ids
statt `'static`-Referenzen). Speichern/Überschreiben/Löschen als JSON unter
`~/.config/editron/export_presets.json`; erscheinen neben den eingebauten
Presets. Unbekannte Ids fallen beim Laden auf Katalog-Standards zurück.

## Hardware-Encoder (`EncoderDef`/`EncoderQuality`)

Jede Codec-Familie (H.264/HEVC) hat mehrere Encoder-Backends (`[0]` = Software):
NVENC (`-cq`), Intel QSV (`-global_quality`), VAAPI (`-vaapi_device` +
`format=nv12,hwupload` + `-qp`), VideoToolbox (nur Bitrate). `video_codec_args`
wählt das passende Qualitäts-Flag je Backend. Nicht von `ffmpeg -encoders`
gelistete Hardware-Encoder werden ausgeblendet (`available_video_encoders`) und
validiert; ein Fehler zeigt die ffmpeg-stderr + Software-Fallback-Hinweis.

## Bild-Sequenzen + Einzel-Frame

PNG/JPEG/TIFF-Container (`image_sequence: true`, nur Video-Phase). Der Worker
(`render_image_sequence`) schreibt erst in ein Temp-Unterverzeichnis und
verschiebt die fertigen Frames atomar (`<stamm>_%06d.<ext>`, Startnummer
wählbar) — Abbruch/Fehler hinterlässt keine halbe Sequenz. Der Programmmonitor
hat ein Kamera-Icon („Frame exportieren" → `export.frame`): 1-Frame-Plan am
Playhead → `services::export_frame` → Bilddatei (`frame_export_args`).

## Verifikation

- Unit: `render_queue::tests::*` (Zustandsmaschine, Pump, Reorder, Cancel),
  `export_preset::tests::*` (Roundtrip, Fallback), `export::tests::*`
  (Encoder-Katalog/-Verfügbarkeit, `video_codec_args`-Flags je Backend,
  Bild-Sequenz-Muster/-Container).
- End-to-End: `end_to_end_export_renders_png_sequence` (volle Worker-Pipeline);
  App-E2E via `EDITRON_TEST_EXPORT` (Job läuft im Queue-Tab im Hintergrund).

# Editron — Architektur: Mehrere Sequenzen + verschachtelte Sequenzen (Nesting)

## Projektmodell (`core/sequences.rs`)

`AppState.timeline` ist jetzt ein `SequenceStore` (vorher genau eine
`TimelineStore`). Der Store hält `Vec<Sequence>` (je `id`/`name`/`bin_id` +
eigene `TimelineStore` mit eigener Undo-History, Sequenz-Einstellungen, Playhead,
Zoom), einen aktiven Index und die offenen Tabs. **Der Clou: `Deref`/`DerefMut`
auf die aktive Sequenz** — aller Bestandscode (`state.timeline.clips`,
`state.timeline.set_playhead(..)`) wirkt unverändert auf die aktive Sequenz, und
`&state.timeline` coerct an `&TimelineStore`-Parameter weiter (Player, Export,
Compose blieben fast unangetastet). Geändert wurden nur Konstruktion
(`state.rs`, `project.rs`) und die Stellen, die ALLE Sequenzen brauchen:
Persistenz und das Dirty-Tracking (`aggregate_revision()` summiert die
Revisionen aller Sequenzen).

Undo-Isolation ist gratis: jede Sequenz hat ihre eigene `past`/`future`.

## Persistenz (`.etron` Format v11)

`ProjectFile.sequences: Vec<SequenceDoc>` (je `id`/`name`/`binId`/`timeline`)
+ `activeSequenceId`. Das alte Einzel-`timeline`-Feld wird beim Speichern
weggelassen (`TimelineDoc::is_empty`-skip) und nur zum Laden von v≤10-Dateien
gelesen (= genau eine Sequenz „Sequenz 01“). `load_timeline_doc` faktorisiert
die Per-Sequenz-Ladelogik. Verwaiste Nest-Verweise (auf gelöschte Sequenzen)
werden beim Laden entfernt.

## Nesting-Modell

Ein **Nest-Clip** ist ein `TimelineClip` mit `nest_seq: Some(seq_id)` (und
leerem `asset_id`). Er wird wie ein Medien-Clip getrimmt/verschoben/komponiert
(`is_nest()`, von der Orphan-Bereinigung ausgenommen, NICHT `is_generator()`).
`insert_nest_clips` legt je Nest einen Video- und — falls die innere Sequenz
Audio enthält — einen verknüpften Audio-Clip an (Dauer = innere Sequenzlänge).

**Rekursionsschutz** (`SequenceStore::would_create_cycle`): Das Einfügen von
`nested` in `host` ist verboten, wenn `nested == host` oder `host` aus `nested`
über Nest-Kanten (transitiv) erreichbar ist (DFS über alle Sequenzen). Damit
kann sich keine Sequenz selbst enthalten.

## Geteilte Auflösung (`core/compose.rs`)

`composite_sequence_frame(timeline, resolver, t, w, h, …, fetch_leaf, depth)`
ist DIE rekursive Auflösung, die **Player UND Export** teilen ⇒ Vorschau und
Export sind pixelgleich. Sie komponiert die sichtbaren Programm-Ebenen einer
Sequenz auf opakes Schwarz; eine Nest-Ebene wird rekursiv aus dem `resolver`
(`NestResolver`-Trait, von `SequenceStore` implementiert) an der inneren
Sequenzzeit `clip.media_time_at(t)` aufgelöst; Blatt-Clips liefert die
`fetch_leaf`-Closure (Player: Frame-Cache/ffmpeg; Export: Einzelbild-ffmpeg).
Nests werden in **innerer** Auflösung komponiert und contain-fit ins äußere
Frame gelegt (inneres Seitenverhältnis bleibt erhalten). Tiefe ist auf
`MAX_NEST_DEPTH=16` begrenzt (das Modell ist azyklisch).

Framerate-Anpassung ist automatisch: das Modell rechnet in Sekunden, die
äußere↔innere Abbildung ist `media_time_at` (speed-/rückwärts-/standbild-bewusst).

## Export (`core/export.rs`)

`build_render_plan`/`validate` nehmen `&dyn NestResolver` (Dialog/Worker =
`&state.timeline`, sonst `NoNests`). Der Nest-Kandidat folgt dem Titel-Muster
(kein Decoder, natural = innere Auflösung). `RenderPlan.nests`/`nest_media`
sammeln (transitiv, `gather_nests`) alles, was der Worker self-contained braucht.
`SegLayer::Nest`/`NestLayer.advance` komponiert je Frame über
`composite_sequence_frame`; Effekte/Grade des Nest-Clips werden danach
eingebrannt. Audio: `flatten_nest_audio` flacht inneres Audio rekursiv
(zeitverschoben, Master/Spur/Clip-Gains gefaltet) in `plan.audio` ein.

## Player-Vorschau (`core/player.rs`, `panels/monitor.rs`)

`render_nest_previews` komponiert sichtbare Nest-Clips über denselben Kern und
lädt sie als `player://clip/<id>` hoch (`nest_sig`-Cache je innere
Sequenz/Frame/Größe ⇒ stehender Playhead rendert nicht jedes Tick neu). Der
Monitor hat einen Nest-Zweig in `resolve_program_layers` (Textur-Key + innere
Auflösung als natural; Grade/Effekte über die bestehende GPU-Pipeline =
formelgleich zum CPU-Export).

## UI

Sequenz-Tabs über der Timeline (`render_sequence_tabs`): Klick wechselt,
Mittelklick/× schließt den Tab (Sequenz bleibt), Doppelklick = Inline-Rename,
Drag sortiert, „+“ = neue Sequenz, Rechtsklick-Menü. Doppelklick auf einen
Nest-Clip öffnet die innere Sequenz. Der Medien-Browser zeigt Sequenzen als
eigene Einträge (clapperboard-Icon, Doppelklick öffnet, `DragPayload::Sequences`
= Nest-Drag in eine andere Timeline). Commands `sequence.new` (Mod+Shift+N),
`duplicate`/`rename`/`delete` (letzte Sequenz geschützt; bei Nest-Nutzung
`DialogId::ConfirmDeleteSequence`), `open`/`openNested`.

## Verifikation

- Unit: `sequences::tests::*` (Default/Add/Close/Remove/Rekursionsschutz
  direkt+transitiv/Undo-Isolation), `project::tests::*` (Mehr-Sequenz-Roundtrip,
  Nest-Persistenz, Lösch-Aufräumen), `compose::tests::*`
  (einfache + zweifache Verschachtelung, Frame-Zuordnung),
  `export::tests::nested_sequence_resolves_in_render_plan`/`…_audio_flattens…`.
- End-to-End: `end_to_end_export_renders_nested_sequence` (grünes inneres Video
  → Nest → Ausgabe-Mittelpixel grün). Tab-Leiste per Screenshot verifiziert.

## Bewusste v1-Grenzen

- Player-Vorschau-AUDIO von Nests ist stumm (Export hat es via `flatten_nest_audio`).
- Innere Spur-Bus-FX/-Automation und innere Crossfades fließen NICHT ins
  geflachte Nest-Audio; Nest-Audio bypasst die äußere Spur-Bus-FX; angenommen
  wird Nest-Geschwindigkeit 1.
- Der Render-Cache cacht Nest-Segmente nicht (fällt auf Live-Compositing zurück).

# Editron — Architektur: Timeline-Austauschformate (Interop)

OpenTimelineIO, CMX-3600-EDL und Final Cut Pro XML — damit Profi-Produzenten
Schnitte an DaVinci Resolve (Grading) & Co. übergeben und von dort empfangen.
Interop ist **binär**: entweder der Schnitt kommt frame-genau drüben an oder das
Feature ist wertlos. Quelle: `core/interop/` (`mod.rs`, `otio.rs`, `edl.rs`,
`fcpxml.rs`, Tests in `roundtrip.rs`).

## Frame-Genauigkeit (das eigentliche Problem)

Alle Timeline-/Quell-Zeiten laufen über die **rationale Sequenzrate**
(`sequence::FrameRate`, z. B. 24000/1001) und werden als ganzzahlige Frame-Zahlen
geführt — driftfrei auch an krummen NTSC-Raten. Da Editrons Clips bereits auf dem
Frame-Raster der Sequenz liegen (`src_in`/`start`/`duration` über `frame_round`),
ist der Roundtrip Editron→Format→Editron **verlustfrei**. Beim Lesen rechnet jede
`RationalTime` über ihre EIGENE (per `FrameRate::from_fps` rekonstruierte) Rate in
Sekunden und dann frame-genau auf die Sequenzrate — gleiche Rate ⇒ identische
Frame-Zahl, ungleiche Rate ⇒ realzeit-erhaltend gerundet (das Beste ohne Resampling).

## Eine gemeinsame Zwischendarstellung (IR)

`InteropTimeline` (Spuren V1..Vn/A1..An aus `InteropItem::{Gap,Clip,Transition}`,
dedup-Mediendatei-Tabelle, Marker) ist format-neutral. `build_export` baut sie aus
der aktiven Sequenz, jeder Serializer verbraucht dieselbe IR — die Frame-Mathematik
existiert **genau einmal**. `build_import` dreht das um: ein Parser liefert die IR,
daraus entstehen Spuren/Clips/Assets. So teilen sich OTIO/EDL/FCPXML die Logik.

## Keine stillen Datenverluste

Was ein Format nicht abbilden kann, wird übersprungen UND als Warnung gesammelt,
die der Nutzer im Ergebnis-Dialog (`overlays/interop_report.rs`) sieht — nie
kommentarlos. Beispiele: Titel/Untertitel/Nests/Geschwindigkeit werden beim Export
als **gleichlange Lücke** geschrieben (Timing bleibt frame-genau) + Hinweis; EDL
exportiert nur eine Video-Spur + meldet ausgelassene; fremde Effekte beim Import
werden ignoriert + gemeldet.

## Formate im Detail

- **OTIO 0.17** (`otio.rs`, primär, Import+Export): JSON über `serde_json` (kein
  C++-Binding). `Timeline.1`/`Stack.1`/`Track.1`/`Clip.1`/`Gap.1`/`Transition.1`/
  `ExternalReference.1`/`Marker.2`. Übergänge als `SMPTE_Dissolve` mit
  `in_offset`/`out_offset`. Auflösung/Drop-Frame liegen in `metadata.Editron`
  (Fremdtools ignorieren unbekannte Metadaten) ⇒ Editron-Roundtrip voll verlustfrei.
- **CMX-3600-EDL** (`edl.rs`, Import+Export): genau eine Video-Spur, V+A-Events,
  `FROM CLIP NAME`, Drop-/Non-Drop über `core/timecode`. Out-Punkte exklusiv.
  Dissolves als Standard-Zwei-Ereignis-Form (`D nnn`). EDL trägt KEINE Bildrate —
  beim Import 29,97 (DF) bzw. 25 (NDF) angenommen + gemeldet.
- **FCPXML 1.11** (`fcpxml.rs`, Export): handgeschriebenes XML mit Escaping.
  `resources`(format+asset+media-rep), `library→event→project→sequence→spine`.
  V1 = primäre Storyline (Clips+Gaps), höhere Spuren/Audio = verbundene Clips
  (`lane` ≠ 0). Zeiten als gekürzte Rationalzahlen „N/Ds" (ein Frame = den/num s).

## UI

Datei-Menü → „Sequenz importieren/exportieren" (Untermenüs), Commands
`sequence.{import,export}.{otio,edl,fcpxml}` (Export mit `when: timelineHasClips`),
rfd-Dateidialoge in `services.rs`. Der Import legt eine neue Sequenz an, fehlende
Medien als Offline-Assets, online auffindbare werden über den vorhandenen
Relink-Pfad (`resolve_relink`) per ffprobe verifiziert; der Ergebnis-Dialog bietet
bei fehlenden Medien direkt „Medien verknüpfen…" (Relink-Wizard).

## Verifikation

- Unit/Roundtrip: `interop::roundtrip::*` — OTIO frame-lossless @ 23,976
  (Start/Dauer/Quell-In/Marker/Auflösung), OTIO-Dissolve überlebt, EDL-Cut-Roundtrip
  @ 25, EDL Drop-Frame, FCPXML wohlgeformt (mit `xmllint`, falls vorhanden) +
  rationale Zeiten + auflösende `ref`s, Auslassungs-Meldungen (Nest→Lücke, EDL
  höhere Spuren). Fixtures `tests/fixtures/{resolve_basic.otio,premiere_basic.edl,
  resolve_dissolve.edl}` parsen frame-genau.
- End-to-End: `EDITRON_TEST_INTEROP="otio:/tmp/out.otio"` exportiert die
  Test-Sequenz durch den echten App-Pfad.

## Bewusste v1-Grenzen

- FCPXML ist Export-only (Import folgt). Geschwindigkeit/Reverse/Freeze werden beim
  Export nicht als Retiming übertragen (Clip läuft normal) + Hinweis.
- EDL-Dissolves sind nur cut-genau lossless; zentrierte Dissolves verschieben beim
  Roundtrip um die halbe Dauer (CMX-Eigenheit). OTIO ist der verlustfreie Pfad.
- Effekte/Farbkorrektur fremder Tools werden ignoriert (Schnitt bleibt erhalten).

---

# Editron — Architektur: High-Bit-Depth-Pipeline & Float-Compositing (>8 Bit)

Ziel: 10-Bit-Material (Log-Footage, HDR-Lieferungen) ohne Qualitätsverlust
verarbeiten — kein Banding nach einem Grade, messbare Parität zwischen Vorschau
und Export, ehrliche Farbraum-Tags. Resolve-Liga.

## Grundsatzentscheidung: f32 in Gamma, nicht linear

Die gesamte CPU-Pipeline rechnet jetzt in **f32-RGBA, display-referred (gamma-
codiert, 0..1)** — dieselbe Semantik wie das alte `u8/255`, nur in voller
Präzision. Bewusst NICHT linear-light:

- **Compositing/Alpha-Blending bleibt im Gamma-Raum.** Linear-Light-Blending
  würde die Optik aller Bestands-Übergänge/Titel-Kanten ändern und verlangt
  HDR-Grading (bewusst ausgeklammert). Der Gewinn (kein Banding) kommt aus der
  Präzision, nicht aus dem Farbraum.
- Die Farbkorrektur (`core/grade.rs`) konvertiert intern weiter für Weißabgleich/
  Belichtung nach linear und zurück (unverändert) — und ist die EINZIGE
  Referenz: `grade_buffer` ruft pro Pixel `grade_pixel` (der frühere u8-LUT-Klon
  ist weg ⇒ CPU-Export und GPU-Shader rechnen garantiert formelgleich).

`core/pixbuf.rs` ist das Fundament: f32-Frame + Konvertierungen
(`rgba8↔f32`, `rgba64le↔f32`), Quantisierung mit **TPDF-Dithering** (f32→u8,
bricht Restbanding flimmerfrei pro Pixel) bzw. Runden (f32→u16), und
`pix_fmt_bit_depth()`.

## Stufe 1 — Decode in höherer Bittiefe (`core/player.rs`, `core/export.rs`)

- ffprobe liefert jetzt `bit_depth` (aus `bits_per_raw_sample`, sonst aus
  `pix_fmt`) + `color_transfer`/`color_primaries`/`color_space`/`color_range`
  (`VideoStreamInfo`, `services::probe_media`).
- Export-Compositing-Layer dekodieren **rgba64le (16 Bit)** wenn die Quelle
  >8 Bit hat, sonst **rgba (8 Bit)** — 8-Bit-Material zahlt keine Bandbreite
  drauf (`VideoLayerPlan.src_bit_depth`, geerbt aus dem Asset). Der Schnellpfad
  (ein Layer, keine Korrektur) gibt direkt das Pipe-Format aus und reicht damit
  10-Bit-Quellen verlustfrei durch.
- v1-Grenze: Nest-Blätter (`leaf_frame`) und die **Player-Vorschau** dekodieren
  noch 8-Bit (die Vorschau-Texturen sind R8G8B8A8; 16-Bit-Vorschau kommt mit
  dem GPU-16F-Compositing).

## Stufe 2 — Export-Compositing in Float + 10-Bit-Encode (`core/export.rs`)

- `compose::composite_frame`, `grade::grade_buffer`, `effects::apply_effects_buffer`
  arbeiten auf f32-Puffern. Jeder Layer: Roh-Decode → f32 → Effekte → Grade →
  Src-over-Compositing — alles in f32, **quantisiert wird erst in der Encoder-
  Pipe**.
- **Pipe-Format** (`pipe_pix_fmt`/`pipe_hi_bit`/`pipe_bytes_per_px`): leitet sich
  aus der Ziel-Bittiefe (`resolved_output_pix_fmt`) ab. >8 Bit ⇒ `rgba64le`
  (verlustarm, der Encoder dithert 16→10), sonst `rgba` mit TPDF-Dither.
- **10-Bit-Profile**: ProRes/DNxHR über die Profil-Auswahl (waren es schon);
  HEVC `main10`, H.264 `high10`, VP9 Profil 2, AV1 10-Bit über den Schalter
  `VideoSettings.tenbit` (`codec_tenbit_pix_fmt`, Dialog-Checkbox „10-Bit-Ausgabe",
  in Presets persistiert). VAAPI bleibt 8-Bit (Hardware-10-Bit encoderspezifisch).

## Verifikation

- `core::pixbuf::*` — Roundtrips, Dither bricht Banding (mehr Codes, keine Lücke
  >2 LSB), Bittiefe-Erkennung.
- `core::grade::float_grade_with_dither_beats_8bit_banding` — Float+Dither
  erschließt ≥1,5× mehr Helligkeitsstufen als der simulierte 8-Bit-Pfad und füllt
  die Range lückenlos.
- `core::export::pipe_format_follows_target_bit_depth` + `encoder_args_cover_codec_specifics`
  — Pipe-/Encoder-Routing (HEVC main10 etc.).
- `core::export::format_matrix_*` — ProRes-Ausgabe real 10-Bit (`pix_fmt` enthält „10").
- **`core::export::end_to_end_export_preserves_10bit_gradient`** (Kernbeweis):
  12-Bit-ProRes-Quelle → Grade → ProRes-Ausgabe behält >256 Helligkeitsstufen
  (in einer 8-Bit-Pipeline physikalisch unmöglich).

## Stufe 3 — GPU-Vorschau (`src/ui/grade_shader.rs`, `src/panels/monitor.rs`)

- Der Programmmonitor komponiert den Layer-Stack auf der GPU (pro Layer
  `draw_texture_quad_graded`: Transform-Quad + eingebetteter Grade-Fragment-
  Shader), Effekte über die RenderTexture-Kette `ui/fx_shader.rs`.
- **Output-Dithering im Grade-Shader** (TPDF aus `gl_FragCoord`, formelgleich zu
  `core/pixbuf.rs`, an den Extremen getapert) bricht das Banding gegradeter
  Verläufe auf dem 8-Bit-Display. Schaltbar über `EDITRON_GRADE_DITHER=0` bzw.
  `GradeShader::set_dither`.
- **GPU↔CPU-Parität (DoD)**: `EDITRON_TEST_PARITY=1` rendert denselben Grade auf
  GPU (Shader, Dither aus) und CPU (`grade_buffer` → derselbe `grade_pixel`) und
  meldet die Differenz — gemessen **max 1 LSB, mean 0,03 LSB**. Vorschau und
  Export rechnen also identisch.

## Bewusste v1-Grenzen (Stufe 3)

- Der Compositor schreibt in den 8-Bit-Default-Framebuffer; ein echter
  **RGBA16F-Render-Target** ist über die sichere raylib-rs-API nicht sauber
  verfügbar (RenderTexture ist R8, der Wrapper kapselt das FBO privat) — er
  bräuchte rohes rlgl. Auf einem 8-Bit-Display ist der Gewinn ohnehin durch das
  Dithering abgedeckt.
- Die **Vorschau-Quelltexturen bleiben 8 Bit** (R8G8B8A8). 10-Bit-Quellen werden
  fürs Scrubbing auf 8 Bit dekodiert (das Dithering glättet die Grade-Stufen);
  der **Export** dekodiert/rechnet dagegen voll in 16 Bit/f32 (Stufe 1/2) — die
  Bildqualitäts-Garantie gilt für die Lieferung, nicht den 8-Bit-Vorschau-Monitor.

## Stufe 4 — Farbmanagement-Grundlagen (`core/export.rs`, `core/player.rs`)

- **Erkennung**: `services::probe_media` liest `color_transfer`/`color_primaries`/
  `color_space`/`color_range` (+ `bit_depth`). `export::OutputColor::from_stream`
  klassifiziert: BT.709 (SDR-Default), BT.2020, BT.2020+PQ (SMPTE 2084),
  BT.2020+HLG.
- **Ehrliche Export-Tags**: `build_render_plan` erkennt den dominanten
  Quellfarbraum (`detect_output_color`, PQ > HLG > BT.2020 > 709) und legt ihn in
  `RenderPlan.color`. `video_codec_args` setzt daraus die `scale`-Matrix UND die
  `-color_primaries/-color_trc/-colorspace`-Tags — kein stummes
  709-Fehltagging mehr. HDR-Material wird so durchgereicht statt zerstört.
- **Vorschau-Tonemap (HDR→SDR)**: HDR-Quellen (PQ/HLG, aus dem Original) werden
  im Player-Decode über `hdr_tonemap_prefix` (zscale linear → Hable-Tonemap →
  BT.709/tv) für SDR-Displays tone-gemappt; Proxy-/Cache-Decode bleibt SDR.
  Braucht ffmpeg mit `zscale` (libzimg) — fehlt es, hält der Layer den letzten
  Frame (kein Absturz).

## Verifikation (Stufe 4)

- `core::export::output_color_detection_and_honest_tags` — Klassifizierung +
  Tags + `video_codec_args` (BT.709 vs BT.2020/PQ).
- **`core::export::detects_and_tags_bt2020_pq_source`** — real PQ-getaggte Quelle
  (libx265 main10) → ffprobe erkennt PQ/BT.2020/10-Bit → Plan trägt
  `Bt2020Pq` → Encoder-Args tragen `smpte2084`/`bt2020`/`bt2020nc` (skippt sauber
  ohne libx265).
- Vorschau-Tonemap visuell verifiziert: PQ-Verlauf erscheint als sauberer
  SDR-Verlauf (R 13→234) statt PQ-verzerrt.

## Bewusste v1-Grenzen (Stufe 4)

- Die Korrektur/Compositing rechnen weiter in 709-Gamma; **HDR-Grading** (PQ/HLG
  korrekt graden) ist NICHT Teil — HDR-Quellen werden ungegradet sauber
  durchgereicht bzw. für die Vorschau tonemappt. Ein Grade auf HDR-Material ist
  noch nicht farbkorrekt.
- `detect_output_color` inspiziert direkte Clips; Multicam-Winkel und
  Nest-Inhalte fallen auf BT.709 zurück.

## Stufe 5 — Performance & erhaltene 8-Bit-Schnellpfade

Messung `export_perf_8bit_vs_16bit` (`cargo test export_perf -- --ignored --nocapture`),
1280×720, 100 Frames, libx264 ultrafast:

| Pfad | fps |
| --- | --- |
| 8-Bit-Schnellpfad (ein Layer, keine Korrektur, ffmpeg-direkt) | **339** |
| Float-Compositing → 8-Bit (rgba + Dither, mit Grade) | 36,3 |
| Float-Compositing → 10-Bit (rgba64le-Pipe, mit Grade) | 33,7 |

- **8-Bit-Schnellpfad erhalten**: Segmente mit genau einem unveränderten Layer
  (`VideoLayerPlan::is_identity`) laufen weiter ffmpeg-direkt in die Encoder-Pipe
  ohne CPU-Compositing/f32 — ~9× schneller als der Compositing-Pfad. Bei
  8-Bit-Quelle UND 8-Bit-Ziel fließen reine 8-Bit-Bytes (keine Bandbreite drauf).
- **16-Bit kostet ~7%** gegenüber Float-8-Bit (33,7 vs 36,3 fps): die verdoppelte
  Pixel-Bandbreite wird gut absorbiert, weil Decode + f32-Wandlung + Grade +
  Compositing den Aufwand dominieren, nicht das Byteschieben. Die rgba64le-Pipe
  greift nur bei >8-Bit-Zielen.
- Decode-Bandbreite: >8-Bit-Quellen werden als rgba64le (8 B/px) dekodiert, 8-Bit
  als rgba (4 B/px) — die Verdopplung trifft nur tatsächliches High-Bit-Material.
