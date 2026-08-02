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
