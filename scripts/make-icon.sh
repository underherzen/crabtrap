#!/usr/bin/env bash
# Пересобрать AppIcon.icns из AppIcon.svg (нужно только при смене иконки;
# результат коммитится, CI его не генерирует).
# Зависимость: resvg (cargo install resvg).
set -euo pipefail

ICON_DIR="$(cd "$(dirname "$0")/../packaging/macos/icon" && pwd)"
SVG="$ICON_DIR/AppIcon.svg"
ICONSET="$(mktemp -d)/AppIcon.iconset"

command -v resvg >/dev/null || {
    echo "resvg not found: cargo install resvg" >&2
    exit 1
}

mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
    resvg -w "$size" -h "$size" "$SVG" "$ICONSET/icon_${size}x${size}.png"
    resvg -w $((size * 2)) -h $((size * 2)) "$SVG" "$ICONSET/icon_${size}x${size}@2x.png"
done

cp "$ICONSET/icon_512x512@2x.png" "$ICON_DIR/AppIcon-1024.png"
iconutil -c icns "$ICONSET" -o "$ICON_DIR/AppIcon.icns"
echo "wrote $ICON_DIR/AppIcon.icns"
