#!/usr/bin/env bash
# Baut ein minimales Editron.app-Bundle (macOS) mit Dateizuordnung für .etron.
# Auf einem Mac ausführen:  ./make-app-bundle.sh [release|debug]
#
# Danach das Bundle z. B. nach /Applications kopieren; Launch Services
# registriert die .etron-Zuordnung beim ersten Start automatisch.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
PROFILE="${1:-release}"

if [[ "$PROFILE" == "release" ]]; then
  (cd "$ROOT" && cargo build --release)
else
  (cd "$ROOT" && cargo build)
fi

BIN="$ROOT/target/$PROFILE/editron"
[[ -x "$BIN" ]] || { echo "Fehler: $BIN fehlt." >&2; exit 1; }

APP="$ROOT/target/$PROFILE/Editron.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$HERE/Info.plist" "$APP/Contents/Info.plist"
cp "$BIN" "$APP/Contents/MacOS/editron"

# Ad-hoc-Signatur, damit Gatekeeper das lokale Bundle startet.
codesign --force --deep --sign - "$APP" 2>/dev/null || true

echo "Fertig: $APP"
echo "Tipp: 'open $APP' starten oder nach /Applications kopieren."
