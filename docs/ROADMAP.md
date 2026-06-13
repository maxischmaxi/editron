## 9. Audio-Engine-Feinschliff (Shuttle, Rückwärts, Sample-Genauigkeit)

Bekannte Engine-Lücken (in `docs/ARCHITECTURE.md` selbst gelistet): kein Ton
bei Shuttle (Rate ≠ 1) und Rückwärtswiedergabe, Block- statt sample-genaues
Einsetzen neuer Clips (≤ 85 ms), keine Drift-Korrektur gegen die
Hardware-Clock.

```text
Behebe die bekannten Lücken der Audio-Engine in Editron — auf absolut professionellem Niveau, wie die Transport-Audioqualität von DaVinci Resolve und Adobe Premiere Pro, sodass professionelle Video-Produzenten dem Ton beim Schneiden vertrauen können.

Ausgangslage (dokumentiert in docs/ARCHITECTURE.md, Mixdown in src/core/player.rs, Transport in src/core/playback.rs): kein Ton bei Shuttle (JKL, Rate ≠ 1) und bei Rückwärtswiedergabe; neue Clips setzen im Mixdown nur block-genau ein (AUDIO_CHUNK_FRAMES = 4096, bis ~85 ms Versatz); kein Drift-Ausgleich zwischen Playhead-Clock und Hardware-Audio-Clock über lange Wiedergaben. Bekannte raylib-Fallen beachten: AudioStream::update nimmt Bytes statt Frames (FFI direkt, siehe MasterStream), Mix-Block muss ≥ Geräte-Periode sein.

Umfang:
1. Sample-genaues Einsetzen: Clip-Starts/-Enden innerhalb eines Mix-Blocks exakt am richtigen Sample beginnen/enden lassen (Teilblock-Verarbeitung statt Ganzblock-Gate), inklusive kurzer Anti-Klick-Rampe (~5 ms) an harten Schnittkanten.
2. Audio bei Shuttle: Bei JKL-Raten (0.25×–4×) hörbares, pitch-korrigiertes Audio über Resampling/atempo der Decoder-Ketten — wie Premiere beim Shuttlen. Bei sehr hohen Raten degradieren (z. B. ab 4× stumm), Verhalten klar definieren.
3. Rückwärtswiedergabe mit Ton: blockweises Rückwärts-Lesen über segmentiertes Vorwärts-Decoding mit Umkehr-Puffer (Scrubbing-Charakter wie in Profi-NLEs ist akzeptabel, aber kontinuierlich und ohne Aussetzer).
4. Drift-Korrektur: Die Differenz zwischen konsumierten Hardware-Samples und der Playhead-Clock messen und sanft ausregeln (Micro-Resampling oder Clock-Slewing der Playhead-Position — Bild folgt Ton), sodass eine 30-Minuten-Wiedergabe lippensynchron bleibt.
5. Audio-Scrubbing beim Playhead-Ziehen (kurze Sample-Schnipsel am Playhead, abschaltbar) — der Standard-Workflow für Schnitt nach Ton.
6. Tests/Verifikation: Sample-Genauigkeit per gerendertem Mixdown-Vergleich (Testton-Clips an krummen Positionen), Drift-Messung über simulierte lange Wiedergabe, manuelle Verifikation mit EDITRON_AUDIO_DEBUG.

Qualitätsanspruch: Keine Knackser, keine Aussetzer, kein hörbarer Versatz — Ton ist beim professionellen Schnitt das Taktgefühl des Editors, die Engine muss sich an Premiere Pro und Resolve messen lassen.
```

## 10. Medien-Browser: Bins, Metadaten & Verwaltung

Keine Bins/Ordner im Medien-Browser (Suche existiert), keine
Metadaten-Spalten/-Bearbeitung.

```text
Baue den Medien-Browser von Editron zu einer professionellen Medienverwaltung aus — auf dem Niveau des Projekt-Panels von Adobe Premiere Pro bzw. des Media Pools von DaVinci Resolve, sodass professionelle Video-Produzenten auch große Projekte (hunderte Clips) organisiert halten können.

Ausgangslage: src/panels/media_browser.rs hat eine flache Asset-Liste mit Suche, Thumbnails und Drag&Drop in die Timeline. Keine Ordnerstruktur, keine Metadaten-Ansicht, keine Sortierung.

Umfang:
1. Bins (Ordner) im MediaStore (src/stores.rs / src/core/types.rs): beliebig verschachtelbar, Assets gehören zu genau einem Bin (Root als Standard). Persistenz im .etron-Format (versioniert, Altprojekte landen im Root). Anlegen/Umbenennen/Löschen (mit Inhalt-Behandlung: Nachfrage), Drag&Drop von Assets zwischen Bins, Mehrfachauswahl (Mod/Shift-Klick, Marquee).
2. Zwei Ansichten wie Premiere: Icon-Ansicht (Thumbnail-Raster mit Hover-Scrub über das Thumbnail, sofern Frames vorhanden) und Listen-Ansicht mit sortierbaren Metadaten-Spalten (Name, Dauer, Framerate, Auflösung, Codec, Audio-Kanäle, Dateigröße, Pfad, Aufnahmedatum aus ffprobe). Spaltenbreiten anpassbar, Sortierung klickbar, Persistenz des Ansichts-Zustands.
3. Asset-Funktionen: Umbenennen (Anzeigename unabhängig vom Dateinamen), Farbetiketten, Suchfeld durchsucht Name+Metadaten über alle Bins (mit Bin-Pfad-Anzeige im Treffer), „Im Dateimanager zeigen“, „In Quellmonitor laden“, Eigenschaften-Ansicht (vollständige ffprobe-Daten im Info-Panel).
4. Verwendungs-Tracking: pro Asset anzeigen, ob und wie oft es in der Timeline verwendet wird; „In Timeline anzeigen“ springt zur ersten Verwendung. Beim Entfernen eines verwendeten Assets warnen.
5. Alles über Commands (Kontextmenüs command-basiert wie im Rest der App), Undo für Bin-/Metadaten-Operationen, Tests für Bin-Modell + Persistenz-Roundtrip + Verwendungs-Tracking.

Qualitätsanspruch: Bei 500 Assets muss das Panel flüssig bleiben (Thumbnail-Laden lazy über ui.texture_requests, Suche ohne Ruckler). Organisation ist die halbe Postproduktion — das Ergebnis muss sich am Premiere-Projekt-Panel messen lassen.
```

## 11. Proxy-Workflow

Fehlt komplett (nur Proxy-_Encoder-Profile_ im Export-Katalog) — für
4K-Material auf normaler Hardware essenziell.

```text
Implementiere einen Proxy-Workflow in Editron — auf absolut professionellem Niveau, wie Proxies in Adobe Premiere Pro und DaVinci Resolve, sodass professionelle Video-Produzenten 4K/8K-Material auf normaler Hardware flüssig schneiden können.

Ausgangslage: Die Medien-Engine (src/services.rs) kapselt ffmpeg in Worker-Threads mit Job-Registry und Events; der Player (src/core/player.rs) dekodiert pro sichtbarem Clip über ffmpeg-Pipes; MediaAsset (src/core/types.rs) kennt bereits offline/Relink. Es gibt keinerlei Proxy-Unterstützung.

Umfang:
1. Proxy-Erzeugung: Command/Kontextmenü „Proxies erstellen“ für ausgewählte Assets (und „für alle“) — Transcode-Jobs in der bestehenden Worker-/Job-Registry-Infrastruktur (parallelisiert, abbrechbar, Fortschritt pro Asset als Badge/Prozent im Medien-Browser). Proxy-Format: ProRes Proxy oder DNxHR LB in halber/viertel Auflösung (wählbar), Audio durchgereicht. Ablage in einem Proxy-Ordner neben dem Projekt (konfigurierbar), Wiederverwendung bei erneutem Öffnen (Existenz+mtime-Check), Persistenz des Proxy-Pfads am MediaAsset im .etron-Format.
2. Umschalter „Proxies verwenden“ (global, Command + Button im Programmmonitor wie Premiere): Bei aktivem Toggle dekodieren Player UND Thumbnails/Waveforms aus der Proxy-Datei; der EXPORT verwendet IMMER die Originale (das ist der Kern des Workflows — niemals versehentlich Proxy-Qualität exportieren). Laufende Decoder-Sessions beim Umschalten sauber neu aufsetzen.
3. Korrektheit: Proxy und Original müssen zeitlich exakt deckungsgleich sein (gleiche Dauer/Framerate; bei VFR-Material über CFR-Transcode normalisieren). Transform-/Grade-Mathematik ist bereits auflösungsunabhängig — verifizieren, dass Vorschau mit Proxy und Export mit Original identisch gerahmt sind.
4. Status-Sichtbarkeit: Proxy-Badge am Asset (vorhanden/wird erstellt/fehlt), Anzeige im Info-Panel, Warnung im Export-Dialog falls Assets offline sind, aber Proxies existieren (Hinweis auf Original-Relink).
5. Fehlerfälle: Proxy-Erzeugung schlägt fehl → Fehler-Badge + Retry; Proxy-Datei gelöscht → automatischer Fallback auf Original mit Hinweis. Tests: Pfad-/Zustandslogik, Roundtrip, Toggle-Verhalten (Export nutzt nie Proxy).

Qualitätsanspruch: Der Workflow muss unsichtbar funktionieren — einmal eingerichtet, schneidet der Editor 4K-Material wie HD, und der Export ist garantiert in Originalqualität. Das Ergebnis muss sich an Premiere Pro messen lassen.
```

## 12. Render-Cache & Wiedergabe-Performance

Kein Render-Cache/Prerendering; Scrubbing setzt pro Seek Decoder neu auf,
Frames werden bei Last gedroppt. Kein Hardware-Decode.

```text
Implementiere Render-Cache und Wiedergabe-Performance-Verbesserungen in Editron — auf absolut professionellem Niveau, vergleichbar mit der Timeline-Performance von DaVinci Resolve und Adobe Premiere Pro, sodass professionelle Video-Produzenten auch komplexe Sequenzen flüssig abspielen und scrubben können.

Ausgangslage: src/core/player.rs startet pro sichtbarem Video-Layer einen ffmpeg-Decoder (rawvideo/rgba über Pipes); jeder Seek setzt die Session neu auf (spürbare Latenz beim Scrubben), bei Überlast werden Frames gedroppt. Es gibt keinen Frame-Cache und kein Prerendering. Texture-Uploads laufen über ui.texture_requests zwischen den Frames.

Umfang:
1. Frame-Cache fürs Scrubbing: dekodierte Frames (pro Clip, in Wiedergabeauflösung) in einem RAM-begrenzten LRU-Cache halten (Budget konfigurierbar, Standard z. B. 2 GB); beim Scrubben zuerst den Cache treffen, nur bei Miss dekodieren. Beim Pausieren die Umgebung des Playheads vorausdekodieren (read-ahead in beide Richtungen), damit Frame-Stepping (←/→) sofort reagiert.
2. Smarteres Seeking: Decoder-Sessions bei kleinen Sprüngen weiterverwenden (vorwärts innerhalb weniger Sekunden: weiterlesen statt neu aufsetzen), ffmpeg-Seek mit -ss vor -i (Keyframe-genau) plus Feinpositionierung. Scrub-Anfragen koaleszieren (nur den letzten angefragten Frame dekodieren, nicht jeden Maus-Tick).
3. Hardware-Decode optional: verfügbare hwaccels erkennen (ffmpeg -hwaccels: VAAPI/NVDEC/VideoToolbox) und für die Decoder-Ketten nutzen, mit sauberem Fallback auf Software bei Fehlern; Umschalter in den Einstellungen bzw. per Env-Override.
4. Sequenz-Render-Cache (In-to-Out-Prerender wie Premieres „Render In to Out“): Bereiche der Sequenz im Hintergrund in eine Cache-Datei rendern (bestehende Export-Compositor-Logik wiederverwenden, niedriges Encoder-Preset), farbige Render-Leiste im Timeline-Lineal (rot = komplex/ungecacht, grün = gecacht), Cache-Invalidierung über die bestehenden Revision-Zähler (Edits im Bereich invalidieren ihn). Wiedergabe bevorzugt gecachte Bereiche.
5. Messbarkeit: Dropped-Frame-Indikator im Programmmonitor (wie Resolve), optionales Performance-Overlay (Decode-/Upload-Zeiten). Verifikation: Scrub-Latenz und Playback-Stabilität vorher/nachher dokumentieren, cargo test für Cache-Invalidierung und Seek-Koaleszenz.

Qualitätsanspruch: Scrubben muss sich „klebrig-direkt“ anfühlen (Frame unter der Maus ohne spürbare Latenz bei gecachten Bereichen), Wiedergabe mehrlagiger Sequenzen stabil ohne Drops. Performance ist das Erste, was ein Profi an einem NLE spürt.
```

## 13. Export-Ausbau: Hintergrund-Export, Render-Queue, Presets, Hardware-Encoder

Kein Hintergrund-Export (Dialog ist modal und blockiert die App), keine
Render-Queue, keine eigenen Presets, keine Hardware-Encoder
(NVENC/VideoToolbox/QSV), keine Bild-Sequenzen.

```text
Baue den Sequenz-Export von Editron zu einem professionellen Render-System aus — auf dem Niveau des Adobe Media Encoder / der Renderliste von DaVinci Resolve, sodass professionelle Video-Produzenten Lieferformate effizient und parallel produzieren können.

Ausgangslage: src/core/export.rs + src/overlays/export_dialog.rs — vollwertiger Export mit Codec-Katalog, Validierung, Renderplan, zweiphasigem Worker (Audio-Mix → Video segmentweise), Fortschritt/ETA/Abbruch, atomarem Finalisieren. Aber: der Dialog ist modal und blockiert die App während des Renderns, es gibt genau einen Job, keine eigenen Presets, keine Hardware-Encoder, keine Bild-Sequenzen.

Umfang:
1. Hintergrund-Export: Der Render-Worker läuft bereits in einem eigenen Thread — den Dialog nach Job-Start schließbar machen, Fortschritt in die StatusBar (Prozent + ETA, Klick öffnet die Queue), Weiterarbeiten während des Renderns ohne Einschränkung. WICHTIG: Der Renderplan wird beim Start aus einem Snapshot der Timeline gebaut — sicherstellen, dass spätere Edits den laufenden Export nicht beeinflussen (Plan ist bereits entkoppelt — verifizieren, auch für Medien-Relink während des Renderns).
2. Render-Queue: mehrere Jobs anlegen (gleiche Sequenz mit verschiedenen Presets oder verschiedene Bereiche), Queue-Ansicht (neues Panel oder Dialog-Tab) mit Status je Job (wartend/läuft/fertig/Fehler/abgebrochen), Reihenfolge änderbar, sequentielle Abarbeitung (parallel optional später), Einzelne abbrechen/neu starten. Beim App-Beenden mit laufender Queue warnen (cancel_all_jobs existiert).
3. Eigene Presets: aktuelle Export-Einstellungen unter Namen speichern/überschreiben/löschen (JSON im XDG-Config-Verzeichnis), erscheinen neben den eingebauten Presets im Dialog.
4. Hardware-Encoder: verfügbare Encoder erkennen (ffmpeg -encoders wird bereits abgefragt): h264_nvenc/hevc_nvenc, h264_videotoolbox/hevc_videotoolbox, h264_qsv/hevc_qsv, h264_vaapi/hevc_vaapi — als Encoder-Wahl im Dialog („Hardware (NVENC)“ etc.), mit encoder-spezifischen Qualitätsparametern (CQ/Bitrate statt CRF wo nötig) und Validierung (nicht verfügbare ausblenden). Fallback-Hinweis bei Encoder-Fehler mit ffmpeg-stderr.
5. Bild-Sequenz-Export: PNG/JPEG/TIFF-Sequenz als „Container“ im Katalog (Muster out_%06d.png, Startnummer, nur Video-Phase), plus Einzel-Frame-Export („Frame exportieren“ am Programmmonitor, wie Premieres Kamera-Icon).
6. Tests: Preset-Persistenz-Roundtrip, Queue-Zustandsmaschine, Encoder-Detection-Mapping; End-to-End-Verifikation via EDITRON_TEST_EXPORT.

Qualitätsanspruch: Render-Zuverlässigkeit ist heilig — kein Job darf ein halbes File hinterlassen (atomares Finalisieren beibehalten), Fortschritt/ETA müssen ehrlich sein, und ein Encoder-Fehler muss verständlich gemeldet werden. Das Ergebnis muss sich am Media Encoder messen lassen.
```

## 14. Mehrere Sequenzen & Nesting

`AppState` hält genau eine `TimelineStore`. Ohne mehrere Sequenzen und
verschachtelte Sequenzen ist kein dokumentarischer/episodischer Workflow
möglich.

```text
Implementiere mehrere Sequenzen pro Projekt und verschachtelte Sequenzen (Nesting) in Editron — auf absolut professionellem Niveau, wie in Adobe Premiere Pro und DaVinci Resolve, sodass professionelle Video-Produzenten dokumentarische und episodische Projekte strukturieren können.

Ausgangslage: AppState (src/state.rs) hält genau eine TimelineStore; das .etron-Format (src/core/project.rs) speichert genau eine Timeline. Der Medien-Browser bekommt ggf. parallel Bins — Sequenzen sollen dort als eigene Asset-Art erscheinen.

Umfang:
1. Projektmodell: Liste von Sequenzen (je eigene TimelineStore mit eigener Undo-History, eigenen Sequenz-Einstellungen, eigenem Playhead/Zoom), eine davon aktiv. Persistenz im .etron-Format (versioniert; Altprojekte = eine Sequenz). Sequenzen erscheinen im Medien-Browser (eigenes Icon, Doppelklick öffnet sie), Commands: Neue Sequenz, Duplizieren, Umbenennen, Löschen (mit Schutz: nicht die letzte, Warnung bei Verwendung als Nest).
2. Sequenz-Tabs über der Timeline (wie Premiere): offene Sequenzen als Tabs, Klick wechselt, Mittelklick/× schließt den Tab (Sequenz bleibt im Projekt), Reihenfolge per Drag. Der Programmmonitor folgt der aktiven Sequenz; Dirty-Tracking (Revision-Zähler) über alle Sequenzen aggregieren.
3. Nesting: Eine Sequenz per Drag&Drop aus dem Medien-Browser in eine andere Timeline einsetzen — als Nest-Clip (Videospur, Audio-Mixdown-Anteil analog), Dauer = Sequenzlänge, trimm-/verschiebbar wie ein normaler Clip, ClipFx (Transform/Deckkraft) und Grade anwendbar. Rekursionsschutz: eine Sequenz darf sich nicht (auch nicht transitiv) selbst enthalten — beim Einfügen prüfen und ablehnen.
4. Wiedergabe & Export von Nests: Der Player und der Export-Renderplan lösen Nest-Clips rekursiv auf (Layer-Stapel der inneren Sequenz an der Medienzeit-Position, dann äußere Transformationen darauf). Gemeinsame Auflösungslogik in src/core/compose.rs, von Player und Export geteilt — Vorschau und Export müssen identisch sein. Framerate-Anpassung innen/außen korrekt (innere Sequenzzeit → äußere Zeit).
5. „Doppelklick auf Nest öffnet die innere Sequenz“ (im Tab), wie Premiere. Tests: Roundtrip mit mehreren Sequenzen, Rekursionsschutz (direkt + transitiv), Nest-Auflösung im Renderplan (Frame-Zuordnung), Undo-Isolation pro Sequenz.

Qualitätsanspruch: Nesting ist Premieres mächtigstes Strukturwerkzeug — die Auflösung muss frame-genau und in Vorschau und Export identisch sein, der Rekursionsschutz wasserdicht. Das Ergebnis muss sich an Premiere Pro messen lassen.
```

## 15. Interop: OTIO / EDL / FCP-XML

Kein Austauschformat — für „production ready“ im professionellen Sinn
(Übergabe an Grading/Mischung) ein Muss.

```text
Implementiere Timeline-Austauschformate (Interop) in Editron — auf absolut professionellem Niveau, sodass professionelle Video-Produzenten Schnitte an DaVinci Resolve (Grading) und andere Tools übergeben und von dort empfangen können.

Ausgangslage: Es gibt keinerlei Austauschformat. Das Timeline-Modell lebt in src/core/timeline.rs (Tracks/Clips mit src_in/src_duration, verknüpfte A/V-Paare), Medien in src/core/types.rs, Timecode-Formatierung in src/core/timecode.rs.

Umfang:
1. OpenTimelineIO (OTIO, JSON-Schema) als primäres Format: Export der aktiven Sequenz (Tracks, Clips mit Medienreferenzen als absolute Pfade + Dateinamen, Source-Range in Frames bei Sequenz-Framerate, Lücken als Gaps, Marker falls vorhanden, Übergänge falls vorhanden) und Import (OTIO-Datei → neue Sequenz; unbekannte Medienpfade als Offline-Assets anlegen und den vorhandenen Relink-Wizard anstoßen). Die OTIO-JSON-Struktur direkt mit serde abbilden (kein C++-Binding) — Schema-Version OTIO 0.17 (`"OTIO_SCHEMA": "Timeline.1"` etc.), gegen in Resolve erzeugte Beispieldateien testen.
2. CMX-3600-EDL: Export der aktiven Sequenz (V- und A-Events, Quell-/Record-Timecode aus den Sequenz-/Medien-Framerates, FROM CLIP NAME-Kommentare, Überblendungen als Dissolve-Events falls vorhanden) und Import (EDL → Sequenz mit Offline-Assets + Relink). Die EDL-Eigenheiten korrekt: eine Video-Spur, Events nummeriert, Drop-/Non-Drop-Timecode-Notation.
3. Final Cut Pro XML (fcpxml, Version 1.11) mindestens als Export — Resolve und viele Tools importieren es zuverlässig; Struktur (resources/format/asset, spine mit clips, Offsets als Rationalzahlen „N/Ds“) sauber abbilden.
4. UI: Datei-Menü „Importieren“ / „Exportieren“ → Untermenü mit den Formaten (Commands, rfd-Dateidialoge); beim Import Fortschritt + Ergebnis-Zusammenfassung (n Clips, m offline → Relink öffnen). Klare Grenzen kommunizieren: was Editron nicht abbilden kann (z. B. Effekte fremder Tools), wird beim Import ignoriert und im Ergebnis-Dialog aufgelistet — niemals stillschweigend.
5. Tests sind hier das Wichtigste: Roundtrip Editron → OTIO → Editron verlustfrei für alle Kernfelder (Frame-genau, an krummen Framerates wie 23.976 mit Rationalzahlen-Genauigkeit); EDL-Timecode-Berechnungen; fcpxml gegen das offizielle DTD-Schema validieren. Beispieldateien aus Resolve/Premiere als Fixtures ins Repo legen (tests/fixtures/).

Qualitätsanspruch: Interop ist binär — entweder der Schnitt kommt frame-genau in Resolve an oder das Feature ist wertlos. Frame-Genauigkeit bei allen Framerates, keine stillen Datenverluste, jede Auslassung wird dem Nutzer gemeldet.
```

## 16. HiDPI-Skalierung

Bekannte Lücke: aktuell 1 Logikpixel = 1 Fensterpixel — auf
HiDPI-/Retina-Displays ist die UI winzig bzw. unscharf.

```text
Implementiere HiDPI-Skalierung in Editron — auf absolut professionellem Niveau: gestochen scharfe UI auf Retina-/4K-Displays, wie sie professionelle Video-Produzenten von DaVinci Resolve und Adobe Premiere Pro erwarten.

Ausgangslage (dokumentiert als bekannte Lücke): 1 Logikpixel = 1 Fensterpixel. Das UI-Framework (src/ui/) rechnet in absoluten Pixeln (Rect/RectCut-Layout, 4-px-Skala aus src/theme.rs), Fonts werden mit 2×-Supersampling gerastert (src/ui/text.rs), Icons sind tessellierte Pfade, Maus-Koordinaten kommen aus raylib (src/ui/input.rs).

Umfang:
1. Skalierungsfaktor ermitteln (raylib get_window_scale_dpi, plus manueller Override in den Einstellungen/Env-Var für Sonderfälle) und als globalen UI-Scale einführen: ein zentraler Faktor, der Layout-Logikpixel auf Framebuffer-Pixel abbildet.
2. Saubere Architektur statt Streu-Multiplikationen: Das Layout rechnet weiter in Logikpixeln; die Zeichen-/Hit-Test-Schicht (Ui-Kontext: fill/text/icon/Clip-Stack, interact, Maus-Koordinaten) übersetzt zentral. Ziel ist, dass Panels NICHT angefasst werden müssen — die Übersetzung passiert an der Ui-/Input-Grenze.
3. Schärfe: Fonts in physikalischer Auflösung rastern (Atlas-Größe × Scale, das 2×-Supersampling entsprechend anpassen), Icon-Tessellation in physikalischer Auflösung, Linien/Hairlines auf physikalische Pixel gesnappt (1-px-Linien müssen scharf bleiben, nicht 1,5 px verschmiert). Player-/Thumbnail-Texturen unverändert (Inhalte skalieren wie bisher über draw_texture_pro).
4. Dynamik: Fenster-Verschiebung zwischen Monitoren mit unterschiedlichem DPI zur Laufzeit erkennen und Atlanten/Scale neu aufbauen (mindestens: beim Scale-Wechsel sauber neu initialisieren, kein Neustart nötig). Fraktionale Faktoren (1.25/1.5) müssen funktionieren, nicht nur 2.0.
5. Verifikation: visueller Smoke-Test (EDITRON_SHOT) bei Scale 1.0/1.5/2.0 vergleichen (Layout identisch, nur schärfer), Maus-Hit-Tests an Panel-Grenzen und in gescrollten Bereichen testen, alle Overlays (Menüs, Dialoge, Tooltips, Drag-Ghost) prüfen.

Qualitätsanspruch: Auf einem 4K-Display muss Editron aussehen wie eine native Profi-App — pixelscharfe Typografie, scharfe 1-px-Linien, korrekte Hit-Targets. Keine halb skalierten Mischzustände.
```

## 17. App-Reife: Einstellungen-Dialog & Autosave-Versionen

Der Einstellungen-Command ist ein Stub („Einstellungen folgen“); Autosave gibt
es nur als Sitzungs-Autosave beim Wechsel/Beenden — Premiere hat
zeitgesteuertes Autosave mit Versionshistorie.

```text
Implementiere einen Einstellungen-Dialog und zeitgesteuertes Autosave mit Versionen in Editron — auf absolut professionellem Niveau, wie es professionelle Video-Produzenten von Adobe Premiere Pro und DaVinci Resolve erwarten (verlorene Arbeit ist inakzeptabel).

Ausgangslage: Der Command app.settings ist ein Stub („Einstellungen folgen“, src/core/commands.rs). Es gibt nur ein Sitzungs-Autosave beim Projektwechsel/Beenden (safeguard_unsaved → autosave.etron). Persistente Nutzerdaten liegen bereits im XDG-Config-Verzeichnis (keymap.json, recent_projects.json).

Umfang:
1. Einstellungen-Infrastruktur: zentrale, typisierte Settings-Struktur, als settings.json im XDG-Config-Verzeichnis persistiert (serde, Defaults bei fehlenden Feldern — vorwärts-/rückwärtskompatibel), live wirksam ohne Neustart. Änderungen laufen über den Store, damit UI und Subsysteme konsistent lesen.
2. Einstellungen-Dialog (modales Overlay im Stil des Export-Dialogs, Command app.settings + Mod+Komma): Kategorien links, Inhalte rechts. Erste Kategorien: Allgemein (Sprache vorbereitet, Wiedergabe-Auflösungs-Default), Autosave (an/aus, Intervall, Versionsanzahl), Wiedergabe (Hardware-Decode-Toggle sofern vorhanden, Audio-Gerät/Puffer sofern sinnvoll), Medien (Proxy-Zielordner sofern vorhanden, ffmpeg-Pfad-Override mit Validierung + Versionsanzeige), Erscheinungsbild (UI-Scale-Override sofern vorhanden). Nur Kategorien für tatsächlich existierende Subsysteme bauen — keine toten Schalter.
3. Zeitgesteuertes Autosave mit Versionen: alle N Minuten (Default 5, konfigurierbar) bei dirty Projekt eine Versionskopie nach <projektordner>/.etron-autosave/<name>_JJJJ-MM-TT_HH-MM-SS.etron schreiben (atomar wie das normale Speichern), maximal K Versionen behalten (Default 20, älteste löschen), niemals die Originaldatei anfassen. Bei ungespeicherten Projekten weiterhin ins XDG-Verzeichnis. Der Timer läuft im Mainloop (kein eigener Thread nötig), pausiert während Export-Rendering kritischer Phasen nicht die UI.
4. Wiederherstellung: Datei-Menü „Autosave-Versionen…“ → Liste der Versionen des aktuellen Projekts (Zeitstempel, Größe), Auswahl öffnet die Version (als ungespeicherte Kopie, Original bleibt unberührt). Nach einem Absturz beim nächsten Start anbieten, die jüngste Autosave-Version zu öffnen, wenn sie neuer als die Projektdatei ist.
5. Tests: Settings-Roundtrip inkl. unbekannter Felder, Autosave-Rotation (K-Limit), Versions-Dateinamen, Crash-Recovery-Erkennung (mtime-Vergleich).

Qualitätsanspruch: Ein Stromausfall nach drei Stunden Schnitt darf maximal ein Autosave-Intervall kosten. Die Einstellungen müssen sofort wirken und robust persistieren — das ist Grundvertrauen, das Profis von Premiere und Resolve gewohnt sind.
```

## 18. Multicam-Schnitt

Für Premiere/Resolve-Parität relevant, aber spät auf der Liste.

```text
Implementiere Multicam-Schnitt in Editron — auf absolut professionellem Niveau, wie Multikamera-Sequenzen in Adobe Premiere Pro und der Multicam-Schnitt in DaVinci Resolve, sodass professionelle Video-Produzenten mehrkamerige Produktionen (Interviews, Events, Konzerte) effizient schneiden können.

Voraussetzungen: mehrere Sequenzen/Nesting und Sequenz-Einstellungen sollten bereits umgesetzt sein (Multicam baut konzeptionell auf Nest-Clips auf).

Umfang:
1. Multicam-Quelle erstellen: Mehrfachauswahl von Assets im Medien-Browser → „Multicam-Quelle erstellen“ mit Synchronisierung wahlweise per gemeinsamem Startpunkt, per Timecode (sofern Medien-TC vorhanden) oder per Audio-Waveform-Analyse (Kreuzkorrelation der vorhandenen Waveform-Peaks — der Premiere-Killer-Workflow). Ergebnis ist ein Multicam-Asset (intern eine spezielle Sequenz: je Kamera ein Winkel mit Sync-Offset), persistiert im .etron-Format.
2. Multicam-Clip in der Timeline: verhält sich wie ein normaler Clip (trimmen, verschieben, ClipFx, Grade), trägt aber den aktiven Winkel; Razor-Split + Winkelwechsel sind die Kern-Edits.
3. Multicam-Monitor: Ansicht im Programmmonitor (umschaltbar), die alle Winkel als Raster (2×2, 3×3 je nach Anzahl) synchron abspielt — ein Decoder je Winkel (die bestehende Ein-Decoder-pro-Layer-Infrastruktur in src/core/player.rs wiederverwenden, Wiedergabeauflösung der Kacheln reduziert); der aktive Winkel ist markiert.
4. Live-Schnitt: Während der Wiedergabe schaltet Zifferntaste 1–9 den Winkel um UND setzt einen Schnitt am Playhead (wie Premiere); ohne Wiedergabe wechselt die Zifferntaste nur den Winkel des ausgewählten Clips. Alles als Commands mit when-Klauseln (nur im Multicam-Kontext aktiv), in allen Keymap-Presets.
5. Export: Multicam-Clips rendern den aktiven Winkel (Auflösung über den Renderplan wie Nest-Clips — frame-genau identisch zur Vorschau). „Auf einzelne Clips reduzieren“ (Flatten) als Command: ersetzt Multicam-Clips durch normale Clips des gewählten Winkels.
6. Tests: Audio-Sync-Korrelation (synthetische Testsignale mit bekanntem Offset), Winkelwechsel-Schnittlogik, Roundtrip, Renderplan-Auflösung.

Qualitätsanspruch: Der Live-Schnitt per Zifferntasten muss latenzfrei und die Audio-Synchronisierung sample-präzise sein — Multicam steht und fällt mit dem Sync. Das Ergebnis muss sich an Premiere Pro messen lassen.
```

## 19. GPU-Compositing & 10-Bit/HDR-Pipeline (Langstrecke)

Aktuell läuft alles über `rawvideo/rgba` (8 Bit) und der Export-Compositor
rechnet auf der CPU — womit 10-Bit/HDR-Material und ernsthaftes Grading
prinzipiell ausscheiden. Das ist die Langstrecke, auf der Resolve seine
Jahrzehnte Vorsprung hat.

```text
Rüste Editrons Bild-Pipeline auf GPU-Compositing und >8-Bit-Verarbeitung um — auf absolut professionellem Niveau, in Richtung der Bildqualität von DaVinci Resolve, sodass professionelle Video-Produzenten 10-Bit-Material (Log-Footage, HDR-Lieferungen) ohne Qualitätsverlust verarbeiten können.

Ausgangslage: Die gesamte Pipeline ist 8 Bit RGBA — Decoder liefern rawvideo/rgba (src/core/player.rs), der Export-Compositor rechnet auf der CPU in u8 (src/core/compose.rs), die Farbkorrektur existiert doppelt als GLSL-Shader (Vorschau) und CPU-Pfad (Export). Banding bei Grades auf 10-Bit-Material ist damit unvermeidbar.

Umfang (als tragfähige Stufen planen und Stufe für Stufe verifizieren):
1. Decode in höherer Bittiefe: Pipe-Format auf 16 Bit pro Kanal umstellen (ffmpeg rgba64le bzw. p010/yuv420p10 + Konvertierung), Quell-Bittiefe aus ffprobe erkennen und nur dann hochfahren (Speicher-/Bandbreiten-Kosten bewusst steuern: 8-Bit-Material bleibt 8 Bit).
2. Export-Compositing in Float: compose::composite_frame und grade_buffer auf f32-Verarbeitung pro Kanal heben (linear-light-korrektes Blending erwägen und Entscheidung dokumentieren), Ausgabe-Dithering beim Quantisieren auf das Ziel-Pixelformat; Encoder-Seite 10-Bit-Profile anbieten (H.265 main10, ProRes ist es ohnehin, yuv420p10le/yuv422p10le je Codec) inkl. Validierung.
3. GPU-Compositing für die Vorschau: Layer-Stack inkl. Transformationen und Grade vollständig auf der GPU komponieren (Render-Texture-Kette in raylib, 16F-Framebuffer wo verfügbar); die bestehende GLSL-Grade-Pipeline einbetten. CPU-Export-Pfad bleibt Referenz — ein Parität-Test (gleicher Frame GPU vs. CPU innerhalb definierter Toleranz) gehört zur Definition of Done.
4. Farbmanagement-Grundlagen: Quell-Transfer/Primaries aus ffprobe lesen (BT.709/BT.2020/HLG/PQ erkennen), Tone-Mapping HDR→SDR für die Vorschau auf SDR-Displays, korrekte Tags im Export (color_primaries/trc/space werden für BT.709 bereits gesetzt — auf erkannte Quellfarbräume erweitern). Vollständiges HDR-Grading ist NICHT Teil dieses Schritts — aber die Pipeline darf 10-Bit-HDR-Material nicht mehr zerstören.
5. Performance bewusst messen: 10-/16-Bit verdoppelt Bandbreiten — Frame-Zeiten und Export-fps vorher/nachher dokumentieren, 8-Bit-Schnellpfade erhalten.

Qualitätsanspruch: Kein Banding auf 10-Bit-Verläufen nach einem Grade, messbare Parität zwischen Vorschau und Export, ehrliche Farbraum-Tags. Das ist die Bildqualitäts-Liga, in der Resolve spielt — jede Stufe muss verifiziert sein, bevor die nächste beginnt.
```
