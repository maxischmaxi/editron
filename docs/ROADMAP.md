==== G01 — Color-Grade kopieren und auf mehrere Clips einfuegen ====
Ziel: Ein "Grade kopieren"/"Grade einfuegen"-Workflow, mit dem die komplette Farbkorrektur eines Clips
auf einen oder mehrere selektierte Clips uebertragen wird. Das ist heute gar nicht moeglich und einer
der groessten Produktivitaetsgewinne pro Aufwand bei laengeren Projekten.
Fertig wenn: Es gibt Commands color.copyGrade und color.pasteGrade (auf alle selektierten Clips
anwendbar, ein Undo-Schritt). Im Color-Panel und/oder Kontextmenue erreichbar. Default-Keybinding gesetzt.
Funktioniert ueber Sequenzgrenzen hinweg via internem Clipboard im AppState.
Anker: struct ColorGrade in src/core/grade.rs ist bereits Clone + serde. Pro-Clip-Grade haengt am
TimelineClip (src/core/timeline.rs). Nur Clipboard-Feld + zwei Commands noetig, keine neue Engine.
Konventionen: Als Commands registrieren + binden; ein Undo-Snapshot pro Paste.
Verifikation: cargo test (Grade bleibt nach Copy/Paste wertgleich); manuell zwei Clips, Grade uebertragen.
Aufwand: S

==== G02 — Extend Edit (Schnittkante bis zum Playhead trimmen) per Tastatur ====
Ziel: Eine tastaturgetriebene Operation, die die naechste Schnittkante (auf den Ziel-Spuren) bis zur
aktuellen Playhead-Position verlaengert oder kuerzt — der Kern des mausfreien Trim-Workflows ("Extend Edit"
in Premiere = E, FCP = Shift+X).
Fertig wenn: Command timeline.extendEdit existiert, nimmt die Kante am/naechst zum Playhead auf den
aktiven Ziel-Spuren und ruft die vorhandene Trim-Logik mit dem Delta bis zum Playhead auf. Default-Binding
gesetzt (Vorschlag E). Respektiert Lock/Sync-Lock. Ein Undo-Schritt.
Anker: fn apply_trim und fn ripple_trim_clip / fn roll_edit in src/core/timeline.rs existieren bereits;
es fehlt nur der Command, der Kante+Delta aus Playhead/Selektion bestimmt. Ziel-Spur-Logik wie bei
src/core/edit.rs (perform_source_edit) nutzen.
Konventionen: Command + Binding in allen drei Presets (E ist in Premiere belegt).
Verifikation: cargo test (neuer Test: Kante landet exakt am Playhead-Frame); EDITRON_TEST-Screenshot.
Aufwand: S

==== G03 — Clips und Schnittkanten per Tastatur nudgen (frame- und sekundengenau) ====
Ziel: Selektierte Clips und (im Trim-Kontext) Schnittkanten per Tastatur um 1 Frame bzw. groesseren
Schritt nach links/rechts verschieben/trimmen. Aktuell geht Feinpositionierung nur per Maus-Drag.
Fertig wenn: Commands clip.nudgeLeft/clip.nudgeRight (Move um 1 Frame), plus Variante fuer groesseren
Schritt; im Trim-Modus nudgen sie stattdessen die aktive Kante. Default-Bindings (Vorschlag Alt+Pfeil bzw.
Komma/Punkt). Respektiert Snapping-Aus, Lock, Links/Sync-Lock. Ein Undo-Schritt pro Tastendruck (oder
sinnvoll koalesziert).
Anker: fn move_clips und fn apply_trim in src/core/timeline.rs sind vorhanden; Frame-Dauer aus der
Sequenz-FrameRate (src/core/sequence.rs). Nur Commands + Bindings.
Konventionen: Command + Binding; deutsche Command-Titel.
Verifikation: cargo test (Versatz exakt 1 Frame an 23,976/29,97); manuell.
Aufwand: S

==== G04 — .etron vorwaertskompatibel machen (unbekannte Zukunftsfelder verlustfrei erhalten) ====
Ziel: Ein aelterer Build soll ein neueres .etron-Projekt oeffnen, bearbeiten und speichern koennen, OHNE
neue/unbekannte Felder zu verlieren. Heute droppt ProjectFile unbekannte Felder (nur #[serde(default)])
und weist neuere Versionen sogar hart ab — beides fuehrt zu Datenverlust in Mixed-Version-Teams.
Fertig wenn: ProjectFile (und die relevanten verschachtelten Structs) bekommen ein #[serde(flatten)] extra: serde_json::Map<String, Value> und reichen Unbekanntes verlustfrei durch.
Der Hard-Reject bei file.version > PROJECT_VERSION wird zu einem Best-Effort-Load mit Warn-Hinweis
(statt Abbruch), solange das Basismodell deserialisierbar ist.
Anker: Vorbild existiert bereits: #[serde(flatten)] in struct AppSettings (src/core/settings.rs). In
src/core/project.rs sind struct ProjectFile, PROJECT_VERSION und der Versions-Reject (if file.version >
PROJECT_VERSION). Genau dieses Muster auf ProjectFile uebertragen.
Konventionen: Roundtrip-Test, der ein Projekt mit einem unbekannten Extra-Feld laedt+speichert und das
Feld erhalten findet.
Verifikation: cargo test.
Aufwand: S

==== G05 — Loudness-Normalisierung beim Export (EBU R128 / Streaming-LUFS) ====
Ziel: Beim Export optional die Audiospur auf ein Ziel-Lautheitsmass normalisieren (Integrated LUFS +
True-Peak-Ceiling). Ohne das ist KEIN delivery-konformer Broadcast-/Streaming-Export abschliessbar.
Fertig wenn: Im Export-Dialog ein "Lautheit normalisieren"-Abschnitt mit Presets (-23 LUFS EBU R128,
-16 LUFS, -14 LUFS Streaming, -24 LKFS ATSC A/85) und True-Peak-Limit (z. B. -1 dBTP). Umsetzung via
ffmpeg loudnorm (2-Pass: messen, dann anwenden) im Audio-Export-Pfad. Aus + frei einstellbar moeglich.
Anker: Audio-Export in src/core/export.rs (fn mix_audio_to_wav, fn process_audio_track). Codec-Tabellen
AUDIO_CODECS. Dialog in src/overlays/export_dialog.rs, Presets bei den Export-PRESETS in export.rs.
Konventionen: Setting/Preset in der Export-Konfiguration persistieren; deutsche UI-Texte.
Verifikation: Export eines Testtons, gemessenes Integrated-LUFS liegt am Ziel (+-1 LU).
Aufwand: S-M

==== G06 — Marker als Snapping-Ziel in der Timeline ====
Ziel: Clip-Kanten/Playhead beim Ziehen auch an Sequenz-Marker einrasten lassen (Beat-genaues Schneiden zu
Musik). Heute snappt nur an Clip-Kanten, Playhead und t=0.
Fertig wenn: Die Snap-Ziel-Sammlung der Timeline enthaelt zusaetzlich die Zeitpunkte aller Sequenz-Marker
(und Bereichsmarker-Grenzen). Verhalten respektiert den Snapping-An/Aus-Schalter (Taste S).
Anker: Snapping-Logik in src/panels/timeline.rs (Sammlung der Snap-Targets, ~collect_snap_targets /
snap_adjust). Marker liegen in src/core/marker.rs / an der Sequenz.
Konventionen: Keine Format-Aenderung. Nur die Snap-Target-Liste erweitern.
Verifikation: EDITRON_TEST_MARKER + Drag, Clip rastet an Markerzeit ein.
Aufwand: S

==== G07 — Programm-Wiedergabe loopen (umschaltbar) ====
Ziel: Ein Loop-Toggle fuer den Programm-Monitor (ueber die ganze Sequenz oder zwischen In/Out). Heute
loopt nur der Quellmonitor; das Programm loopt allenfalls implizit, wenn In/Out gesetzt sind.
Fertig wenn: Command playback.toggleLoop schaltet Programm-Loop an/aus (loopt zwischen In/Out, sonst ueber
die ganze Sequenz). Zustand sichtbar (Button im Programmmonitor). Default-Binding gesetzt.
Anker: Loop-Vorbild im Quellmonitor (src/panels/monitor.rs, SourceMonitorPanel). Wiedergabe-Routing in
src/core/playback.rs (active_target). Player in src/core/player.rs.
Konventionen: Command + Binding; Button mit Theme-Tokens.
Verifikation: Manuell — Wiedergabe springt am Ende zurueck.
Aufwand: S

==== G08 — Track-Management komplettieren (loeschen, Hoehe verstellbar, Add-Bindings) ====
Ziel: Spuren vollwertig verwalten: Spur per Command loeschen, Track-Hoehe per Drag verstellen (fuer
Waveforms/Keyframes), und Add/Remove-Bindings. Heute laesst sich nur hinzufuegen (Funktion remove_track
existiert, ist aber NICHT als Command registriert), und die Track-Hoehe ist fix pro Spurart.
Fertig wenn: Command timeline.removeTrack (ruft die vorhandene remove_track-Funktion, mit Schutz/Prompt
bei belegter Spur). Track-Hoehe per Sash-Drag am Spurkopf aenderbar und in der Sequenz persistiert.
Default-Bindings fuer addVideoTrack/addAudioTrack/removeTrack.
Anker: fn add_track / remove_track und toggle_track_flag in src/core/timeline.rs; Spurkopf-Rendering und
fixe Hoehen (VIDEO_H/AUDIO_H, track_height) in src/panels/timeline.rs.
Konventionen: Track-Hoehe ins .etron -> PROJECT_VERSION erhoehen + #[serde(default)].
Verifikation: cargo test (Roundtrip Track-Hoehe); manuell loeschen/resizen.
Aufwand: S (Hoehe-Persistenz S-M)

==== G09 — Stems- / Multitrack-Audio-Export ====
Ziel: Audiospuren getrennt exportieren (Stems: Dialog/Musik/Effekt) bzw. als getrennte Streams muxen,
statt sie immer zu einem Stereo-Master zu summieren. Die noetige Infrastruktur existiert schon fast komplett.
Fertig wenn: Export-Option "Stems" — pro Audiospur (oder pro Rolle/Gruppe) eine eigene Ausgabedatei bzw.
ein eigener Audio-Stream im Container. Bus-FX/Automation pro Spur bleiben angewandt.
Anker: src/core/export.rs erzeugt in fn process_audio_track bereits Per-Spur-Temp-WAVs, die in
fn mix_audio_to_wav summiert UND geloescht werden — diese WAVs stattdessen optional als Stems ausgeben/muxen.
Konventionen: Option im Export-Dialog (src/overlays/export_dialog.rs); deutsche Labels.
Verifikation: Export mit 2 Audiospuren erzeugt 2 Stems; Inhalt entspricht den Einzelspuren.
Aufwand: S-M

==== G10 — Untertitel-Profi-Politur (CPS-Warnung, Split/Merge, Font-Auswahl) ====
Ziel: Den Untertitel-Editor auf Profi-Untertitelungs-Standards heben: Zeichen-pro-Sekunde-Warnung,
Segment teilen/zusammenfuehren, und die bereits im Datenmodell vorhandene Schriftfamilien-Auswahl im Panel
sichtbar machen.
Fertig wenn: Pro Segment ein CPS-/Zeichen-Indikator (rot oberhalb Schwelle, z. B. >17 CPS / >42 Zeichen pro
Zeile). Commands subtitle.split (am Playhead) und subtitle.merge (mit Nachbarn). Font-Familie + Gewicht im
Untertitel-Panel auswaehlbar (Feld existiert, ist nur nicht exponiert).
Anker: struct SubtitleStyle in src/core/subtitle.rs (hat bereits ein Font-Feld), Panel src/panels/subtitles.rs.
Konventionen: Commands fuer Split/Merge registrieren; deutsche UI.
Verifikation: cargo test (Split/Merge erhalten Gesamtzeit/Text); manuell CPS-Warnung.
Aufwand: S

==== G11 — Medien-Filter (Label/Typ/Verwendung) + Ordner-/rekursiver Import ====
Ziel: Den Medien-Browser von reiner Freitextsuche zu echtem Filtern erweitern (nach Farblabel, Medientyp,
"in Timeline verwendet/unbenutzt") und Ordner rekursiv importieren koennen. Heute scheitert ein Ordner-Import
an ffprobe und es gibt keine Filter.
Fertig wenn: Filterleiste im Browser (Typ Video/Audio/Bild, 8 Farblabels, Verwendung). Ordner-Drop oder
"Ordner importieren" scannt rekursiv nach unterstuetzten Endungen und importiert nur Dateien.
Anker: src/services.rs (fn import_paths, VIDEO_EXT/AUDIO_EXT/IMAGE_EXT, detect_kind); Browser in
src/panels/media_browser.rs; Verwendungs-Tracking via asset_usage_count (src/core/timeline.rs).
Konventionen: Filter-UI mit Theme-Tokens; keine Format-Aenderung noetig.
Verifikation: Manuell — Ordner mit gemischten Dateien import; Filter blendet korrekt.
Aufwand: S-M

=====================================================================================
PHASE 2 — HOHE WIRKUNG (Aufwand S-M / einzelne L; schliesst KO-Kriterien)
=====================================================================================

==== G12 — RGB-/Luma-Kurven (primaere Color-Kurven) ====
Ziel: Klassische Kurven-Werkzeuge fuer die Farbkorrektur: eine Luma-Master-Kurve plus separate Kurven pro
Kanal (R/G/B). Das mit Abstand am meisten vermisste Color-Tool; Voraussetzung fuer ernsthaftes Grading.
Fertig wenn: Im Color-Panel ein Kurven-Editor (Stuetzpunkte hinzufuegen/ziehen/loeschen, monotone Spline-
Interpolation). Wirkt im GPU-Shader UND formelgleich im CPU-Pfad (Export + Scopes). Im .etron persistiert.
Bypass + Reset wie bei den restlichen Grade-Sektionen.
Anker: struct ColorGrade in src/core/grade.rs erweitern; CPU-Auswertung in fn grade_pixel/grade_buffer (am
besten Kurve in eine 1D-LUT/Tabelle vorberechnen in fn precompute -> GradeParams). GPU-Gegenstueck in
src/ui/grade_shader.rs (LUT als Uniform/Textur). Panel src/panels/color.rs. Scopes src/panels/scopes.rs
verifizieren das Ergebnis automatisch (sie laufen ueber den CPU-Pfad).
Konventionen: GPU==CPU formelgleich (zentral!). ColorGrade-Aenderung -> PROJECT_VERSION erhoehen + #[serde(default)] + Roundtrip-Test. EDITRON_TEST_GRADE um Kurven-Parameter erweitern.
Verifikation: cargo test (CPU-Kurve trifft Referenzwerte; grade_buffer == grade_pixel); Screenshot.
Aufwand: M

==== G13 — 3D-LUT-Import (.cube) und Anwendung ====
Ziel: Industrie-Standard-Looks und Kamera-Konvertierungen ueber .cube-3D-LUTs laden und anwenden
(Input-/Look-LUT-Slot pro Clip). Heute existiert kein einziger LUT-Pfad.
Fertig wenn: .cube-Parser (1D + 3D, gaengige Groessen). LUT-Slot im Color-Panel mit Datei-Auswahl + Staerke.
Anwendung per Trilinear-Sampling im GPU-Shader UND formelgleich im CPU-Pfad. Im .etron wird der LUT-Pfad
(plus Staerke) referenziert; fehlende LUT -> Offline-Hinweis wie bei Medien.
Anker: GPU src/ui/grade_shader.rs (3D-LUT-Textur), CPU src/core/grade.rs (grade_pixel/grade_buffer),
Panel src/panels/color.rs. Position in der Pipeline (vor/nach Lift-Gamma-Gain) klar festlegen.
Konventionen: GPU==CPU formelgleich. .etron-Version erhoehen. Offline/Relink-Muster fuer fehlende LUT.
Verifikation: cargo test (Identitaets-LUT veraendert nichts; bekannte LUT trifft Referenz); Screenshot.
Aufwand: M

==== G14 — Form-Masken mit Feather (pro Effekt und pro Clip) ====
Ziel: Geometrische Masken (Ellipse, Rechteck, Polygon; spaeter Bezier-Pen) mit weicher Kante, die einen
Effekt oder Grade auf eine Region begrenzen — invertierbar, mehrere Masken kombinierbar. Groesste einzelne
Effekt-Luecke; blockiert Alltagsaufgaben (Gesicht verpixeln, partielle Korrektur, Compositing-Cutout).
Fertig wenn: Maskendaten haengen an EffectInstance (bzw. am Grade); Maske wird im Monitor editierbar
angezeigt (Handles). Maske moduliert die Effekt-/Grade-Anwendung mit Feather. Wirkt im GPU- UND CPU-Pfad.
Mehrere Masken pro Effekt, jeweils invertierbar/addierbar.
Anker: struct EffectInstance in src/core/effects.rs (Maskenfeld ergaenzen); per-Pixel-Anwendung in
fn per_pixel (normierte UVs liegen vor) und im GPU-Shader src/ui/fx_shader.rs; weicher Rand analog
vorhandener soft_edge/Feather-Helfer. Editier-Gizmo analog src/panels/transform_gizmo.rs.
Konventionen: GPU==CPU formelgleich. .etron-Version erhoehen + #[serde(default)] + Roundtrip-Test.
Maskeneditor als Command-getriebenes Werkzeug. EDITRON_TEST-Flag fuer einen maskierten Effekt.
Verifikation: cargo test (Maske begrenzt Effekt korrekt, Feather-Rampe); Screenshot maskierter Blur.
Aufwand: M-L

==== G15 — Blend-Modi im Compositor (Multiply/Screen/Overlay/Add/...) ====
Ziel: Ebenen-Mischmodi ueber das reine Src-over hinaus. Schaltet ein grosses kreatives Feld frei
(Lichteffekte, Texturen, Doppelbelichtungen) bei sehr geringem Aufwand relativ zur Wirkung.
Fertig wenn: Pro Clip ein BlendMode-Enum (Normal/Multiply/Screen/Overlay/Add/Darken/Lighten/SoftLight/...).
Die Mischformel an der zentralen Compositing-Stelle waehlt nach Modus; GPU-Compositing im Monitor zieht
formelgleich nach. Auswahl im Effekt-/Clip-Inspektor.
Anker: EINZIGE Mischstelle ist fn composite_band in src/core/compose.rs (heute dst = src*a + dst*(1-a)).
GPU-Gegenstueck im Monitor-Compositing (src/panels/monitor.rs / src/ui/fx_shader.rs). Clip-Feld am
TimelineClip (src/core/timeline.rs).
Konventionen: GPU==CPU formelgleich. .etron-Version erhoehen + #[serde(default)].
Verifikation: cargo test (Multiply/Screen treffen Referenzformeln); Screenshot zweier Layer.
Aufwand: M

==== G16 — Replace Edit (+ Fit-to-Fill / echtes 4-Punkt) ====
Ziel: Quellmaterial in einen vorhandenen Timeline-Clip einsetzen, dabei Dauer UND Position erhalten
(Replace), mit Match-Frame-Sync; plus Fit-to-Fill (4-Punkt), das die Geschwindigkeit so setzt, dass die
Quell-Range die Ziel-Range fuellt. Eine der meistgenutzten Profi-Operationen; aktuell nur Warnung statt
echtem 4-Punkt.
Fertig wenn: Command timeline.replace (ersetzt den Clip unter dem Playhead/Selektion durch das aktuelle
Quellmaterial, Dauer/Position bleiben, Sync via Match-Frame). Command timeline.fitToFill (4-Punkt: setzt
clip speed, damit src-range in target-range passt). Default-Bindings.
Anker: fn perform_source_edit und fn match_frame in src/core/edit.rs (Infrastruktur fuer Three-Point ist
da; 4-Punkt wird dort heute nur als Warnung behandelt). fn set_clip_speed in src/core/timeline.rs fuer
Fit-to-Fill.
Konventionen: Commands + Bindings (Replace = z. B. F in Premiere/Resolve-Preset).
Verifikation: cargo test (Replace haelt Dauer/Position; Fit-to-Fill-Speed exakt = src_len/target_len).
Aufwand: M

==== G17 — Speed-Ramping / Time-Remap (variable Geschwindigkeit mit Keyframes) ====
Ziel: Geschwindigkeit ueber die Clip-Dauer per Keyframes variieren (Time-Remap), statt nur konstantem
Faktor. Sehr verbreitetes Feature (FCP/Premiere Time Remap, Resolve Retime-Kurve).
Fertig wenn: Das Speed-Feld wird animierbar; die Medienzeit-Abbildung integriert die Speed-Kurve ueber die
Zeit (statt konstant zu multiplizieren). Keyframe-Editor zeigt die Speed-Kurve; Audio bleibt pitch-korrekt
bzw. stumm wie bei statischer Speed. Ripple/Dauer-Kopplung bleibt konsistent.
Anker: pub speed: f64 in struct TimelineClip (src/core/timeline.rs) -> auf AnimatedParam heben; Abbildung in
fn media_time_at und fn eff_speed muss die Kurve integrieren. Keyframe-System (struct AnimatedParam,
enum Interp in src/core/animation.rs) wiederverwenden. Keyframe-Editor src/panels/effect_controls.rs.
Konventionen: .etron-Version erhoehen + #[serde(default)] (alte feste speed migrieren); Audio-Pitch-Pfad
(atempo) wie bei statischer Speed. EDITRON_TEST_SPEED ggf. um Kurve erweitern.
Verifikation: cargo test (integrierte Medienzeit stimmt an Stuetzpunkten; Roundtrip).
Aufwand: M-L

==== G18 — LUFS- / True-Peak-Metering im Mixer ====
Ziel: Profi-Lautheits-Metering im Audio-Mixer: Integrated/Short/Momentary LUFS (BS.1770 K-Weighting +
Gating) und echtes True-Peak (Oversampling). Ergaenzt G05 — man sieht beim Mischen, was beim Export rauskommt.
Heute nur Sample-Peak.
Fertig wenn: Loudness-Meter im Mixer (Integrated/Short/Momentary + dBTP), gespeist aus dem vorhandenen
Mix-Block-Pfad. RMS-/True-Peak korrekt. Optional Ziel-Linie (-14/-23 LUFS).
Anker: Mixdown-Block im Player (src/core/player.rs, drive_audio/mix_block) liefert die Samples; Meter-State
in src/stores.rs (AudioStore, track_levels/master_level). Panel src/panels/audio_mixer.rs.
Konventionen: DSP blockgroessen-invariant (wie src/core/audio_fx.rs). Deutsche Labels.
Verifikation: cargo test (BS.1770-Referenzsignal trifft erwartetes LUFS); manuell.
Aufwand: M

==== G19 — Auto-Ducking + De-Esser ====
Ziel: Zwei Alltags-Cleanup-Tools, die direkt auf der vorhandenen DSP-Kette aufsetzen: Auto-Ducking (Musik
unter Sprache via Sidechain) und De-Esser (frequenzselektive Kompression).
Fertig wenn: Auto-Ducking = der vorhandene Kompressor mit Sidechain-Eingang (Key = andere Spur/Clip),
konfigurierbar. De-Esser = EQ-Detektor (Zisch-Band) steuert Gain-Reduktion. Beide als Effekte im Katalog,
Player UND Export formelgleich.
Anker: src/core/audio_fx.rs (Kompressor existiert mit Pegelfolger; Sidechain-Eingang ergaenzen). Effekt-UI
src/panels/effect_controls.rs (EQ-Kurve/GR-Meter sind dort schon).
Konventionen: EINE blockgroessen-invariante DSP-Quelle fuer Player+Export (wie alle Audio-Effekte).
Verifikation: cargo test (Ducking senkt Pegel bei Key-Signal; De-Esser greift nur im Zisch-Band).
Aufwand: S-M

==== G20 — Adjustment Layers (Einstellungsebenen) ====
Ziel: Ein spezieller Clip-Typ ohne eigenes Medium, der Effekte/Grade auf ALLE darunterliegenden Clips
anwendet. Standard fuer "ein Grade/Effekt ueber mehrere Clips".
Fertig wenn: Neuer Clip-Typ "Adjustment Layer" (wie Titel/Untertitel ein synthetischer Clip), traegt
ClipFx + ColorGrade, und der Compositor wendet ihn als Pass auf das zusammengesetzte Bild der darunter
liegenden Spuren an — im Player UND im Export gleich.
Anker: Synthetische Clips als Vorbild: src/core/title.rs / TrackKind. Compositing in
src/core/compose.rs (composite_sequence_frame). Grade/FX-Anwendung wie bei normalen Clips.
Konventionen: .etron-Version erhoehen + #[serde(default)]. GPU==CPU formelgleich. Als Command einfuegbar.
Verifikation: cargo test (Adjustment ueber 2 Clips faerbt beide); Screenshot.
Aufwand: M

==== G21 — Archive / Consolidate + portable (relative) Medienpfade ====
Ziel: Ein Projekt samt benutzter Medien einsammeln und in einen Zielordner kopieren ("Consolidate"),
optional auf benutzte Bereiche getrimmt, mit relativen/portablen Pfaden — damit Projekte uebergebbar und
archivierbar sind. Heute nur absolute Pfade -> verschieben = alles offline.
Fertig wenn: Command/Dialog "Projekt konsolidieren": kopiert alle (oder nur benutzte/getrimmte) Medien in
<ziel>/media, schreibt das .etron mit relativen Pfaden, relinkt. Beim Laden werden relative Pfade relativ
zur Projektdatei aufgeloest.
Anker: Medienimport/-pfade in src/services.rs (import_paths, import_one); Render-Plan kennt benutzte Ranges
(src/core/export.rs, render_segments/RenderPlan); ffmpeg-Dispatcher fuer Trim-Kopien vorhanden. Pfad-Logik
in src/core/project.rs (Speichern/Laden).
Konventionen: Relative-Pfad-Option ins Modell -> PROJECT_VERSION erhoehen + #[serde(default)].
Verifikation: Konsolidieren, Projektordner verschieben, oeffnen -> alles online; cargo test fuer Pfadlogik.
Aufwand: M-L

==== G22 — Auto-Transkription / Auto-Captions (Speech-to-Text) ====
Ziel: Audio automatisch transkribieren und als getimte Untertitel-/Caption-Segmente erzeugen
(Whisper-Klasse). Heute ist der "Transkript-Workflow" reines manuelles Tippen — das ist die schmerzhafteste
Untertitel-Luecke gegenueber Premiere/Resolve/FCP.
Fertig wenn: Command "Auto-Transkription": extrahiert Audio (ffmpeg), laesst whisper.cpp (oder vergleichbar)
asynchron laufen, schreibt Cues in eine Untertitel-Spur. Fortschritt/Cancel wie beim Proxy-Workflow.
Sprache waehlbar.
Anker: Async-Service-Muster aus src/core/proxy.rs + src/services.rs (begrenzte Parallelitaet, -progress)
uebernehmen. Audio-Extraktion via ffmpeg. Ziel = bestehende Subtitle-Spur (src/core/subtitle.rs).
Konventionen: Externe Binary optional/konfigurierbar (Pfad in AppSettings, src/core/settings.rs); deutsche UI.
Verifikation: Manuell mit kurzem Clip -> plausible getimte Segmente.
Aufwand: L

==== G23 — Bild-Sequenz als Quelle importieren (%0Nd) ====
Ziel: Eine nummerierte Frame-Folge (z. B. EXR/DPX/PNG render_0001.png ...) beim Import als EINEN Clip
erkennen statt als hunderte Einzelbilder. Pflicht fuer VFX-Renders.
Fertig wenn: Beim Drop/Import eines Frames einer Folge wird die Sequenz erkannt (%0Nd), als ein Asset mit
der Bildrate der Sequenz importiert und ueber den vorhandenen Bildpfad dekodiert.
Anker: Import in src/services.rs (detect_kind, import_paths). Bild-Handling (Default-Dauer) in
src/core/timeline.rs. ffmpeg unterstuetzt image2-Sequenzen nativ.
Konventionen: keine Format-Aenderung zwingend; Asset-Typ ggf. markieren.
Verifikation: Ordner mit nummerierten Frames -> ein Clip; Wiedergabe laeuft.
Aufwand: M

==== G24 — MXF-Export + Broadcast-Delivery-Presets ====
Ziel: Sendeserver-taugliche Master schreiben: MXF-Container (OP1a, z. B. XDCAM HD422 / ProRes-MXF /
DNxHD-MXF). Heute ist .mxf nur Import-Endung, kein Export-Muxer.
Fertig wenn: MXF als Export-Container mit passenden Codec-Profilen; Delivery-Presets (Broadcast 1080i/p)
inkl. korrektem Color-Tagging. Validierung der Codec/Container-Kombination.
Anker: Codec-/Container-Tabellen und fn video_codec_args in src/core/export.rs; Container-Validierung dort;
Export-PRESETS-Liste. Dialog src/overlays/export_dialog.rs.
Konventionen: Deutsche Preset-Namen; Hardware-Encoder-Filter wie bestehend (ffmpeg -encoders).
Verifikation: Export -> ffprobe bestaetigt MXF/Codec/Color-Tags.
Aufwand: M

==== G25 — Audio-Scopes (Goniometer + Phasenkorrelation; optional Spektrum) ====
Ziel: Mono-Kompatibilitaet und Phase pruefen — Goniometer (Lissajous) und Korrelationsmeter; optional
Spektrum/Spektrogramm. Pflicht-Check vor jedem Delivery; das Scopes-Panel ist heute video-only.
Fertig wenn: Audio-Modus im Scopes-Panel: Goniometer + numerischer Korrelationswert (-1..+1) aus dem
Mixdown-Stereo-Signal. Spektrum optional als zweiter Schritt.
Anker: src/panels/scopes.rs (heute Waveform/Parade/Vektorskop/Histogramm, video-only). Stereo-Samples aus
dem Player-Mixdown (src/core/player.rs) bzw. AudioStore (src/stores.rs).
Konventionen: Software-Plot wie die Video-Scopes (kein GPU-Readback-Stall).
Verifikation: Mono-Signal -> Korrelation ~+1, Out-of-Phase -> ~-1; manuell.
Aufwand: S-M

==== G26 — Two-Up-Trim-Vorschau im Programmmonitor ====
Ziel: Beim Roll-/Ripple-/Slip-Trim zwei Bilder nebeneinander zeigen (ausgehender + eingehender Frame der
Schnittkante). Ohne dieses visuelle Feedback ist Profi-Trimming "blind".
Fertig wenn: Waehrend einer Trim-Geste schaltet der Programmmonitor auf ein Zwei-Bild-Layout (links
Ende-des-linken-Clips, rechts Anfang-des-rechten-Clips), mit Frame-Offset-Anzeige. Danach zurueck zur
Normalansicht.
Anker: Monitor-Rendering src/panels/monitor.rs (zwei Decode-Targets); Trim-Gesten in src/panels/timeline.rs
(Roll/Ripple/Slip-Drag). Dekoder fuer zwei Frames ist vorhanden.
Konventionen: Nur UI-State, keine Format-Aenderung.
Verifikation: EDITRON_TEST_TOOL + Drag -> Screenshot Zwei-Bild-Layout.
Aufwand: M-L

==== G27 — Verlauf-Fuellung + Glow fuer Text ====
Ziel: Modernen Titel-Look ermoeglichen: Verlaufs-Fuellung (statt Volltonfarbe) und Glow. Engine ist
maskenbasiert und blockweise — gut erweiterbar.
Fertig wenn: TitleSpec unterstuetzt eine Verlaufs-Fuellung (2+ Stops, Winkel) und einen Glow (gefaerbter
Blur der Fuellmaske vor der Fuellung). Beides im Inspektor einstellbar, im gemeinsamen Rasterizer (Monitor
== Export) gerendert.
Anker: struct TitleSpec in src/core/title.rs; Rasterizer fn render_title in src/core/text_raster.rs (hat
schon Outline/Schatten/Box-Maskenpaesse). Panel src/panels/graphics.rs.
Konventionen: Ein Rasterpfad fuer Monitor+Burn-in (Paritaet!). .etron-Version erhoehen + #[serde(default)].
Verifikation: cargo test (Raster auflösungs-konsistent); Screenshot Verlaufstext + Glow.
Aufwand: M

==== G28 — Sub-Clips ====
Ziel: Aus langem Quellmaterial benannte Teilstuecke (Sub-Clips) anlegen — Doku-/Reality-Workflow.
Fertig wenn: Aus In/Out im Quellmonitor ein benannter Sub-Clip im Bin erstellbar, der auf dasselbe Asset
mit begrenzter Range verweist; verhaelt sich beim Einfuegen wie ein eigenes Asset.
Anker: Quellmonitor In/Out (src/panels/monitor.rs, SourceMonitorPanel); Asset-/Bin-Modell src/core/bin.rs;
Range-begrenzte Referenz auf ein MediaAsset.
Konventionen: .etron-Version erhoehen + #[serde(default)] + Roundtrip-Test. Als Command.
Verifikation: cargo test (Sub-Clip-Range bleibt erhalten); manuell.
Aufwand: M

=====================================================================================
PHASE 3 — STRATEGISCH (Aufwand L-XL; Profi-Tiefe & Differenzierung)
=====================================================================================

==== G29 — HSL-Qualifier (sekundaere Farbkorrektur per Farbauswahl) ====
Ziel: Eine Farbe per Pipette selektieren (Hue/Sat/Lum-Range mit Weichheit) und nur diesen Bereich
korrigieren — der Einstieg in Resolve-Sekundaerkorrektur.
Fertig wenn: Pipette im Programmmonitor erzeugt eine HSL-Range-Maske; ein zweiter Korrektur-Block wirkt nur
in dieser Maske. GPU == CPU formelgleich. Maske visualisierbar (Highlight).
Anker: ColorGrade/Pipeline in src/core/grade.rs (zweiter, maskierter Korrekturblock); GPU
src/ui/grade_shader.rs; Pipette-Infrastruktur im Monitor (src/panels/monitor.rs). Verknuepfbar mit G14-Masken.
Konventionen: GPU==CPU formelgleich. .etron-Version erhoehen.
Verifikation: cargo test (nur selektierte Huerange wird veraendert); Screenshot.
Aufwand: L

==== G30 — Power Windows (Kreis/Linear-Verlauf/Polygon) + Maskentracking ====
Ziel: Lokales Grading per geometrischer Maske mit Verlauf, plus mitlaufendes Tracking. Resolve-Kern-Workflow.
Fertig wenn: Grade-gebundene Maske (Kreis/Verlauf/Polygon) mit Feather begrenzt die Korrektur; Maske ist
ueber die Zeit keyframe-/track-bar. GPU == CPU.
Anker: Erweiterungspunkt fuer rechteckige Masken existiert in src/core/compose.rs; Grade-Pfad src/core/grade.rs.
Setzt G14 (Masken) und idealerweise G31 (Tracking) voraus.
Konventionen: GPU==CPU formelgleich. .etron-Version erhoehen.
Verifikation: cargo test (Window begrenzt Grade); Screenshot.
Aufwand: L

==== G31 — Motion-Tracking (Punkt + Planar) ====
Ziel: Bewegung im Bild verfolgen und das Ergebnis auf Maske/Effekt/Text anwenden (z. B. bewegtes Objekt
verpixeln, Schild ersetzen).
Fertig wenn: Tracker (Punkt, spaeter planar) erzeugt Keyframe-Transformdaten; diese sind auf Masken/Effekte/
Titel anwendbar. Vorwaerts/rueckwaerts trackbar; Korrektur einzelner Keyframes moeglich.
Anker: Keyframe-System (src/core/animation.rs); Anwendung auf ClipFx/Masken. CPU-Tracking-Algorithmus neu
(z. B. NCC/Lucas-Kanade auf den dekodierten Frames aus src/core/player.rs).
Konventionen: Ergebnis als animierte Parameter; .etron-Version erhoehen.
Verifikation: cargo test auf synthetischer Bewegung; manuell.
Aufwand: L

==== G32 — Warp-Stabilizer ====
Ziel: Verwackeltes Material glaetten (Analyse der Kamerabewegung + Gegen-Transform). Standarderwartung.
Fertig wenn: Stabilisierungs-Effekt analysiert den Clip (asynchron, Fortschritt), berechnet eine geglaettete
Transform-Kurve und wendet sie an (mit Crop/Rand-Optionen). Im Player UND Export gleich.
Anker: Frame-Zugriff via src/core/player.rs; Anwendung als Transform (compose.rs/animation.rs);
Async-Analyse-Muster wie Proxy (src/core/proxy.rs/services.rs).
Konventionen: GPU==CPU fuer die Anwendung; Analyse cachebar.
Verifikation: Verwackelter Test -> sichtbar ruhiger; manuell.
Aufwand: L

==== G33 — AAF-Export (Audio-Schnitt + Pegel) fuer Pro-Tools-Handoff ====
Ziel: AAF schreiben, damit der Schnitt (mind. Clip-Positionen + Lautstaerke-Automation) an Pro Tools/Avid
uebergeben werden kann. Einziges echtes KO-Kriterium fuer ernsthafte Audio-Post.
Fertig wenn: Export einer Sequenz als AAF mit Clip-Positionen, Quellreferenzen und Volume-Automation;
"keine stillen Verluste" wie bei der bestehenden Interop (Unabbildbares wird gemeldet).
Anker: Interop-IR struct InteropTimeline und das Report-Muster in src/core/interop/ (mod.rs, +
overlays/interop_report.rs). AAF als neue Variante analog otio.rs/edl.rs/fcpxml.rs. KEINE neue Dependency,
falls machbar (sonst minimaler AAF-Writer).
Konventionen: Datei-Menue-Untermenue + Command sequence.export.aaf; Fixtures in tests/fixtures/.
Verifikation: AAF in Pro Tools/Avid bzw. einem AAF-Validator oeffnen; cargo test fuer Frame-Genauigkeit.
Aufwand: L

==== G34 — Surround- / Multichannel-Audio (5.1/7.1) ====
Ziel: Mehrkanal-Audio durchgaengig: Quellen, Mixer-Routing, Player-Bus und Export-Mapping. Heute ist alles
hart auf Stereo geklemmt (-ac 2, clamp(1,2)) -> Film/Broadcast-Delivery unmoeglich.
Fertig wenn: Projekt-/Sequenz-Kanal-Layout (Stereo/5.1/7.1); Mixer mit Surround-Panning (Pan-Law);
Player-Mixdown und Export mappen korrekt auf das Layout; Per-Clip-Kanal-Konfiguration (Quellkanal -> Ziel).
Anker: Greift durch: src/core/player.rs (drive_audio Mixdown), src/panels/audio_mixer.rs (Panning),
src/core/export.rs (-ac/-channel_layout, AUDIO_CODECS), Modell in src/core/timeline.rs.
Konventionen: .etron-Version erhoehen + #[serde(default)]. DSP blockgroessen-invariant.
Verifikation: cargo test (Kanal-Mapping); Export -> ffprobe bestaetigt Layout.
Aufwand: L

==== G35 — Bezier-Keyframes + Value-Graph-Editor ====
Ziel: Feine Eases (Bezier-Tangenten, Overshoot/Anticipation) und Geschwindigkeitskontrolle ueber einen
Value-Graph-Editor — hebt Animation von "Schnittprogramm" auf Motion-Graphics-Niveau. Heute ist Easing fest.
Fertig wenn: enum Interp bekommt Bezier{in,out}-Tangenten; der Keyframe-Editor hat einen Graph-Modus
(Kurven sichtbar, Tangenten ziehbar). Wirkt auf alle animierten Parameter.
Anker: enum Interp und struct AnimatedParam in src/core/animation.rs (apply erweitern); Keyframe-Editor in
src/panels/effect_controls.rs.
Konventionen: .etron-Version erhoehen + #[serde(default)] (lineare/Ease-Keys migrieren).
Verifikation: cargo test (Bezier trifft Referenzkurve); Screenshot Graph-Editor.
Aufwand: M

==== G36 — Mehrteilige Titel + Per-Run-Styling ====
Ziel: Mehrere unabhaengig positionierbare Textboxen pro Titel und unterschiedliche Styles pro Textabschnitt
(ein Wort fett/farbig). Voraussetzung fuer echte Bauchbinden und fuer MOGRT (G37). Heute = ein String, ein Stil.
Fertig wenn: TitleSpec haelt mehrere TextBoxen (je eigene Position/Stil) und Style-Runs innerhalb eines
Texts. In-Canvas-Editor und Inspektor unterstuetzen das. Rasterizer rendert mehrere Boxen/Runs.
Anker: struct TitleSpec in src/core/title.rs (von einem Text/Stil auf Vec<TextBox> + Runs heben); Rasterizer
fn render_title in src/core/text_raster.rs (ist blockweise/maskenbasiert); In-Canvas-Editor
src/panels/title_editor.rs; Inspektor src/panels/graphics.rs.
Konventionen: .etron-Version erhoehen + #[serde(default)] (alte Titel migrieren). Monitor==Export-Paritaet.
Verifikation: cargo test (Raster konsistent); Screenshot Bauchbinde mit zwei Stilen.
Aufwand: L

==== G37 — MOGRT-aequivalente Titel-Vorlagen-Bibliothek ====
Ziel: Titel als parametrische Vorlage speichern/laden (benannte Parameter) und in einer Bibliothek
verwalten — Team-Workflow fuer Marken-Bauchbinden. Heute sind Vorlagen 4 hartkodierte Rust-Fabriken.
Fertig wenn: Aktueller Titel als .etron-Vorlage mit benannten Parametern speicherbar; Vorlagen-Panel zum
Durchsuchen/Anwenden; angewandte Vorlage behaelt editierbare Parameter. Baut auf G36 auf.
Anker: enum TitleTemplate / build in src/core/title.rs; Vorlagen als serde-Strukturen ablegen
(Verzeichnis konfigurierbar in src/core/settings.rs). Panel-Erweiterung src/panels/graphics.rs.
Konventionen: Vorlagen-Format versioniert; deutsche UI.
Verifikation: Vorlage speichern, neu anwenden, Parameter aendern; cargo test fuer Roundtrip.
Aufwand: L (M, wenn nur Speichern/Laden ohne Galerie)

==== G38 — Shape- / Vektor-Grafik-Clips (Rechteck/Ellipse/Linie/Pen) ====
Ziel: Eigenstaendige Vektor-Grafikelemente (Trennlinien, Akzentbalken, Logohintergruende) ohne PNG-Import.
Fertig wenn: Neuer Grafik-Clip-Typ (analog Titel) mit Form, Fuellung, Kontur; durch denselben Layer-/
Compositing-Pfad gerendert; im Monitor editierbar.
Anker: Synthetischer Clip wie Titel (src/core/title.rs / TrackKind); Rendering ueber den gemeinsamen
Compositing-Pfad (src/core/compose.rs); Gizmo analog src/panels/transform_gizmo.rs.
Konventionen: .etron-Version erhoehen + #[serde(default)]. Monitor==Export-Paritaet.
Verifikation: cargo test (Form-Raster); Screenshot.
Aufwand: L

==== G39 — HDR-/10-bit-Vorschau + Color Management ====
Ziel: Verlaessliches HDR-/Wide-Gamut-Monitoring und 10-bit-Anzeige. Export ist bereits 10-bit-faehig, die
Vorschau bleibt aber 8-bit RGBA ohne korrektes Farbmanagement -> Grading "blind".
Fertig wenn: Vorschau-Pipeline mit hoeherer Bittiefe + Color-Management (Rec.709/Rec.2020/PQ/HLG, korrektes
Tone-Mapping fuer SDR-Displays, Blending in Linearlicht). Optional Display-Farbraum-Auswahl.
Anker: Vorschau-Pfad src/core/player.rs + GPU src/ui/fx_shader.rs/grade_shader.rs; HDR-Tagging/Tonemap
existiert in src/core/export.rs (OutputColor) und im Player als Referenz fuer die Mathematik.
Konventionen: GPU==CPU formelgleich; an die bestehende HDR-Tagging-Logik anschliessen.
Verifikation: HDR-Quelle -> korrektes Tone-Mapping in der Vorschau; Vergleich mit Export.
Aufwand: L

==== G40 — Optical-Flow-Frame-Interpolation fuer Zeitlupe ====
Ziel: Weiche statt ruckelnde Slow-Motion durch bewegungskompensierte Zwischenbilder. Macht G17
(Speed-Ramping) broadcast-tauglich.
Fertig wenn: Optionaler Retime-Modus "Optical Flow", der zwischen Quellframes interpoliert (Flow-Schaetzung

- Warp), im Player (ggf. gecacht) UND Export.
  Anker: Medienzeit-Abbildung fn media_time_at (src/core/timeline.rs) liefert sub-Frame-Positionen; Flow/Warp
  neu; Frame-Cache (src/core/frame_cache.rs) fuer Zwischenframes nutzen.
  Konventionen: Ergebnis deterministisch (Export==Vorschau, sofern gecacht).
  Verifikation: Zeitlupe sichtbar weicher; manuell.
  Aufwand: L

==== G41 — Adaptive Wiedergabe-Aufloesung bei Ueberlast ====
Ziel: Bei steigender Dropped-Frame-Rate die Vorschau-Aufloesung automatisch senken (1/2, 1/4) und bei
Entlastung wieder anheben — aus "solide" wird echtes smooth playback. Heute ist program_scale statisch.
Fertig wenn: Regelung beobachtet die Drop-Telemetrie und passt die Vorschau-Skala dynamisch an; per Setting
abschaltbar; sichtbarer Hinweis auf reduzierte Aufloesung.
Anker: Drop-Telemetrie (DriveCtx.drops / drops_recent in src/core/player.rs); Preview-Scales
(PREVIEW_SCALES in src/stores.rs, default_preview_scale). Reine Steuerlogik.
Konventionen: Setting in AppSettings (src/core/settings.rs).
Verifikation: Schwere Sequenz bleibt fluessig; manuell + Perf-Overlay.
Aufwand: M

==== G42 — Spatial-Index fuer Clips (lange Timelines skalierbar) ====
Ziel: O(log n)-Zugriff auf "Clips, die [t0,t1] ueberlappen", statt linearer Scans. Bei stundenlangen
Timelines mit hunderten/tausenden Clips kostet der flache Vec pro Frame/Edit spuerbar Latenz.
Fertig wenn: Ein Intervall-Index (z. B. sortiert/Intervall-Baum) pro Spur beschleunigt Compositing,
range_signature und complex_spans; Ergebnisse identisch zu vorher, nur schneller.
Anker: Flacher Vec<TimelineClip> + pervasive iter().filter/find in src/core/timeline.rs; Verbraucher:
compositing (src/core/compose.rs), src/core/render_cache.rs (range_signature, complex_spans).
Konventionen: Reine interne Optimierung, kein Verhaltens-/Formatwechsel; durch bestehende Tests abgesichert.
Verifikation: cargo test (gleiche Ergebnisse); Benchmark mit synthetischer Langzeit-Timeline.
Aufwand: M

==== G43 — Monitor-Profi-Overlays (Zebra, Focus-Peaking, Before/After-Wipe, Safe-Zonen) ====
Ziel: Visuelles Belichtungs-/Fokus-Feedback und Grade-Vergleich. Heute nur fixe Safe-Margins.
Fertig wenn: Zebra (Belichtungswarnung ueber Schwelle), Focus-Peaking (Kantenhervorhebung), ein
Before/After-Split/Wipe-Modus fuer den Grade, und anpassbare Safe-Zonen/Grid. Je als GPU-Overlay-Pass.
Anker: src/panels/monitor.rs (fn draw_safe_margins, Wipe-Compositing existiert fuer Transitions als Vorbild);
enum MonitorView in src/stores.rs (Variante Compare ergaenzen). GPU src/ui/fx_shader.rs.
Konventionen: Overlays per Command toggelbar; Theme-Tokens.
Verifikation: Screenshots (Zebra auf Ueberbelichtung, Before/After-Split).
Aufwand: M

==== G44 — Vollbild- / Zweitmonitor-Wiedergabe (SDI optional/spaeter) ====
Ziel: Cinema-Preview im Vollbild bzw. auf einem zweiten Monitor. (SDI/Decklink bewusst zurueckgestellt —
Aufwand L, Nischenbedarf.)
Fertig wenn: Command fuer Borderless-Vollbild der Programm-Wiedergabe, optional auf einem zweiten Monitor;
sauberes Zurueckschalten.
Anker: Fenster-/Mainloop in src/main.rs; Programm-Rendering src/panels/monitor.rs.
Konventionen: Command + Binding.
Verifikation: Manuell — Vollbild an/aus, korrekte Aufloesung.
Aufwand: S (SDI separat L)

==== G45 — Voiceover- / Punch-In-Aufnahme ====
Ziel: Kommentar/VO direkt im Programm auf eine Audiospur aufnehmen (Arm/Punch-In). Heute kein Input-Pfad.
Fertig wenn: Audiospur "scharf schalten", Eingang waehlen, bei Wiedergabe punch-in aufnehmen; Aufnahme landet
als Clip auf der Spur; Pegel-Monitoring.
Anker: Audio-Input-Stream neu (raylib/cpal); Arm/Record-State; Ziel = Audiospur (src/core/timeline.rs);
Mixdown/Monitoring src/core/player.rs.
Konventionen: Geraet/Buffer in AppSettings (src/core/settings.rs). Als Command.
Verifikation: Aufnahme -> Clip mit korrekter Laenge/Inhalt; manuell.
Aufwand: M

==== G46 — Node-/Layer-basiertes Grading (langfristig, optional) ====
Ziel: Nicht-destruktiver Stack mehrerer Korrekturschritte mit Reihenfolge-Kontrolle (serielle/parallele/
Layer-Nodes) — Resolve-Signature. Optionales Fernziel; Lumetri-Paritaet ist auch ohne Nodes erreichbar.
Fertig wenn: ColorGrade wird zu einem geordneten Stack von Grade-Nodes; Pipeline wertet sie der Reihe nach
aus (GPU == CPU); Panel zeigt/ordnet die Nodes.
Anker: struct ColorGrade -> Node-Liste in src/core/grade.rs; Pipeline grade_pixel/grade_buffer + GPU
src/ui/grade_shader.rs; Panel src/panels/color.rs.
Konventionen: GPU==CPU formelgleich. .etron-Version erhoehen + Migration des bisherigen Single-Grade.
Verifikation: cargo test (Node-Reihenfolge wirkt korrekt); Screenshot.
Aufwand: L-XL

==== G47 — Smart Bins + Keywords/Ratings ====
Ziel: Kriterienbasierte Auto-Organisation (Smart Bins/Collections) und Freitext-Keywords + Sterne/Reject
zusaetzlich zu den 8 Farblabels. Hebt Sichtungs-/Logging-Workflows auf Resolve/FCP-Niveau.
Fertig wenn: Smart Bins, die Assets nach Regeln (Typ/Label/Keyword/Rating/Verwendung) automatisch fuellen;
Keyword-Tags + Rating pro Asset; Filterbar (siehe G11).
Anker: Bin-/Asset-Modell src/core/bin.rs (MediaLabel existiert); Browser src/panels/media_browser.rs.
Konventionen: .etron-Version erhoehen + #[serde(default)].
Verifikation: cargo test (Smart-Bin-Regel waehlt korrekt); manuell.
Aufwand: M

==== G48 — Scene-Cut-Detection ====
Ziel: Zusammenschnitte automatisch an Szenenwechseln zerschneiden (oder Marker setzen).
Fertig wenn: Command "Szenenschnitte erkennen" analysiert einen Clip (ffmpeg select=scene o. ae.) und legt
Schnitte/Marker an den erkannten Grenzen an; Schwellwert einstellbar.
Anker: ffmpeg-Dispatcher src/services.rs; Schnitt-/Marker-Anlage in src/core/timeline.rs / src/core/marker.rs.
Konventionen: Async-Fortschritt wie Proxy; als Command.
Verifikation: Test-Schnitt mit harten Cuts -> Schnitte an den richtigen Stellen; manuell.
Aufwand: M

==== G49 — Auto / Smart Reframe (Seitenverhaeltnis-Konvertierung mit Motiv-Tracking) ====
Ziel: 16:9 -> 9:16/1:1 mit automatischem Nachfuehren des Bildinhalts (Saliency/Tracking) — fast Pflicht fuer
Social-Delivery. Heute nur statisches Crop/Pad.
Fertig wenn: Effekt/Operation, die je Frame einen Crop-Fokus aus Saliency/Bewegung waehlt und als animierte
Transform anwendet; manuelle Korrektur moeglich; im Player UND Export gleich.
Anker: Frame-Analyse via src/core/player.rs; Anwendung als animierte ClipFx-Transform (compose.rs/animation.rs);
Async-Analyse wie Proxy.
Konventionen: GPU==CPU fuer die Anwendung; .etron-Version erhoehen.
Verifikation: Querformat -> Hochformat, Motiv bleibt im Bild; manuell.
Aufwand: L

==== G50 — Kollaboration / Shared Projects (langfristig) ====
Ziel: Team-Workflow ohne stilles Ueberschreiben — mind. Locking/Merge, langfristig Multi-User. Heute
Last-Write-Wins.
Fertig wenn (Stufe 1): Erkennung paralleler Aenderungen an derselben .etron (mtime/Lockfile), Warnung statt
stillem Ueberschreiben; optional Sequenz-granulares Mergen. Spaeter: echtes Shared-Project-Modell.
Anker: Speicherpfad src/core/project.rs; Autosave/Recovery-Muster src/core/autosave.rs.
Konventionen: Konservativ — niemals fremde Aenderungen verlieren.
Verifikation: Zwei Sessions, parallele Aenderung -> Warnung statt Datenverlust.
Aufwand: L (Stufe 1 M)

ENDE — 50 Goal-Prompts (G01-G50). Phase 1 = sofortiger Qualitaetssprung auf vorhandener Infrastruktur;
Phase 2 = schliesst die lautesten Profi-Luecken (Color-Kurven/LUT, Masken/Blend-Modi, Replace/Speed-Ramp,
Loudness/Stems, Transkription); Phase 3 = strategische Tiefe (Sekundaer-Color, Tracking/Stabilisierung,
AAF/Surround, HDR, Motion-Graphics).
