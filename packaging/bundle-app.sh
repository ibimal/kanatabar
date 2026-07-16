#!/usr/bin/env bash
# Assemble dist/KanataBar.app from target/universal/kanatabar-tray, the
# Info.plist template, and the .icns built from the committed iconset
# (SPEC §12; the .icns is derived, never committed). Ad-hoc signed.
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(./packaging/version.sh)
app=dist/KanataBar.app

if [[ ! -x target/universal/kanatabar-tray ]]; then
    echo "target/universal/kanatabar-tray missing — run packaging/build-universal.sh first" >&2
    exit 1
fi

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"

sed "s/__VERSION__/$version/g" packaging/Info.plist > "$app/Contents/Info.plist"
printf 'APPL????' > "$app/Contents/PkgInfo"
cp target/universal/kanatabar-tray "$app/Contents/MacOS/kanatabar-tray"
iconutil -c icns crates/kanatabar-tray/assets/appicon/KanataBar.iconset \
    -o "$app/Contents/Resources/KanataBar.icns"

codesign --force -s - "$app"
codesign --verify --deep "$app"
echo "built $app ($version)"
