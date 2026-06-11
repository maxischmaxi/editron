# Packaging — Doppelklick-Integration für `.etron`-Projektdateien

Editron öffnet Projektdateien über die Kommandozeile auf allen Systemen:

```sh
editron pfad/zum/projekt.etron   # Projekt öffnen
editron clip.mp4 musik.wav       # Medien direkt importieren
```

Damit **Doppelklick im Dateimanager** funktioniert, muss das Betriebssystem
die Endung `.etron` (MIME `application/x-editron-project`) mit Editron
verknüpfen — dafür gibt es hier je System ein Setup:

## Linux

```sh
cargo build --release
cd packaging/linux
./install.sh ../../target/release/editron
```

Installiert nutzerlokal (`~/.local/share`): MIME-Definition
(`editron-project.xml`, inkl. Magic auf den JSON-Header), den
`.desktop`-Eintrag (`Exec=… %f`) und setzt Editron als Standard-Handler.
Kein Root nötig.

## Windows

```powershell
cargo build --release
cd packaging\windows
.\register-file-association.ps1            # nimmt target\release\editron.exe
# oder: .\register-file-association.ps1 -ExePath "C:\Tools\editron.exe"
```

Schreibt die Zuordnung nach `HKCU\Software\Classes` (nur aktueller Nutzer,
kein Admin). Windows übergibt die Datei als argv — von der App bereits
unterstützt.

## macOS

```sh
cd packaging/macos
./make-app-bundle.sh release
```

Erstellt `target/release/Editron.app` mit `Info.plist`
(`CFBundleDocumentTypes` + exportierter UTI `com.editron.project`).
Finder-Doppelklick schickt kein argv, sondern ein Apple Event (`odoc`) —
das fängt `src/platform/macos.rs` ab (Handler am `NSAppleEventManager`,
funktioniert für Start per Doppelklick und für Dateien, die bei laufender
App geöffnet werden).

> Hinweis: Der macOS-Pfad ist unter Linux entwickelt und dort nicht
> baubar/testbar — beim ersten Mac-Build bitte verifizieren.
