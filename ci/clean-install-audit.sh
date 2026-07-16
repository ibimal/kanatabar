#!/usr/bin/env bash
# List every path a real `sudo kanatactl install` can touch (SPEC §3.2, §10),
# with a ✅ present / ❌ absent marker. Run it before install, after install,
# and after uninstall on real hardware (Phase 6 [HW]: the diff across those
# three runs *is* the uninstall audit — "leaves nothing behind", SPEC §16).
set -euo pipefail

# The user the LaunchAgent installs for: prefer SUDO_USER (the documented
# `sudo kanatactl install` invocation, SPEC §9), else whoever is running this.
TARGET_USER="${SUDO_USER:-$(id -un)}"
TARGET_HOME="$(dscl . -read "/Users/$TARGET_USER" NFSHomeDirectory 2>/dev/null | awk '{print $2}')"
TARGET_HOME="${TARGET_HOME:-$HOME}"

PATHS=(
    "/usr/local/bin/kanatad"
    "/usr/local/bin/kanatactl"
    "/usr/local/bin/kanatabar-tray"
    "/Library/LaunchDaemons/io.github.ibimal.kanatabar.daemon.plist"
    "/Library/LaunchDaemons/io.github.ibimal.kanatabar.vhidd.plist"
    "$TARGET_HOME/Library/LaunchAgents/io.github.ibimal.kanatabar.agent.plist"
    "/Library/Application Support/KanataBar"
    "/Library/Logs/KanataBar"
    "/var/run/kanatabar.sock"
)

echo "clean-install-audit — target user: $TARGET_USER ($TARGET_HOME)"
echo
for path in "${PATHS[@]}"; do
    if [ -e "$path" ]; then
        echo "present  $path"
    else
        echo "absent   $path"
    fi
done

echo
echo "launchd jobs:"
launchctl print system/io.github.ibimal.kanatabar.daemon >/dev/null 2>&1 \
    && echo "loaded   system/io.github.ibimal.kanatabar.daemon" \
    || echo "unloaded system/io.github.ibimal.kanatabar.daemon"
launchctl print system/io.github.ibimal.kanatabar.vhidd >/dev/null 2>&1 \
    && echo "loaded   system/io.github.ibimal.kanatabar.vhidd" \
    || echo "unloaded system/io.github.ibimal.kanatabar.vhidd"
TARGET_UID="$(id -u "$TARGET_USER")"
launchctl print "gui/$TARGET_UID/io.github.ibimal.kanatabar.agent" >/dev/null 2>&1 \
    && echo "loaded   gui/$TARGET_UID/io.github.ibimal.kanatabar.agent" \
    || echo "unloaded gui/$TARGET_UID/io.github.ibimal.kanatabar.agent"
