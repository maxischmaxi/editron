# HiDPI auf macOS (Retina) — Test- & Implementierungsanleitung

> Diese Datei ist für die **macOS-Maschine** gedacht. Die HiDPI-Skalierung ist
> auf Linux/X11 fertig und verifiziert (zentrale Scale-Übersetzung an der
> Ui-/Input-Grenze, physikalisch gerasterte Font-Atlanten — siehe CLAUDE.md
> Abschnitt „HiDPI/UI-Skalierung"). **macOS-Retina ist bewusst NICHT aktiviert**,
> weil raylib auf `__APPLE__` an mehreren Stellen selbst mit der DPI skaliert und
> das ohne echte Retina-Hardware nicht verifizierbar ist. Diese Anleitung führt
> Schritt für Schritt durch Diagnose → Implementierung → Verifikation.
>
> **Ziel:** auf einem Retina-/4K-Mac muss Editron pixelscharf aussehen (Typografie,
> 1-px-Linien, Icons) **und** korrekte Hit-Targets haben — wie DaVinci Resolve /
> Premiere.

---

## 0. Worum es geht (Kurzfassung)

Auf **X11** gilt: `GetScreenWidth() == GetRenderWidth()` (= Fensterpixel), der
Scale kommt aus `GetWindowScaleDPI()` oder dem Override, und **Editron skaliert
ALLES selbst** (Geometrie, Scissor, Maus) zentral in `src/ui/mod.rs`.

Auf **macOS mit Retina** ist das anders: raylib skaliert mehrere Dinge **selbst**
mit der DPI (Viewport, Projektion, Scissor). Wenn Editron zusätzlich selbst
skaliert, wird **doppelt** skaliert. Deshalb braucht macOS eine eigene
Behandlung. Die Hypothese (zu verifizieren, siehe Phase 1):

| | X11 (fertig) | macOS + HIGHDPI (zu bauen) |
|---|---|---|
| Geometrie (`fill`/`text`-Position/…) | Editron × `scale` | raylib skaliert selbst ⇒ Editron × **1.0** |
| Scissor (`BeginScissorMode`) | Editron × `scale` | raylib skaliert selbst (`rcore.c:1163`) ⇒ Editron × **1.0** |
| Maus (`GetMousePosition`) | Pixel ⇒ `/ scale` | liefert Punkte (= logisch) ⇒ **nicht** teilen |
| Font-Atlas-Rasterung | `size × scale × OVERSAMPLE` | **gleich** (`size × dpi × OVERSAMPLE`) — sonst unscharf |
| Icon-Tessellation | × `scale` | **gleich** (× `dpi`) |

Anders gesagt: auf macOS müssen **Rasterung** (Atlas/Icons) weiter in physischer
Auflösung passieren (Schärfe), aber **Geometrie/Scissor/Maus** NICHT mehr von
Editron skaliert werden (das macht raylib). Diese Trennung (`raster_scale` vs.
`geom_scale`) ist der Kern der macOS-Arbeit.

---

## 1. Voraussetzungen

```sh
# Rust-Toolchain
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Build-Abhängigkeiten (Homebrew)
brew install cmake ffmpeg fontconfig

# Repo bauen
cd /pfad/zu/editron
cargo build
```

Ein kurzes Testvideo (falls keins zur Hand):

```sh
ffmpeg -y -f lavfi -i testsrc=size=1280x720:rate=30:duration=4 \
  -f lavfi -i sine=frequency=440:duration=4 \
  -c:v libx264 -pix_fmt yuv420p -c:a aac /tmp/hidpi_test.mp4
```

**Wichtig:** Diese Tests müssen auf einem **echten Retina-Display** (oder einem
per Skalierung auf „Mehr Fläche"/„Größerer Text" gestellten Display) laufen — ein
externer 1×-Monitor zeigt das Problem nicht.

---

## 2. Phase 1 — Ist-Zustand erfassen (VOR jeder Code-Änderung)

Ziel: dokumentieren, wie sich der aktuelle Code (ohne HIGHDPI) auf Retina
verhält, und die **rohen raylib-Maße** auslesen. Daraus leiten wir das exakte
macOS-Modell ab.

### 2.1 DPI-Diagnose-Ausgabe

Das Flag `EDITRON_DPI_DEBUG=1` loggt einmal pro Sekunde die relevanten Maße nach
stderr (eingebaut in `src/main.rs`).

```sh
EDITRON_DPI_DEBUG=1 EDITRON_UI_SCALE=auto \
EDITRON_TEST_IMPORT=/tmp/hidpi_test.mp4 EDITRON_TEST_TIMELINE=1 \
./target/debug/editron
```

Bewege das Fenster, ziehe es ggf. auf einen zweiten Monitor mit anderer DPI, und
notiere die `[dpi] …`-Zeilen. Zum Vergleich — so sieht es auf **X11** (1×-Monitor,
fertige Implementierung) aus:

```
[dpi] dpi=1.000 ui_scale=1.000 screen=1440x900 render=1440x900 logical=1440x900 mouse_raw=(812,431) mouse_logical=(812,431)
```

→ X11: `screen == render`, kein Retina-Backing, Maus = Pixel = logisch (weil
scale 1.0).

**Erwartung auf macOS-Retina (Hypothese, bitte bestätigen):**

- **ohne HIGHDPI** (aktueller Code): `screen == render` (beide in *Punkten*, z. B.
  1440×900), aber `dpi=2.000`. ⇒ `ui_scale=2.0`, `logical = render/2 = 720×450`.
  Das ist **falsch** (UI 2× zu groß) **und** unscharf (macOS skaliert das
  non-Retina-Fenster hoch). Doppelt schlecht — der klassische „alte App auf
  Retina"-Look.
- **mit HIGHDPI** (nach Phase 2): `render == screen × 2` (z. B. screen=1440×900,
  render=2880×1800), `dpi=2.000`.

### 2.2 Screenshots des Ist-Zustands

```sh
# Auto-DPI (zeigt das Problem)
EDITRON_DPI_DEBUG=1 EDITRON_UI_SCALE=auto EDITRON_SHOT=ist_auto.png EDITRON_SHOT_FRAME=120 \
EDITRON_TEST_IMPORT=/tmp/hidpi_test.mp4 EDITRON_TEST_TIMELINE=1 EDITRON_TEST_PLAY=1 \
./target/debug/editron

# Erzwungener Scale 1.0 (definierter Ausgangspunkt, kein Auto-Effekt)
EDITRON_UI_SCALE=1.0 EDITRON_SHOT=ist_10.png EDITRON_SHOT_FRAME=120 \
EDITRON_TEST_IMPORT=/tmp/hidpi_test.mp4 EDITRON_TEST_TIMELINE=1 EDITRON_TEST_PLAY=1 \
./target/debug/editron
```

> Hinweis: `take_screenshot` speichert relativ zum **CWD** (also dort, wo du den
> Befehl startest). Die Bildgröße = Framebuffer in Pixeln.

**Zu berichten nach Phase 1:**
1. Die `[dpi] …`-Zeilen (mind. eine pro getestetem Monitor).
2. `ist_auto.png` + `ist_10.png` — sind Text/Linien scharf oder verwaschen?
3. Stimmt die Fenstergröße der Screenshots mit `render=…` aus dem Log überein?

---

## 3. Phase 2 — HIGHDPI aktivieren

raylib braucht das Flag `FLAG_WINDOW_HIGHDPI` **vor** `InitWindow`. `SetConfigFlags`
ist additiv (`CORE.Window.flags |= flags`, `rcore.c:1884`), d. h. ein manueller
FFI-Aufruf vor `build()` überlebt — raylib-rs' Builder hat nur keine bequeme
Methode dafür.

In `src/main.rs`, direkt **vor** `let (mut rl, thread) = raylib::init()…` (aktuell
~Zeile 1089):

```rust
// macOS: echten Retina-Framebuffer anfordern (COCOA_RETINA_FRAMEBUFFER).
// SetConfigFlags ist additiv ⇒ der nachfolgende build()-Aufruf löscht das Bit
// nicht. NUR auf macOS, weil HIGHDPI sonst GLFW_SCALE_TO_MONITOR auf allen
// Plattformen umlegt (rcore_desktop_glfw.c:1348) und X11 verändern würde.
#[cfg(target_os = "macos")]
unsafe {
    raylib::ffi::SetConfigFlags(raylib::ffi::ConfigFlags::FLAG_WINDOW_HIGHDPI as u32);
}

let (mut rl, thread) = raylib::init()
    .size(1440, 900)
    .title("Editron")
    .resizable()
    .msaa_4x()
    .build();
```

Danach erneut Phase 1.1 fahren und prüfen, dass jetzt `render == screen × 2`
gemeldet wird. **Erst wenn das stimmt**, weiter zu Phase 3.

---

## 4. Phase 3 — Doppelskalierung diagnostizieren & beheben

Nach Phase 2 ist HIGHDPI an, aber Editron skaliert noch nach dem X11-Modell
(alles × `scale`) — auf macOS skaliert raylib zusätzlich selbst. Erwartetes
Symptom: **alles riesig** (≈ 4× statt 2×) und/oder Maus/Klicks am falschen Ort,
Scissor-Clipping verschoben.

Die saubere Lösung trennt zwei Faktoren. Empfohlener Plan:

### 4.1 Zwei Scale-Begriffe einführen

- `raster_scale` — für die **physische Rasterung** (Font-Atlas-baseSize,
  Icon-Strichstärke). **Beide Plattformen** = effektiver DPI-Scale.
- `geom_scale` — für die **Geometrie-Übersetzung** in `Ui` (`sx`/`pv`/`rp`/`rpf`,
  `begin_scissor`) und die Maus-Division. **X11** = DPI-Scale, **macOS** = `1.0`
  (raylib skaliert selbst).

Konkret:

1. **`src/ui/text.rs`** — `Fonts::load(rl, thread, raster_scale)` bleibt wie jetzt
   (Atlas in `size × raster_scale × OVERSAMPLE`). Aber `FontHandle.render_size`
   (das Größen-Argument für `draw_text_ex`) muss auf macOS die **logische** Größe
   sein (nicht × scale), weil raylibs Viewport bereits hochskaliert. Auf X11
   bleibt `render_size = size × scale`.
   → Praktisch: `render_size = size × geom_scale`, `baseSize = size × raster_scale × OVERSAMPLE`.

2. **`src/ui/mod.rs`** — `Ui::scale` (genutzt von `sx`/`pv`/`rp`/`rpf`/`begin_scissor`)
   bekommt **`geom_scale`** statt des bisherigen Werts. Auf macOS = 1.0 ⇒ keine
   Eigenskalierung, raylib macht es. Die `icon`-Methode reicht weiter den
   **`raster_scale`** an `IconSet::draw` (Schärfe) — Icons werden über
   `draw_line_ex` mit absoluten Koordinaten gezeichnet, also: Koordinaten ×
   `geom_scale`, aber Strichstärke fein genug (raylib + MSAA rastern in physischer
   Auflösung, weil der Viewport hochskaliert).
   ⚠️ Hier genau hinschauen: Icons zeichnen mit `ui_scale` sowohl Punkte als auch
   Stroke. Auf macOS müssen die **Punkte × geom_scale (=1.0)** laufen, die
   **Schärfe** kommt automatisch vom Viewport. Stroke = `2 × icon_scale`
   (logisch), raylib rastert es physisch.

3. **Maus** in `src/main.rs` (`input.mouse /= ui_scale`, ~Zeile 1250) — durch
   `geom_scale` teilen. Auf macOS liefert `GetMousePosition` bereits Punkte
   (= logisch), also `geom_scale = 1.0` ⇒ keine Division. **Verifizieren** über
   `mouse_raw` vs. `mouse_logical` im DPI-Log (siehe 4.3).

4. **`screen`** in `src/main.rs` (~Zeile 1259) — bleibt `render / raster_scale`?
   Nein: das Layout rechnet in Punkten. Auf macOS ist `screen` (Punkte) bereits
   die logische Größe ⇒ `logical = GetScreenWidth()` direkt. Auf X11 ist
   `logical = render / scale`. Sauber: `logical = render / raster_scale`
   funktioniert auf BEIDEN (X11: render/dpi; macOS: render(=screen×dpi)/dpi =
   screen = Punkte). ✓ Diese Zeile muss also `raster_scale` benutzen, nicht
   `geom_scale`.

> Faustregel: **Rasterung & logische Größe** → `raster_scale` (= echter DPI).
> **Geometrie-Übersetzung & Maus** → `geom_scale` (X11: DPI, macOS: 1.0).

### 4.2 Wo `geom_scale` herkommt

```rust
// X11/Windows: raylib skaliert NICHT selbst ⇒ Editron muss.
// macOS: raylib skaliert Viewport/Projektion/Scissor selbst ⇒ Editron darf nicht.
#[cfg(target_os = "macos")]
let geom_scale = 1.0;
#[cfg(not(target_os = "macos"))]
let geom_scale = raster_scale;
```

`raster_scale` = der bisherige `ui_scale` (Override > DPI).

### 4.3 Maus-Hit-Test verifizieren (entscheidend)

Razor-Vorschaulinie folgt der **logischen** Maus. Wenn die Maus-Mathematik
stimmt, liegt die Linie exakt unter dem Cursor:

```sh
EDITRON_DPI_DEBUG=1 \
EDITRON_TEST_IMPORT=/tmp/hidpi_test.mp4 EDITRON_TEST_TIMELINE=1 \
EDITRON_TEST_TOOL=razor \
./target/debug/editron
```

Fahre mit der echten Maus über die Timeline-Clips. Die orange Linie **muss** unter
dem Cursor kleben. Im `[dpi]`-Log muss `mouse_logical` die **logische** Position
sein (im `screen`-Bereich), nicht doppelt verkleinert/vergrößert.

---

## 5. Phase 4 — Finale Verifikation

Nach den Anpassungen aus Phase 3. Jeweils Screenshot + visuelle Prüfung.

```sh
# Editor mit Timeline + Wiedergabe
EDITRON_TEST_IMPORT=/tmp/hidpi_test.mp4 EDITRON_TEST_TIMELINE=1 EDITRON_TEST_PLAY=1 \
EDITRON_SHOT=v_editor.png EDITRON_SHOT_FRAME=150 ./target/debug/editron

# Export-Dialog (Overlay-Skalierung: Slider/Dropdowns/Scrollbar/Checkbox)
EDITRON_TEST_IMPORT=/tmp/hidpi_test.mp4 EDITRON_TEST_TIMELINE=1 \
EDITRON_TEST_DIALOG=export \
EDITRON_SHOT=v_export.png EDITRON_SHOT_FRAME=150 ./target/debug/editron

# Laufzeit-Scale-Wechsel (Atlas-Neuaufbau ohne Neustart)
EDITRON_TEST_IMPORT=/tmp/hidpi_test.mp4 EDITRON_TEST_TIMELINE=1 \
EDITRON_TEST_SCALE_TO=1.0 \
EDITRON_SHOT=v_dynamic.png EDITRON_SHOT_FRAME=200 ./target/debug/editron
```

### Checkliste (alles muss erfüllt sein)

- [ ] **Schärfe:** Titelleiste „Editron", Menü, Timecodes, Statusleiste, Clip-Namen
      sind gestochen scharf — kein Verwaschen, keine 1,5-px-Säume.
- [ ] **1-px-Linien:** Trennlinien zwischen Panels, Playhead, Timeline-Lineal sind
      exakt scharf (nicht grau-verschmiert).
- [ ] **Layout:** identisch zur X11-Referenz (gleiche relative Anordnung), nur
      schärfer — keine halb skalierten Mischzustände, keine abgeschnittenen Panels.
- [ ] **Hit-Test:** Razor-Linie klebt unter dem Cursor; Buttons/Tabs reagieren dort,
      wo sie sichtbar sind; Klicks in **gescrollten** Bereichen (Medien-Browser-Liste,
      Effekt-Panel) treffen die richtige Zeile.
- [ ] **Overlays:** Export-Dialog zentriert, Backdrop deckt alles, alle Widgets
      (Slider/Dropdowns/Scrollbar/Checkbox/Buttons) scharf & korrekt platziert;
      Kontextmenü (Rechtsklick) und Tooltips erscheinen am Cursor.
- [ ] **Drag-Ghost:** Asset aus dem Medien-Browser ziehen — das Ghost-Label hängt am
      Cursor, korrekt skaliert.
- [ ] **Fraktional:** mit `EDITRON_UI_SCALE=1.5` ebenfalls scharf (falls das Display
      1,5× fährt) — kein nur-2.0-Sonderfall.
- [ ] **Monitor-Wechsel:** Fenster auf einen Monitor mit anderer DPI ziehen — UI baut
      sich scharf neu auf, **ohne Neustart** (DPI-Log zeigt den Wechsel).
- [ ] **Video/Thumbnails:** Programmmonitor-Bild + Timeline-Thumbnails sind scharf
      (Inhalt skaliert wie bisher über `draw_texture_pro`).

---

## 6. Fehlerbilder & Diagnose

| Symptom | wahrscheinliche Ursache | Fix |
|---|---|---|
| Alles ~2× zu groß, **unscharf** | HIGHDPI nicht aktiv (Phase 2 fehlt) ⇒ non-Retina-Backing | `#[cfg(macos)] SetConfigFlags(FLAG_WINDOW_HIGHDPI)` vor `build()` |
| Alles ~2× zu groß, **scharf** | doppelte Geometrie-Skalierung (Editron × scale **und** raylib × scale) | `geom_scale = 1.0` auf macOS (Phase 4.1) |
| Scharf & richtig groß, aber **Klicks daneben** | Maus doppelt geteilt | Maus durch `geom_scale` (=1.0 macOS) teilen, nicht `raster_scale` |
| Clipping verschoben / Panels schneiden falsch ab | Scissor doppelt skaliert (`begin_scissor` × scale **und** raylib `rcore.c:1163`) | `begin_scissor` nutzt `geom_scale` (=1.0 macOS) |
| Text **unscharf**, Geometrie sonst ok | Atlas in logischer statt physischer Auflösung gerastert | `baseSize = size × raster_scale × OVERSAMPLE` (raster_scale = echter DPI) |
| Text **scharf aber zu klein/groß** | `render_size` falsch | `render_size = size × geom_scale` |

`EDITRON_DPI_DEBUG=1` ist bei allen Fällen der erste Schritt: die Relation
`render` ↔ `screen` ↔ `dpi` und `mouse_raw` ↔ `mouse_logical` zeigt sofort, welche
Schicht doppelt skaliert.

---

## 7. Referenzen

**Editron-Code (Zeilen ungefähr, können driften):**
- `src/main.rs` — `raylib::init()…build()` (~1089), `detected_dpi_scale` (~1079),
  Maus-Division (~1250), `screen = render/ui_scale` (~1259), `EDITRON_DPI_DEBUG`-Log.
- `src/ui/mod.rs` — `Ui::scale` + Helfer `sx`/`pv`/`rp`/`rpf` (~460), `begin_scissor`
  (~488), `text`/`icon`.
- `src/ui/text.rs` — `FontHandle::load` (`baseSize`/`render_size`), `Fonts::load`.
- `src/ui/icons.rs` — `IconSet::draw(…, ui_scale)`.
- `src/core/settings.rs` — `PerfSettings.ui_scale`, `resolve_ui_scale`,
  `EDITRON_UI_SCALE`.

**raylib-Quelle (raylib-sys 5.5.1, `raylib/src/`):**
- `rcore.c:1884` — `SetConfigFlags` ist additiv (`|=`).
- `rcore.c:1163` — Scissor wird auf `__APPLE__` IMMER mit `GetWindowScaleDPI`
  skaliert (unabhängig vom Flag).
- `rcore.c:3546` — `SetupViewport`: auf `__APPLE__` Viewport × scale, Ortho auf
  `render.width`.
- `rcore_desktop_glfw.c:1348` — HIGHDPI setzt `GLFW_SCALE_TO_MONITOR=TRUE`
  (alle Plattformen) + `GLFW_COCOA_RETINA_FRAMEBUFFER=TRUE` (nur macOS).

**Test-Flags (vollständig in CLAUDE.md):**
- `EDITRON_DPI_DEBUG=1` — DPI-/Maß-Diagnose nach stderr (1×/s).
- `EDITRON_UI_SCALE=1.0|1.5|2.0|auto` — Scale-Override bzw. Auto-DPI.
- `EDITRON_TEST_SCALE_TO=2.0` — schaltet nach 60 Frames live um (Laufzeit-Rebuild).
- `EDITRON_TEST_MOUSE=x,y` — synthetische Maus in **Framebuffer-Pixeln**.
- `EDITRON_TEST_TOOL=razor` — Razor-Vorschau für den Hit-Test.
- `EDITRON_SHOT=…png` / `EDITRON_SHOT_FRAME=N` — Screenshot bei Frame N, dann Ende.

---

## 8. Was zurückzumelden ist

Damit die `cfg(macos)`-Implementierung final korrekt wird, bitte melden:
1. Die `[dpi]`-Zeilen aus Phase 1 (ohne HIGHDPI) **und** Phase 2 (mit HIGHDPI),
   je 1×-/2×-Monitor.
2. Ob `GetMousePosition` auf macOS+HIGHDPI Punkte (= `screen`-Bereich) oder Pixel
   (= `render`-Bereich) liefert — ablesbar an `mouse_raw` vs. der tatsächlichen
   Cursor-Position.
3. Screenshots der Checkliste (mind. Editor + Export-Dialog) bei 2.0 (und 1.5,
   falls möglich).

Mit diesen Daten lässt sich die `raster_scale`/`geom_scale`-Trennung exakt
verdrahten — die Hypothese in Abschnitt 0/4 ist begründet, aber erst der echte
macOS-Lauf bestätigt das genaue Modell.
