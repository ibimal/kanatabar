#!/usr/bin/env bash
# Build the release artifacts (SPEC §12, §13):
#   dist/KanataBar-<ver>.pkg                      app → /Applications,
#                                                 kanatad+kanatactl → /usr/local/bin,
#                                                 postinstall = kanatactl install
#   dist/kanatabar-<ver>-macos-universal.tar.gz   the three bare binaries,
#                                                 for `sudo kanatactl install`
#   dist/SHA256SUMS
# Everything ad-hoc signed; no Developer ID, no notarization (SPEC §12).
set -euo pipefail
cd "$(dirname "$0")/.."

./packaging/build-universal.sh
./packaging/bundle-app.sh

version=$(./packaging/version.sh)
pkgroot=dist/pkgroot
scripts=dist/pkgscripts

rm -rf "$pkgroot" "$scripts"
mkdir -p "$pkgroot/Applications" "$pkgroot/usr/local/bin" "$scripts"

cp -R dist/KanataBar.app "$pkgroot/Applications/"
cp target/universal/kanatad target/universal/kanatactl "$pkgroot/usr/local/bin/"

# postinstall: the payloaded kanatactl does the real work (dirs, plists,
# launchctl bootstraps) — one code path for pkg and tarball installs. It
# detects the payloaded app bundle and points the agent into it.
cat > "$scripts/postinstall" <<'EOF'
#!/bin/sh
set -e
/usr/local/bin/kanatactl install
EOF
chmod 0755 "$scripts/postinstall"

# Strip what xattrs we can (quarantine, FinderInfo) from the staging root so
# the payload stays minimal. SIP-protected `com.apple.provenance` survives on
# local builds and shows up as AppleDouble `._*` payload entries — harmless:
# the installer restores those as xattrs on the installed files, not as
# literal `._` files. Ad-hoc signatures live inside the Mach-O and survive.
xattr -cr "$pkgroot" 2>/dev/null || true

# pkgbuild marks app components RELOCATABLE by default: the installer then
# follows Spotlight to any existing copy of the bundle id — e.g. this very
# build tree — and installs the app THERE instead of /Applications. Observed
# on HW (2026-07-16): the payload app landed inside dist/ and /Applications
# stayed empty. Pin every component down.
components=dist/components.plist
pkgbuild --analyze --root "$pkgroot" "$components"
idx=0
while plutil -replace "$idx.BundleIsRelocatable" -bool NO "$components" >/dev/null 2>&1; do
    idx=$((idx + 1))
done

pkgbuild \
    --root "$pkgroot" \
    --scripts "$scripts" \
    --component-plist "$components" \
    --identifier io.github.ibimal.kanatabar \
    --version "$version" \
    --install-location / \
    "dist/KanataBar-$version.pkg"

COPYFILE_DISABLE=1 tar -czf "dist/kanatabar-$version-macos-universal.tar.gz" \
    -C target/universal kanatad kanatactl kanatabar-tray

(cd dist && shasum -a 256 \
    "KanataBar-$version.pkg" \
    "kanatabar-$version-macos-universal.tar.gz" \
    > SHA256SUMS)

echo "artifacts:"
ls -l "dist/KanataBar-$version.pkg" "dist/kanatabar-$version-macos-universal.tar.gz" dist/SHA256SUMS
