# Cask template for the ibimal/homebrew-tap — the release workflow substitutes
# __VERSION__/__SHA256__ and pushes the result to Casks/kanatabar.rb (SPEC §13).
cask "kanatabar" do
  version "__VERSION__"
  sha256 "__SHA256__"

  url "https://github.com/ibimal/kanatabar/releases/download/v#{version}/KanataBar-#{version}.pkg"
  name "KanataBar"
  desc "Supervisor and menu bar app for the kanata keyboard remapper"
  homepage "https://github.com/ibimal/kanatabar"

  depends_on macos: :ventura

  pkg "KanataBar-#{version}.pkg"

  uninstall script:  {
              executable: "/usr/local/bin/kanatactl",
              args:       ["uninstall"],
              sudo:       true,
            },
            pkgutil: "io.github.ibimal.kanatabar"

  caveats <<~EOS
    KanataBar requires kanata and the Karabiner-DriverKit-VirtualHIDDevice
    driver; the menu bar Setup Wizard walks you through installing both and
    granting the needed permissions.

    KanataBar is not notarized (no paid Apple Developer account — see the
    project README). If macOS blocks it, allow it under System Settings →
    Privacy & Security → "Open Anyway".

    brew upgrade is the only update mechanism; the app never self-updates.
  EOS
end
