#!/bin/sh
# Editron-Installer: lädt die passende Release-Binary von GitHub herunter,
# prüft die SHA256-Summe und installiert nach ~/.local/bin.
#
#   curl -fsSL https://raw.githubusercontent.com/maxischmaxi/editron/main/install.sh | sh
#
# Anpassbar über Umgebungsvariablen:
#   EDITRON_VERSION=0.1.0    bestimmte Version statt der neuesten
#   EDITRON_INSTALL_DIR=…    Zielverzeichnis (Default: ~/.local/bin)
#
# Unterstützte Plattformen: Linux x86_64 · macOS Apple Silicon (arm64).
# Andere Plattformen bauen aus dem Quelltext (siehe README, „Entwicklung“).
set -eu

REPO="maxischmaxi/editron"
INSTALL_DIR="${EDITRON_INSTALL_DIR:-$HOME/.local/bin}"

say()  { printf '%s\n' "$*"; }
fail() { printf 'Fehler: %s\n' "$*" >&2; exit 1; }

command -v curl >/dev/null 2>&1 || fail "curl wird benötigt."
command -v tar  >/dev/null 2>&1 || fail "tar wird benötigt."

# Plattform → Release-Target bestimmen.
os="$(uname -s)"
arch="$(uname -m)"
case "$os/$arch" in
  Linux/x86_64) target="x86_64-unknown-linux-gnu" ;;
  Darwin/arm64) target="aarch64-apple-darwin" ;;
  Darwin/x86_64)
    fail "für macOS (Intel) gibt es noch keine fertige Binary — bitte aus dem Quelltext bauen (cargo build --release)." ;;
  Linux/aarch64 | Linux/arm64)
    fail "für Linux (arm64) gibt es noch keine fertige Binary — bitte aus dem Quelltext bauen (cargo build --release)." ;;
  *)
    fail "nicht unterstützte Plattform: $os/$arch" ;;
esac

# Version bestimmen: explizit gesetzt, sonst der neueste Release-Tag.
# Der Redirect von /releases/latest verrät den Tag ohne API-Aufruf.
if [ -n "${EDITRON_VERSION:-}" ]; then
  version="${EDITRON_VERSION#v}"
else
  latest_url="$(curl -fsSLI --proto '=https' -o /dev/null -w '%{url_effective}' \
    "https://github.com/$REPO/releases/latest")" \
    || fail "konnte https://github.com/$REPO/releases/latest nicht erreichen."
  version="${latest_url##*/}"
  version="${version#v}"
  case "$version" in
    latest | releases | "") fail "kein Release gefunden — wurde schon ein Versions-Tag veröffentlicht?" ;;
  esac
fi

archive="editron-$version-$target.tar.gz"
base_url="https://github.com/$REPO/releases/download/v$version"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

say "Lade Editron $version ($target) …"
curl -fsSL --proto '=https' -o "$tmp/$archive" "$base_url/$archive" \
  || fail "Download fehlgeschlagen: $base_url/$archive"

# Prüfsumme verifizieren (SHA256SUMS liegt jedem Release bei).
shatool=""
if command -v sha256sum >/dev/null 2>&1; then shatool="sha256sum"
elif command -v shasum >/dev/null 2>&1; then shatool="shasum -a 256"
fi
if [ -n "$shatool" ]; then
  curl -fsSL --proto '=https' -o "$tmp/SHA256SUMS" "$base_url/SHA256SUMS" \
    || fail "Download fehlgeschlagen: $base_url/SHA256SUMS"
  ( cd "$tmp" && grep " $archive\$" SHA256SUMS | $shatool -c - >/dev/null 2>&1 ) \
    || fail "Prüfsumme von $archive stimmt nicht — Download beschädigt?"
  say "Prüfsumme ok."
else
  say "Hinweis: weder sha256sum noch shasum gefunden — Prüfsumme wird übersprungen."
fi

tar -xzf "$tmp/$archive" -C "$tmp"
mkdir -p "$INSTALL_DIR"
install -m 755 "$tmp/editron-$version-$target/editron" "$INSTALL_DIR/editron"
say "Installiert: $INSTALL_DIR/editron"

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    say ""
    say "Hinweis: $INSTALL_DIR ist nicht im PATH — z. B. in die Shell-Konfiguration aufnehmen:"
    say "  export PATH=\"$INSTALL_DIR:\$PATH\""
    ;;
esac

if ! command -v ffmpeg >/dev/null 2>&1 || ! command -v ffprobe >/dev/null 2>&1; then
  say ""
  say "Hinweis: Editron benötigt zur Laufzeit FFmpeg/ffprobe im PATH,"
  say "  z. B.: apt install ffmpeg · pacman -S ffmpeg · brew install ffmpeg"
fi

say ""
say "Fertig — starten mit: editron"
