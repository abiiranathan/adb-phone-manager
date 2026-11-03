#!/usr/bin/env bash
# Installs ADB Phone Manager, its desktop entry and icon for the current user.
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
data_home=${XDG_DATA_HOME:-"$HOME/.local/share"}
bin_home=${XDG_BIN_HOME:-"$HOME/.local/bin"}

cargo build --release --manifest-path "$root/Cargo.toml"

install -Dm755 "$root/target/release/adb-phone-manager" \
    "$bin_home/adb-phone-manager"
install -Dm644 "$root/packaging/adb-phone-manager.desktop" \
    "$data_home/applications/adb-phone-manager.desktop"

for size in 16 24 32 48 64 128 256 512; do
    install -Dm644 \
        "$root/assets/icons/hicolor/${size}x${size}/apps/adb-phone-manager.png" \
        "$data_home/icons/hicolor/${size}x${size}/apps/adb-phone-manager.png"
done

install -Dm644 "$root/assets/adb-phone-manager.svg" \
    "$data_home/icons/hicolor/scalable/apps/adb-phone-manager.svg"

if command -v update-desktop-database >/dev/null; then
    update-desktop-database "$data_home/applications" >/dev/null 2>&1 || true
fi
if command -v gtk-update-icon-cache >/dev/null; then
    gtk-update-icon-cache -f -t "$data_home/icons/hicolor" >/dev/null 2>&1 || true
fi

printf 'Installed ADB Phone Manager to %s/adb-phone-manager\n' "$bin_home"
