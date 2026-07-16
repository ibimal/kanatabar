#!/usr/bin/env bash
# Gate 10 — release packaging (SPEC §12, §13, §19).
# [AUTO] the packaging scripts run cleanly and their artifacts are sound:
#        universal (two-arch) ad-hoc-signed binaries, a signed KanataBar.app,
#        a pkg whose payload is app→/Applications + kanatad/kanatactl→
#        /usr/local/bin with a kanatactl-install postinstall, a tarball of the
#        three bare binaries, and matching SHA-256 checksums. Plus the
#        install-side behaviors the pkg relies on, by exact test name.
# [HW]   brew-install on a clean Mac → wizard → working remap; `brew upgrade`
#        replaces both binaries and the daemon comes back. See docs/HW-TESTS.md.
set -euo pipefail
cd "$(dirname "$0")/../.."

cargo build --workspace
cargo test --workspace

# The gate-defining install behaviors the pkg's postinstall depends on.
for t in \
    install_prefers_the_bundled_tray_when_the_app_is_present \
    uninstall_leaves_a_foreign_app_bundle_alone \
    postinstall_style_self_install_keeps_the_binaries; do
    cargo test -q -p kanatactl --test install "$t" -- --exact "$t"
done
cargo test -q -p kanatactl --lib install::tests

# Build every release artifact (universal → bundle → pkg + tarball + sums).
./packaging/build-pkg.sh

version=$(./packaging/version.sh)
pkg="dist/KanataBar-$version.pkg"
tarball="dist/kanatabar-$version-macos-universal.tar.gz"

# Universal + signed, every shipped binary.
for bin in kanatad kanatactl kanatabar-tray; do
    archs=$(lipo -archs "target/universal/$bin")
    [[ "$archs" == *x86_64* && "$archs" == *arm64* ]] \
        || { echo "FAIL: $bin is not universal ($archs)"; exit 1; }
    codesign --verify "target/universal/$bin" \
        || { echo "FAIL: $bin signature"; exit 1; }
done

# The app bundle: signed, right identity, right version.
codesign --verify --deep dist/KanataBar.app
grep -q "io.github.ibimal.kanatabar" dist/KanataBar.app/Contents/Info.plist
grep -q "<string>$version</string>" dist/KanataBar.app/Contents/Info.plist

# Pkg payload shape (SPEC §12): app + the two CLI binaries; the tray ships
# ONLY inside the bundle.
payload=$(pkgutil --payload-files "$pkg")
for path in ./Applications/KanataBar.app ./usr/local/bin/kanatad ./usr/local/bin/kanatactl; do
    grep -qx "$path" <<<"$payload" || { echo "FAIL: $path missing from pkg payload"; exit 1; }
done
if grep -qx "./usr/local/bin/kanatabar-tray" <<<"$payload"; then
    echo "FAIL: bare tray binary must not be in the pkg payload"; exit 1
fi

# Postinstall runs the payloaded installer.
expand=$(mktemp -d)/pkg
pkgutil --expand "$pkg" "$expand"
grep -q "/usr/local/bin/kanatactl install" "$expand/Scripts/postinstall" \
    || { echo "FAIL: postinstall must run kanatactl install"; exit 1; }

# No relocatable components: the installer would otherwise follow Spotlight to
# any existing copy of the bundle (HW 2026-07-16: the app landed in the build
# tree instead of /Applications). Fixed packages carry an EMPTY `<relocate/>`;
# the broken form is `<relocate>` with `<bundle id=…>` children.
if grep -q "<relocate>" "$expand/PackageInfo"; then
    echo "FAIL: pkg has relocatable components (missing BundleIsRelocatable=false)"
    exit 1
fi

# Tarball: exactly the three bare binaries (SPEC §12).
[[ "$(tar -tzf "$tarball" | sort | tr '\n' ' ')" == "kanatabar-tray kanatactl kanatad " ]] \
    || { echo "FAIL: tarball contents: $(tar -tzf "$tarball" | tr '\n' ' ')"; exit 1; }

# Checksums match the artifacts.
(cd dist && shasum -c SHA256SUMS)

echo "gate-10 [AUTO]: PASS"
echo
echo "gate-10 [HW]: the brew install/upgrade path needs a human — see"
echo "  docs/HW-TESTS.md Run 9: on a clean Mac, 'brew install --cask"
echo "  ibimal/tap/kanatabar' → Setup Wizard → working remap; then a version"
echo "  bump + 'brew upgrade' replaces kanatad AND the tray, and the daemon"
echo "  comes back Running with the same config."
