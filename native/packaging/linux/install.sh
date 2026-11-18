#!/bin/sh
# Installs Sonic for the current user (no root needed): binary, launcher and icon.
set -e
here="$(cd "$(dirname "$0")" && pwd)"
bin="${XDG_BIN_HOME:-$HOME/.local/bin}"
data="${XDG_DATA_HOME:-$HOME/.local/share}"
mkdir -p "$bin" "$data/applications" "$data/icons/hicolor/512x512/apps"
install -m 755 "$here/sonicplayer" "$bin/sonicplayer"
install -m 644 "$here/sonic.desktop" "$data/applications/sonic.desktop"
install -m 644 "$here/sonic.png" "$data/icons/hicolor/512x512/apps/sonic.png"
command -v update-desktop-database >/dev/null && update-desktop-database "$data/applications" || true
echo "Sonic installed to $bin/sonicplayer. Find it as \"Sonic\" in your app launcher."
