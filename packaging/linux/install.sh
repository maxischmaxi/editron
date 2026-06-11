#!/usr/bin/env bash
# Desktop-Integration für Editron unter Linux (nutzerlokal, kein Root):
#   - MIME-Typ application/x-editron-project (*.etron)
#   - .desktop-Eintrag mit Doppelklick-Öffnen
#
# Aufruf:  ./install.sh [/pfad/zum/editron-binary]
# Ohne Argument wird `editron` im PATH erwartet (z. B. ~/.local/bin/editron).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
BIN="${1:-}"

install -Dm644 "$HERE/editron-project.xml" \
  "$DATA/mime/packages/editron-project.xml"

if [[ -n "$BIN" ]]; then
  BIN="$(readlink -f "$BIN")"
  [[ -x "$BIN" ]] || { echo "Fehler: $BIN ist nicht ausführbar." >&2; exit 1; }
  # Exec auf das absolute Binary umschreiben (Feld-Code %f bleibt erhalten).
  install -Dm644 /dev/stdin "$DATA/applications/editron.desktop" \
    < <(sed "s|^Exec=editron |Exec=$BIN |" "$HERE/editron.desktop")
else
  command -v editron >/dev/null 2>&1 \
    || echo "Hinweis: 'editron' ist nicht im PATH — Doppelklick funktioniert erst, wenn das Binary auffindbar ist (oder Skript mit Pfad-Argument erneut ausführen)."
  install -Dm644 "$HERE/editron.desktop" "$DATA/applications/editron.desktop"
fi

update-mime-database "$DATA/mime" >/dev/null 2>&1 || true
update-desktop-database "$DATA/applications" >/dev/null 2>&1 || true
xdg-mime default editron.desktop application/x-editron-project || true

echo "Fertig: .etron-Dateien öffnen jetzt Editron (Doppelklick im Dateimanager)."
