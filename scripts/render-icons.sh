#!/usr/bin/env bash
# Draws every size of Plug's icon from docs/assets/plug-icon.svg: the pictures
# the docs and the MCP server icon use, and the Mac app icon. Run it after
# changing that file. Needs rsvg-convert (brew install librsvg).
set -euo pipefail
cd "$(dirname "$0")/.."

command -v rsvg-convert >/dev/null || { echo "render-icons: brew install librsvg" >&2; exit 1; }

source_svg=docs/assets/plug-icon.svg
for size in 16 32 64 128 256 512; do
  rsvg-convert -w "$size" -h "$size" -o "docs/assets/plug-icon-$size.png" "$source_svg"
done

# The Mac app icon is the same picture as an 824 tile on a 1024 canvas, with
# the shadow macOS icons carry under them.
mac_svg=$(mktemp)
trap 'rm -f "$mac_svg"' EXIT
{
  echo '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024">'
  echo '<defs><filter id="shadow" x="-20%" y="-20%" width="140%" height="140%"><feGaussianBlur stdDeviation="11"/></filter></defs>'
  echo '<rect x="100" y="112" width="824" height="824" rx="185.4" opacity="0.3" filter="url(#shadow)"/>'
  echo '<svg x="100" y="100" width="824" height="824" viewBox="0 0 120 120">'
  sed '1d;$d' "$source_svg"
  echo '</svg></svg>'
} > "$mac_svg"
for size in 16 32 64 128 256 512 1024; do
  rsvg-convert -w "$size" -h "$size" \
    -o "PlugApp/PlugApp/Assets.xcassets/AppIcon.appiconset/icon-$size.png" "$mac_svg"
done
echo "render-icons: done"
