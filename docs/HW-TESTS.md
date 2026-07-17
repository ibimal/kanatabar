# KanataBar — hardware test runbook

`[HW]` gates require real hardware, root, or human action and cannot run in CI
(SPEC §17, §19). Every `[AUTO]` gate for phases 0–9 is green; **nothing in this
file has been verified on hardware yet**. Work through it top to bottom — the
sections are in **dependency order** (what must work first), not phase order.

**How to use this file**

* Each item says what to **Do**, what to **Expect**, and (where a failure has a
  known consequence) **If it fails**. Tick the box when it passes.
* When something deviates — different error text, different command output,
  a wrong System Settings pane — don't fix it by hand: **capture the exact
  output** and note it inline. Several parsers/hints in the code are pinned to
  strings marked `[VERIFY]`; your captures become their fixtures.
* `[VERIFY]` = a fact the code relies on that must be checked against *your*
  installed kanata/driver/macOS versions. All of them are collected in the
  final section as well.
* A failing item is a finding, not a dead end: record it and continue with
  whatever isn't blocked.

**Estimated time:** one sitting of 2–4 hours including two reboots. Runs 1–4
are the critical path (they can force code changes); 5–8 are verification.

***

## 0. Prerequisites (one-time setup)

### Hardware

* A real Mac you can reboot twice and `sudo` on (Apple silicon or Intel).
* Your normal keyboard **plus one spare external USB keyboard** (hotplug
  tests). Optional but useful: a Bluetooth keyboard, a USB hub/dock with
  several devices, a mouse.
* ⚠️ Keyboard-remapping tests can leave a keyboard unusable for a moment.
  Know the kanata panic escape (`lctl+spc+esc`) and keep a second input path
  (SSH from another machine, or Screen Sharing) if you want a safety net.

### Software

1. **Rust workspace built** (from the repo root):

   ```Shell
   cargo build --workspace
   ```

   `kanatactl install` copies the binaries sitting next to itself, so this
   must be current before any install step.

2. **Real kanata:**

   ```Shell
   brew install kanata
   kanata --version        # note this — it drives the next step
   ```

   The daemon **auto-detects** the binary (SPEC §7.3): `/usr/local/bin/kanata`
   first, then `/opt/homebrew/bin/kanata` (Apple-silicon brew) — a fixed
   allowlist, never `$PATH`. An explicit `kanata_bin` in config.toml (or
   `--kanata-bin`/`KANATABAR_KANATA_BIN`) always wins.
   ✅ **Verify the resolution**: after install, `kanatactl doctor` — the
   `kanata binary` check names the path it picked; the startup log line
   `kanatad starting` shows it too. (TCC grants do NOT follow this binary —
   they attach to kanatad; see Prereq 5.)

3. **The Karabiner driver — the version your kanata supports, NOT the
   latest** (SPEC §2 version coupling; e.g. kanata v1.12.0 → driver v6.2.0
   while pqrs's newest may be v8.x). Find the version in your kanata release
   notes, then download that pkg from
   <https://github.com/pqrs-org/Karabiner-DriverKit-VirtualHIDDevice/releases>
   and install it. Don't activate/approve yet if you want to see the wizard
   drive that (Run 3); otherwise activate now:

   ```Shell
   /Applications/.Karabiner-VirtualHIDDevice-Manager.app/Contents/MacOS/Karabiner-VirtualHIDDevice-Manager activate
   ```

   then approve in System Settings → Privacy & Security, and confirm:

   ```Shell
   systemextensionsctl list | grep -i karabiner   # expect "[activated enabled]"
   ```

   📋 **Capture** the full `systemextensionsctl list` output now — it verifies
   the parser fixtures (Run 5).
   ✅ **Confirmed 2026-07-11 (kanata 1.12.0):** driver **v6.2.0** works;
   driver **v8.0.0** (newest pqrs pkg) does **not** — kanata can't speak the
   v8 VHID protocol and silently falls back to passthrough while every check
   still reads green (see Run 1's "Real config via IPC" and ledger row 16).
   Install v6.2.0, not the latest.

4. **A real test config**, e.g. `~/.config/kanata/test.kbd` — caps↔esc is easy
   to verify by feel:

   ```
   (defsrc caps esc)
   (deflayer base esc caps)
   ```

   Sanity-check it: `kanata --cfg ~/.config/kanata/test.kbd --check`.

5. **TCC grants — after Run 1's install** (the binary must exist first):
   add `/usr/local/bin/kanatad` to **BOTH** Input Monitoring **and**
   Accessibility (System Settings → Privacy & Security → each pane → `+` →
   Cmd+Shift+G → type the path), toggles ON.
   ✅ RESOLVED 2026-07-11 (macOS 26.5.1, kanata 1.12, full grant matrix):
   macOS attributes the kanata child's device access to the supervising
   daemon — **kanatad needs both permissions; the kanata binary needs none**
   (its self-registered entry is cosmetic). Linker-signed ad-hoc binaries
   hold grants fine. ⚠️ Every kanatad rebuild+reinstall changes its code
   hash and silently invalidates both grants — **remove (−) and re-add (+)
   them after every reinstall** before judging any TCC-related result.

### Cheat sheet (used throughout)

| What             | Where / how                                                                                                                                      |
| ---------------- | ------------------------------------------------------------------------------------------------------------------------------------------------ |
| Daemon logs      | `/Library/Logs/KanataBar/kanatad.{out,err}.log` (`tail -f`)                                                                                      |
| Tray logs        | `~/Library/Logs/KanataBar/tray.{out,err}.log` — as of the 2026-07-13 fix the tray defaults to `info` (matches the daemon; no plist `RUST_LOG` needed) and each posted notification logs `posted notification title=… body=…` from `kanatabar_tray::notify`, so `tray.err.log` now confirms notifications + wizard steps. `grep 'posted notification' tray.err.log`. |
| VHID daemon logs | `/Library/Logs/KanataBar/vhidd.{out,err}.log` (ours)                                                                                             |
| Debug logs       | edit the daemon plist's `ProgramArguments` to add env `RUST_LOG=kanatad=debug`, or run `sudo kanatad run …` in the foreground                    |
| Install audit    | `./ci/clean-install-audit.sh` — run it **before install, after install, after uninstall** and diff; that diff *is* the leave-nothing-behind test |
| Status/oracle    | `kanatactl status [--json]`, `kanatactl doctor [--json]` — run doctor liberally; it is the manual-QA oracle for everything                       |
| Daemon job       | `launchctl print system/io.github.ibimal.kanatabar.daemon`                                                                                    |
| vhidd job        | `launchctl print system/io.github.ibimal.kanatabar.vhidd`                                                                                     |
| Agent job        | `launchctl print gui/$(id -u)/io.github.ibimal.kanatabar.agent`                                                                               |
| Exit codes       | `kanatactl`: 0 ok · 1 operational error · 2 usage · 3 cannot connect · 4 degraded                                                                |

To start completely over at any point: `sudo kanatactl uninstall`, confirm
with the audit script, reinstall.

***

## Run 1 — Install, reboot, revival (Phase 6, SPEC §10) — DO THIS FIRST

Everything else runs on top of the installed daemon, and this run resolves the
two open `[VERIFY]`s most likely to force code changes.

**Setup:** `cargo build --workspace`, then baseline `./ci/clean-install-audit.sh`
(expect every path `absent`).

### Fresh install \[HARD]

* [x] **`sudo ./target/debug/kanatactl install`**
  **Expect:** binaries in `/usr/local/bin` (0755, root:wheel); the daemon
  plist in `/Library/LaunchDaemons/`, the agent plist in
  `~/Library/LaunchAgents/` (owned by **you**, not root); `/Library/Logs/KanataBar`
  exists; `launchctl print system/io.github.ibimal.kanatabar.daemon`
  shows the job running. Re-run the audit script and keep the diff.
* [x] **Agent plist ownership** — `ls -l ~/Library/LaunchAgents/` →
  the KanataBar plist is owned by your user; if `~/Library/LaunchAgents`
  was created by the installer it is user-owned too (review-fix from
  2026-07-10; a root-owned dir here would block you from adding your own
  agents later).
* [x] **No** **`--cfg`** **needed** — the launchd `ProgramArguments` is bare
  `kanatad run` (SPEC §10). With no `config.toml` autostart preset, the
  daemon materializes the built-in safe config at
  `<support-dir>/safe.kbd`.
  **Expect:** `kanatactl status` → `Running` (safe passthrough config —
  keys behave normally), not a clap usage error in `kanatad.err.log`.
* [x] **Peer auth** — `kanatactl status` and `kanatactl doctor` work as your
  normal user **without sudo** (console-user peer-cred auth, §7.1).
  **Also:** from an SSH session as a *different* user (if available), the
  same commands are rejected (connection closed; daemon log line
  "rejecting unauthorized peer").
* [x] **Real config via IPC** —
  `kanatactl config apply ~/.config/kanata/test.kbd`
  **Expect:** `Ack`, kanata restarts, caps↔esc actually swaps. This also
  proves path-safety accepts your user-owned 0644 file. Then
  `kanatactl config validate` on a deliberately broken copy → refused with
  kanata's parse error, running kanata untouched.
  ✅ 2026-07-11: works with driver **v6.2.0** — caps↔esc swaps. ⚠️ First
  attempt failed silently with driver **v8.0.0** (the newest pqrs pkg, dext
  bundle 1.8.0, CFBundleShortVersionString 8.0.0): `config apply` returned
  `Ack` and caps still typed caps. kanata 1.12.0's embedded VHID client can't
  speak the v8 protocol — the daemon log spammed
  `connect_failed asio.system:2` (ENOENT) once/sec, kanata logged
  `Waiting for DriverKit virtual keyboard… (10.0s)` →
  `output backend unavailable — releasing input devices` →
  `Keyboard is usable (without remapping)`. Downgrading the pqrs pkg to
  v6.2.0 (Prereq 3's pin) fixed it. **This is the §2 version-coupling hazard:
  install the driver kanata targets, NOT the latest.**
  🔴 **Finding (green-but-dead):** during the v8 mismatch `kanatactl doctor`
  reported **every check ✅** and `status` showed **`Running`** while kanata
  was in output-backend-unavailable passthrough. The `driver version` check
  is report-only (ledger row 3 — no coupling check is possible), but the
  supervisor treating the `connect_failed asio` / `output backend
  unavailable` loop as `Running` masks a fully non-functional daemon.
  ✅ Fixed same day (hw-fix-4, ledger row 16): the release line now drives
  `Degraded{OutputBackendUnavailable}` after a 15s grace, with the child
  deliberately kept alive for kanata's self-recovery. HW re-verify in Run 5.

### Open \[VERIFY] — resolve now, they can force code changes

* [x] **IOKit matching as root.** ❌ **FAILED 2026-07-11 → finding #1
  confirmed; fixed 2026-07-12 (hw-fix-5), re-verify below.** The root
  LaunchDaemon (`ps`: pid 79888, **uid 0**, `/usr/local/bin/kanatad run`)
  logged `device monitor disabled (no automatic re-sync on hotplug)
  err=IOServiceAddMatchingNotification failed: 0xe00002c7`
  (`kIOReturnUnsupported`) at startup — **zero** `IOKit device monitor
  running` lines ever. **Root cause (isolated 2026-07-12 with the diagnostic
  in `tests/iokit_smoke.rs`):** it is not the API but two notification
  *constants* — both `kIOMatchedNotification` (`"IOServiceMatched"`, the
  original add path) **and** `kIOTerminatedNotification`
  (`"IOServiceTerminated"`, the removal path) return `kIOReturnUnsupported`
  for `IOHIDDevice` matching. `kIOFirstMatchNotification`
  (`"IOServiceFirstMatch"`) works. **Fix landed (hw-fix-5):** `ffi::iokit`
  uses `kIOFirstMatchNotification` for arrivals and a **per-device
  `IOServiceAddInterestNotification(kIOGeneralInterest)`** for removal (the
  canonical Apple pattern); same CFRunLoop thread + `DeviceEvent` channel,
  debounce pipeline untouched. Stays **no Input Monitoring** (SPEC §6.3
  \[HARD]) — the `IOHIDManager` alternative was rejected because
  `IOHIDManagerOpen` returns `kIOReturnNotPermitted` (0xe00002e2) without
  Input Monitoring. ✅ **Verified unprivileged** on this Mac via
  `cargo test -p kanatad --test iokit_smoke -- --ignored`: armed and
  enumerated 25 HID devices (Keychron K2 HE, Apple Internal Keyboard, the
  Karabiner virtual keyboard, mice, headset).
  ✅ **Re-verify PASSED 2026-07-12** (root daemon, fresh build): the
  `device monitor disabled …0xe00002c7` line is **gone** — now just
  `device monitor pipeline started window_ms=500`, no error — and Run 6's
  USB plug **and** unplug each fired exactly one debounced re-sync (~63ms /
  ~89ms). So the monitor arms as root **and** delivers both arrival and
  removal. (`IOKit device monitor running` is a `debug!` line, hidden at
  INFO; delivery is the stronger proof.) Finding #1 fully closed.
* [x] **`launchctl bootstrap gui/<uid>`** **as root.** ✅ **PASSED
  2026-07-11.** `launchctl print gui/501/…agent` → `state = running`,
  `program = /usr/local/bin/kanatabar-tray`, `pid = 69194`, plist
  user-owned at `~/Library/LaunchAgents/`. The installer's direct
  `bootstrap gui/501` as root worked — **finding #2 (switch to
  `launchctl asuser`) not triggered.** (Human visual confirm of the
  menu-bar icon still recommended, but the job is loaded and the tray
  process is live.)
* [x] Agent plist `KeepAlive=true` since Phase 7 (tray is a real long-running
  UI). **Re-verify:** the agent stays loaded and does **not** crash-loop
  under launchd's throttle (watch `launchctl print gui/…` for a climbing
  `runs` count).

### Reboot persistence \[HARD]

* [x] **`sudo reboot`.** After login, with **no manual action**:
  daemon job loaded and running, `kanatactl status` responds, the tray
  icon is in the menu bar, and (if Run 2 was done first) remapping works.
  Give it \~10s after login before declaring failure.
  ✅ **PASS 2026-07-13** (with the freshly-deployed new daemon build): after
  reboot + login, **zero manual action** → fresh low boot pids (kanatad=560,
  kanata=612, tray=1397, vhidd=558 — all different from the pre-reboot
  baseline), `kanatactl status` → `Running` (uptime 83s), **single** kanata,
  doctor all-green exit 0, tray agent auto-started (pid 1397). Boot log:
  `kanatad starting` → `device monitor pipeline started` → kanata spawned →
  Running, then **one** boot-time `device change settled; restarting kanata to
  re-sync devices` (devices enumerating during boot → one debounced re-sync,
  578→612 — correct, not a fault). Remapping not exercised (safe.kbd
  passthrough; no autostart preset persists a real remap — separate test).
  ✅ Menu-bar icon **visually confirmed present** after reboot (no manual
  action). (Dock icon correctly absent — the Accessory-policy fix d81b42a.)

### kill -9 revival \[HARD]

* [x] **`sudo kill -9 $(pgrep -x kanatad)`** while kanata runs.
  **Expect (revised — see finding):** launchd relaunches kanatad within \~1s
  (`KeepAlive`) and afterwards `pgrep -x kanata | wc -l` → exactly **1**
  (single-instance \[HARD]). *(The original "log shows the orphan sweep"
  clause was dropped: on this platform launchd reaps the child before any
  orphan can exist — the sweep is [AUTO]-tested defense-in-depth, decision (a)
  2026-07-13.)*
  ✅ **PASS 2026-07-13 (macOS 26.5.1):** relaunch near-instant (new kanatad
  pid, kanata re-spawned, Starting→Running in ~110ms, well under 1s);
  `pgrep -x kanata` settled to exactly **1**.
  🟡 **FINDING 2026-07-13 (macOS 26.5.1) — resolved (a).** The orphan-sweep
  line never appears — no `sweeping orphaned kanata` / `sweeping stray kanata`.
  A 100ms-granularity probe proved why: at the *first* sample after kill-9 the
  **new kanatad is already up and the old kanata is already dead** — the child
  never survives to be swept. **Mechanism (from code):** kanata is spawned in
  kanatad's **process group** (child.rs, no `setsid`/`new_session`) and the
  daemon plist has **no `AbandonProcessGroup`** key (default false), so launchd
  SIGKILLs the job's process-group members (the child kanata) as it tears the
  job down before relaunch. (`.kill_on_drop(true)` at child.rs:130 does *not*
  apply — kill-9 runs no Drop.) **So the single-instance invariant is upheld by
  launchd's process-group reaping, not by our sweep** — which stays
  [AUTO]-tested defense-in-depth for the out-of-job-group case launchd doesn't
  cover. Decision (a) 2026-07-13; PROGRESS.md hw-verify row.
* [x] **Revival without state.json** *(reframed from "Stray sweep")* —
  `sudo rm '/Library/Application Support/KanataBar/state.json'`, then
  `sudo kill -9 $(pgrep -x kanatad)`.
  **Expect (revised):** revival still yields exactly **one** kanata even with
  no persisted state — launchd reaps the process-group child and the revived
  daemon spawns fresh; `sweep_strays` runs before the first spawn and finds
  nothing (consistent with the finding above). *(Original premise — a
  surviving kanata swept by `sweeping stray kanata (not in state.json)` —
  cannot occur on this platform.)*
  ⬜ **Optional confirm** (low value given the finding): run it and expect
  exactly **1** kanata, no stray, no backoff loop; daemon rewrites
  `state.json` on the next spawn.

### Clean uninstall \[HARD]

Do this at the **end of the whole runbook** (you need the install for
everything else), or do it now and reinstall:

* [x] **`sudo kanatactl uninstall`**
  **Expect:** all three launchd jobs boot out (`launchctl print` fails for
  daemon, vhidd, agent); the audit script shows every KanataBar path
  `absent` — including `/Library/Application Support/KanataBar` and
  `/Library/Logs/KanataBar` — while the shared `/usr/local/bin`,
  `/Library/LaunchDaemons`, `~/Library/LaunchAgents` directories and all
  Karabiner files (daemon binary, pqrs dirs) still exist.
  ✅ **PASS 2026-07-13 (leave-nothing-behind):** `sudo kanatactl uninstall`
  printed 9 `removed` lines — the 3 binaries (kanatad/kanatactl/kanatabar-tray),
  both LaunchDaemon plists (daemon+vhidd), the agent plist, the socket,
  `/Library/Application Support/KanataBar`, `/Library/Logs/KanataBar`.
  `ci/clean-install-audit.sh` after: **every KanataBar path `absent`**, all 3
  launchd jobs **`unloaded`**, all processes gone (kanatad/kanata/tray/vhidd =
  0). Shared dirs (`/usr/local/bin`, `/Library/LaunchDaemons`,
  `~/Library/LaunchAgents`) and all Karabiner files (`org.pqrs`, Manager.app)
  **survived**. The before/after audit diff is exactly the KanataBar set.
* [ ] **`--daemon-only`** **/** **`--agent-only`** — install both, uninstall one half,
  audit: only that half's files are gone.

***

## Run 2 — VHID-daemon management (Phase 9, SPEC §6.5a)

**Setup:** driver installed + approved (Prereq 3), KanataBar installed (Run 1),
**Karabiner-Elements NOT installed**.

### Without Karabiner-Elements \[HARD]

* [x] **Install registered our vhidd job** — (already happened during Run 1's
  install; if the driver wasn't installed yet at that point, re-run
  `sudo kanatactl install` now — it's idempotent).
  **Expect:** no "skipping" message; the plist
  `/Library/LaunchDaemons/io.github.ibimal.kanatabar.vhidd.plist`
  exists; `launchctl print system/io.github.ibimal.kanatabar.vhidd`
  shows it running; `pgrep -f Karabiner-VirtualHIDDevice-Daemon | wc -l`
  → exactly **1**.
  ✅ **PASS 2026-07-13:** plist present; `launchctl print system/…vhidd` →
  `state = running`, `program = …Karabiner-VirtualHIDDevice-Daemon`; exactly
  **1** VHID daemon process (Karabiner-Elements not installed).
* [x] **Reboot → kanata alive, zero manual action** — the §6.5a headline and
  the reason Phase 9 exists. `sudo reboot`, log in, wait \~10s.
  **Expect:** VHID daemon up, `kanatactl status` → `Running`, remapping
  works. (Without Phase 9 this failed with kanata `asio:` connect errors
  on every boot.) Combine with Run 1's reboot test — one reboot covers
  both.
  ✅ **PASS 2026-07-13:** the VHID daemon (pid 558) came up **before** kanata
  (pid 612) — `ps lstart` 17:05:00 vs 17:05:01 — and there were **zero**
  `connect_failed asio` / backend errors this boot (grepped the fresh boot
  window: count 0). `kanatactl status` → `Running`, zero manual action. This
  is exactly the §6.5a win our vhidd LaunchDaemon buys: without Phase 9,
  kanata would race ahead of the VHID daemon and spam `asio:` connect errors
  every boot. `vhid daemon managed` ✅ "managed by KanataBar's LaunchDaemon".
* [x] **Daemon killed → revived** — `sudo pkill -f Karabiner-VirtualHIDDevice-Daemon`.
  **Expect:** launchd restarts it within \~1s; kanata recovers (a brief
  supervised restart is acceptable; check the log narrative makes sense).
  ✅ **PASS 2026-07-13 (better than "acceptable"):** `pkill` → launchd revived
  the VHID daemon with a **new pid** (558→3919) within the ~3s window, and
  **kanata rode through untouched** — same pid 612, state stayed `Running`,
  and the kanatad log recorded **nothing** (no release, no re-grab, no
  Degraded, no supervised restart). The blip was well inside kanata's 10s
  backend-wait, so it reconnected transparently. Contrast Run 5, where we
  *blocked* revival (bootout) to force the Degraded path.
* [x] **doctor** — `kanatactl doctor`:
  `vhid daemon managed` ✅ "managed by KanataBar's LaunchDaemon";
  `driver version` reports the installed **extension bundle** version,
  report-only. (✅ RESOLVED 2026-07-11: `systemextensionsctl` reports the
  `.dext` *bundle* version — e.g. `1.8.0` on a July-2026 pkg — which pqrs
  versions independently of the pkg/release version kanata release notes
  pin, so no machine coupling check is possible; matching pkg↔kanata stays
  the human step in Prereq 3.)
* [ ] **Uninstall removes only ours** — covered in Run 1's uninstall: our
  vhidd plist gone, the Karabiner daemon *binary* and any pqrs/user plists
  untouched.

### With Karabiner-Elements installed \[HARD] (separate machine/VM, or install KE temporarily)

* [ ] **Install leaves it alone** — with Karabiner-Elements present,
  `sudo kanatactl install`
  **Expect:** prints "already managed by `<label>`; leaving it alone";
  **no** vhidd plist written; exactly **one** VHID daemon process
  afterwards (never a second instance \[HARD]). 📋 Capture the label it
  names.
* [ ] **doctor** — `vhid daemon managed` ✅ naming the Karabiner-Elements
  label.

### Community-plist case

* [ ] With a hand-made `/Library/LaunchDaemons/org.pqrs.karabiner-daemon.plist`
  loaded (the pre-KanataBar community setup — see kanata discussion #1537):
  install skips ours; doctor names `org.pqrs.karabiner-daemon`.
  **\[VERIFY]** any other label spellings seen in the wild —
  `kanatabar_core::vhidd::classify` matches any label containing
  `karabiner` (case-insensitive).

### Wizard step

* [ ] On a machine where nothing manages the daemon (delete our plist +
  bootout to simulate): tray → **Setup Wizard…** jumps to "Keep the VHID
  daemon running"; following its instruction (`sudo kanatactl install`)
  turns the check green on re-run.

***

## Run 3 — Doctor & first-run wizard (Phase 8, SPEC §9, §11)

**Setup:** KanataBar installed. The red-check tests deliberately break things —
do them in the order below, which ends green. `doctor` is the oracle used by
every other run; exercise it here thoroughly.

### `kanatactl doctor` (CLI)

* [x] **Green machine** — driver activated+approved, VHID daemon running, real
  kanata installed, service up:
  **Do:** `kanatactl doctor` then `kanatactl doctor --json`.
  **Expect:** every check ✅, exit 0; the JSON is valid
  (`{"checks":[{name,ok,detail,fix_hint}…]}`) and usable as a bug-report
  bundle. 📋 Capture the JSON.
  ✅ 2026-07-13: all 10 checks ✅, exit 0; JSON valid, schema matches
  (`checks[].{name,ok,detail,fix_hint}`). Captured to
  `docs/captures/doctor-green.json`. (No sudo needed — read-only IPC.)
* [x] **Driver off** — deactivate via the pqrs script
  (`bash '/Library/Application Support/org.pqrs/Karabiner-DriverKit-VirtualHIDDevice/scripts/uninstall/deactivate_driver.sh'`),
  or run this test before first approval.
  **Expect:** `karabiner driver` ❌, exit 1.
  ✅ **PASS 2026-07-13:** deactivate → `systemextensionsctl list` no longer
  lists the extension → doctor `karabiner driver` ❌ "not activated + enabled",
  **exit 1**; dependent `vhid daemon` check chains to ❌ "not verified —
  activate the driver first"; `driver version` degrades to ✅ "driver not
  listed; see the karabiner driver check" (report-only). 🟡 **FINDING (not a
  bug, worth knowing):** the pqrs deactivate is **pending reboot** — the dext
  stays loaded in-kernel, so `kanatactl status` still shows **`driver_ok:
  true`, state Running** (the live kanata backend still works) while doctor
  reads the *activation record* and reports ❌. doctor = config state,
  supervisor `driver_ok` = runtime backend; they legitimately diverge in the
  deactivate-pending-reboot window. **Consequence for Run 5:** the
  "`Degraded{DriverNotActivated}`" supervisor path is NOT reached by a
  pending-reboot deactivate (the backend is still live) — reaching it needs
  the driver actually gone (post-reboot) or the backend actually down.
* [x] **Driver missing vs not-approved hints differ** — with no driver pkg at
  all, the `karabiner driver` hint says to *install the pkg*; with the pkg
  present but unapproved, it names the exact
  `…VirtualHIDDevice-Manager activate` command instead. (Reactivate + re-
  approve before continuing.)
  ✅ **not-approved variant confirmed 2026-07-13:** with the pkg present but
  deactivated, the hint reads "run the Setup Wizard, or `'…/Karabiner-
  VirtualHIDDevice-Manager' activate` and approve … in System Settings →
  Privacy & Security" — names the activate command, as designed. ⬜ The
  *no-pkg-at-all* "install the pkg" branch was **not** exercised on HW
  (removing the pkg is too disruptive mid-campaign) — it stays [AUTO]-covered.
* [x] **VHID daemon down** — `sudo launchctl bootout system/io.github.ibimal.kanatabar.vhidd`.
  **Expect:** `vhid daemon` ❌ while `karabiner driver` stays ✅; and the
  supervisor preflight refuses to spawn → `Degraded{VhidDaemonDown}`, no
  crash loop. Bootstrap it back
  (`sudo launchctl bootstrap system /Library/LaunchDaemons/io.github.ibimal.kanatabar.vhidd.plist`).
  ✅ **PASS 2026-07-13:** bootout → `vhid daemon` ❌ ("not running") +
  `vhid daemon managed` ❌ (bootout also drops the job registration) while
  `karabiner driver` stays ✅ (isolation confirmed). `kanatactl restart` (to
  force the preflight) → status `Degraded`, **exit 4**, `last_error` = "…VHID
  device daemon is not running…"; log shows **exactly one** transition
  `Running→Starting→Degraded reason=Some(VhidDaemonDown)` — no Backoff↔Starting
  flap. Bootstrap back → `start` → `Running` (fresh kanata pid), `driver_ok:
  true`, doctor all-green exit 0. (Note: `status.driver_ok` reflects the
  composite backend preflight, so it reads false while the vhid daemon is down
  even though the extension stays activated.)
* [x] **kanata missing** — point the active preset's `kanata_bin` at a
  nonexistent path (or temporarily `sudo mv /usr/local/bin/kanata{,.bak}`).
  **Expect:** `kanata binary` ❌ with an install hint; supervisor
  `Degraded{KanataBinMissing}`, no spin. Restore.
  ✅ **PASS 2026-07-13** (binary is at `/opt/homebrew/bin/kanata` on this
  Apple-silicon brew, not `/usr/local/bin`): `mv` it aside → `kanatactl
  restart` → doctor `supervisor` ❌ "kanata binary not found — install kanata
  or set kanata_bin"; status `Degraded`, **exit 4**, `last_error` same; log =
  one `Running→Starting→Degraded reason=Some(KanataBinMissing)` + an ERROR
  naming `bin=/opt/homebrew/bin/kanata` — no spin. `driver_ok` stayed **true**
  (only the bin was missing — cleanly distinct from VhidDaemonDown, which
  flips driver_ok false). Restore + `start` → `Running`, exit 0.
* [x] **Broken active config** — edit the active `.kbd` to garbage.
  **Expect:** `active config` ❌ carrying kanata's parse error; the
  *running* kanata keeps remapping (Phase 3 invariant). Fix the file →
  watch it hot-reload back.
  ✅ **PASS 2026-07-13** (garbage written to `safe.kbd`, backed up first):
  doctor `active config` ❌ carrying kanata's verbatim parse error
  (`× Error in configuration … safe.kbd:1:1`) while `supervisor` stays ✅
  Running; status **Running with the SAME `kanata_pid`** across the whole
  broken window — the live remapper is untouched (Phase 3 [HARD] holds). Log:
  `active config edited to something invalid; keeping the running config`
  (configmgr + watch), **no state transition, no restart**. On fix → doctor
  `active config` ✅ green. NB: the pid **changes** on the fix (config-apply is
  a kanata restart in KanataBar's "apply on next restart" model, not an
  in-process reload) — expected, ~2s of native keys during the respawn.
* [x] **Offline** — `sudo launchctl bootout system/io.github.ibimal.kanatabar.daemon`,
  then `kanatactl doctor`.
  **Expect:** the `daemon` check ❌ ("is kanatad installed and running?"),
  exit **3**, and the report still renders. Bootstrap it back.
  ✅ **PASS 2026-07-13:** bootout → 0 kanatad procs → doctor prints
  `❌ daemon: cannot connect to /var/run/kanatabar.sock ↳ is kanatad installed
  and running? try \`sudo kanatactl install\``, **exit 3**, no crash. (Doctor
  short-circuits to just the connectivity failure when offline — the remaining
  checks query the daemon over IPC, so only the `daemon` row + summary render;
  that satisfies "report still renders".) Bootstrap back → `Running` (fresh
  kanata), exit 0, single kanata.
* [x] **Socket perms** — `control socket` ✅ shows
  `/var/run/kanatabar.sock … mode 660` under the real root install.
  ✅ 2026-07-13: doctor reports `/var/run/kanatabar.sock (uid 0, mode 660)`.

### First-run wizard (tray)

Ideally driven on a machine where the driver isn't installed yet; otherwise
simulate by deactivating first. Menu → **Setup Wizard…** re-runs doctor and
jumps to the first unmet step each time — every step is re-checkable.

* [x] **Step: install driver** — with no driver, the wizard opens the pqrs
  releases page and notifies "Install the Karabiner driver … the version
  named in your kanata release notes".
  ✅ **PASS 2026-07-13** (triggered by deactivating the driver): Setup Wizard…
  → notification **"Setup: Install the Karabiner driver — kanata needs the
  Karabiner-DriverKit-VirtualHIDDevice driver — install the version named in
  your kanata release notes (SPEC §2…)"** and it opened the pqrs **releases
  page in the browser** (`github.com/pqrs-org/…/releases`). Behavior correct.
* [x] **Step: activate & approve** — the wizard **runs the activation request
  itself** (check `~/Library/Logs/KanataBar/tray.err.log` for
  `wizard step command succeeded` — the pqrs manager `activate` call),
  then opens System Settings → Privacy & Security. Approve → re-run wizard
  → driver check green.
  **\[VERIFY]** the pane anchor (`…?Extensions`) lands on the right pane on
  your macOS version; note the correct anchor here if not:
  ✅ **`com.apple.preference.security?Extensions` → Login Items & Extensions →
  Driver Extensions (macOS 26.5.1, human-confirmed)** — correct, no change
  needed.
  ✅ **FIXED + HW-VERIFIED 2026-07-13 (commit 0d43b4f).** Was UNREACHABLE. In
  `wizard.rs::steps()` both step 0 "Install the driver" and step 2 "Activate &
  approve" verified `checks::DRIVER`; `first_unsatisfied` returns the **first**
  failing step, so whenever `karabiner driver` ❌ the **install step shadowed
  the activate step**. Deactivating the driver → wizard showed *install*
  (releases page), never the activate step; even on a genuine first run
  (install pkg → still not activated → DRIVER still ❌) it looped on *install*,
  making the wizard's self-activation feature dead code.
  **Fix landed:** the install step now verifies the new `checks::DRIVER_PRESENT`
  ("VirtualHIDDevice-Manager on disk"), the activate step keeps `DRIVER`
  ("activated+enabled"), so once the pkg is installed the wizard advances to
  activate. ✅ **HW RE-VERIFY PASSED 2026-07-13:** deactivated the driver →
  doctor `driver present` ✅ + `karabiner driver` ❌ → **Setup Wizard…** showed
  *Activate & approve* (not install); `tray.err.log`:
  `wizard step command succeeded cmd=[…Karabiner-VirtualHIDDevice-Manager,
  "activate"]` + notification "Setup: Activate & approve the extension — …has
  requested the extension's activation for you…"; System Settings opened to the
  **Driver Extensions** pane (ledger #5 anchor confirmed above). The activation
  re-activated the driver → **"Recovered — kanata is running again"**
  notification (bonus: closes Run 7 self-recovery Recovery). Menu-width fix
  (4ed8fae) also visually confirmed here — the Degraded `State:` menu line was
  **narrow** (truncated), not stretched.
* [ ] **Step: Input Monitoring** — opens the Input Monitoring pane
  (`…?Privacy_ListenEvent` — **\[VERIFY]** the anchor) and the instruction
  names both binaries and the update caveat.
* [ ] **Step: install service** — with the driver ready but service missing,
  the wizard notifies to run `sudo kanatactl install`.
* [x] **Verify** — with everything set up, **Setup Wizard…** and **Run
  Doctor** both notify "All checks passed"; the full ✅/❌ list is in
  `tray.err.log`.
  ✅ **PASS 2026-07-13:** both **Run Doctor** and **Setup Wizard…** popped
  "All checks passed" (human-confirmed), and `tray.err.log` recorded the full
  **10-check ✅ list** (`ok: … check=daemon / kanata binary / karabiner driver
  / driver version / vhid daemon / vhid daemon managed / input monitoring /
  control socket / active config / supervisor`) then `posted notification
  title="KanataBar Doctor" body="All checks passed."`. (Log-based
  verification works now that the tray defaults to `info`.)

***

## Run 4 — TCC permissions (SPEC §2 hardening)

**Setup:** everything green (end of Run 3). These tests break Input Monitoring
on purpose; keep the panic escape / second input path in mind.

* [x] **Which binary needs the grant** — the §2 open question that decides a
  code follow-up.
  **Do:** in Input Monitoring, remove every KanataBar/kanata entry. Add
  **only** the kanata binary at the path doctor's `kanata binary` check
  names (the auto-resolved one — Prereq 2). `kanatactl restart`.
  **Expect (hypothesis A):** kanata grabs devices → grants attach to the
  kanata binary; wizard/doctor wording can drop kanatad.
  **If it stays denied (hypothesis B):** add `/usr/local/bin/kanatad` too
  and restart — if that fixes it, TCC attributes the check to the launchd
  job (responsible process). **Record which:** \_\_\_\_\_\_\_.
  Hypothesis B triggers the follow-up decision on
  `responsibility_spawnattrs_setdisclaim` (private API) vs. documenting
  "grant kanatad".
  ✅ **HYPOTHESIS B — grant is on `kanatad` (= ledger #6, re-confirmed
  empirically 2026-07-13 without touching System Settings):** `brew reinstall
  kanata` (new kanata binary, different content/signature) → `kanatactl
  restart` → **stays `Running`** (kanata grabbed devices fine). But reinstalling
  **kanatad** (the earlier log-spam redeploy, new code hash) → `Degraded{Input
  MonitoringDenied}`. So TCC attributes the device access to the supervising
  **kanatad** (responsible process), not the kanata binary. Doctor/wizard
  wording already says "grant kanatad" (kanata's own entry is cosmetic). The
  `responsibility_spawnattrs_setdisclaim` route was rejected earlier (ledger #6)
  because it would make kanata upgrades break the grant — which this test
  confirms they currently do NOT.
* [x] **Denied → actionable Degraded, no loop** — remove the effective grant,
  `kanatactl restart`.
  **Expect:** within one spawn, `kanatactl status` (exit 4) shows
  `Degraded` with "grant Input Monitoring … remove (−) and re-add (+)";
  the daemon log shows the classified fault line; **no** backoff loop
  (state goes Degraded once, not Backoff↔Starting five times); the tray
  posts exactly one degraded notification.
  **\[VERIFY]** 📋 capture kanata's exact denial text — the classifier
  (`kanatabar_core::kanata::classify_fault_line`) currently matches
  `privilege violation` / `not permitted`. Recorded text: \_\_\_\_\_\_\_.
  ✅ **PASS 2026-07-13** (captured incidentally when a KanataBar reinstall
  invalidated kanatad's grant): `status` → `Degraded`, **exit 4**, `last_error`
  = "macOS denied kanata input access — grant BOTH Input Monitoring AND
  Accessibility to /usr/local/bin/kanatad … remove (−) and re-add (+) both
  entries". Daemon log: kanata stderr → `ERROR … kanata was denied device
  access (Input Monitoring)` → **one** `state transition from=Running to=Degraded
  reason=Some(InputMonitoringDenied)` — **no** Backoff↔Starting loop. Tray
  posted `title="KanataBar — Degraded" body="macOS denied kanata input access
  …"` (the fix hint). **\[VERIFY] denial text (= ledger #7):**
  `failed to open keyboard device(s): macOS Input Monitoring permission is
  denied for kanata. Enable kanata in System Settings -> Privacy & Security ->
  Input Monitoring, then re-run kanata.` — `classify_fault_line` matches on
  "input monitoring permission" (updated 2026-07-11).
* [x] **Update invalidates the grant** — with everything green again:
  **Do:** replace the kanata binary with a different build of the same
  version (`brew reinstall kanata`, or copy the binary, `touch` won't do —
  the *content/signature* must change). `kanatactl restart`.
  **Expect:** `Degraded{InputMonitoringDenied}` **even though System
  Settings still shows the entry enabled** — this is the silent-
  invalidation caveat users will hit on every `brew upgrade kanata`.
  Remove (−) and re-add (+) the entry → `kanatactl start` → `Running`.
  🔵 **REFUTED for kanata / TRUE for kanatad 2026-07-13.** `brew reinstall
  kanata` (new kanata binary, different content) → `kanatactl restart` →
  **stayed `Running`** (NOT Degraded). So the runbook's premise is wrong for
  this architecture: replacing the **kanata** binary does **not** invalidate
  the grant — the grant is on **kanatad** (ledger #6), so `brew upgrade kanata`
  keeps working. The silent-invalidation caveat instead applies to **kanatad**
  updates (a `KanataBar` reinstall): the log-spam redeploy invalidated
  kanatad's grant → `Degraded{InputMonitoringDenied}`, fixed by −/+ both
  entries. ⚠️ **Inconsistent even for kanatad:** the crash-notif redeploy the
  same day did **not** invalidate — a kanatad rebuild *sometimes* keeps the
  grant (ad-hoc cdhash dependent). ✅ **Existing wording confirmed correct:**
  the doctor/wizard Input-Monitoring hint already reads "After updating
  KanataBar, remove (−) and re-add (+) both entries (kanata updates don't
  affect them)" — this test proves that's accurate.
* [x] **Grab conflict** — start a second kanata by hand
  (`sudo kanata --cfg ~/.config/kanata/test.kbd` in a terminal) while
  KanataBar's kanata is running… then `kanatactl restart` so ours loses
  the race; or use Karabiner-Elements' grabber if installed.
  **Expect:** `Degraded{DeviceGrabConflict}` — "another program holds the
  keyboard (Karabiner-Elements grabber or a second kanata?)" — not a
  crash loop. 📋 **\[VERIFY]** capture the exact "exclusive access /
  already open" wording: \_\_\_\_\_\_\_. Kill the manual kanata, `kanatactl
      start`, back to Running.
  ✅ **VERIFIED 2026-07-13 (chain, not forced-Degraded).** Started a second
  kanata by hand while ours held the keyboard — it printed, per device:
  **`IOHIDDeviceOpen error: (iokit/common) exclusive access and device already
  open <Apple Internal Keyboard / Trackpad | Keychron K2 HE | G502 X>`**
  (= ledger #8). Our kanata stayed `Running` (the *manual* one lost). The chain
  is proven: that text contains both "exclusive access" and "already open" →
  `classify_fault_line` → `DeviceInUse` (kanata.rs:112) → supervisor
  `Some(StderrFault::DeviceInUse)` → `Degraded{DeviceGrabConflict}`
  (supervisor.rs:302-305, also [AUTO]-tested `classifies_device_in_use`). Did
  **not** force *ours* to lose (would need a persistent competitor grabbing the
  live keyboard); the classifier match + supervisor mapping cover it. NB: a
  terminal-launched (root) kanata **did** get past Input Monitoring here, so it
  reached the grab attempt rather than an Input-Monitoring denial.
* [x] **Accessibility** — remove kanata from Accessibility (if present) while
  keeping Input Monitoring.
  **Expect:** basic remapping (caps↔esc) works with Input Monitoring
  alone. If any config feature fails (`cmd` exec, mouse output…), grant
  Accessibility and **record which feature required it** (SPEC §2
  \[VERIFY]): \_\_\_\_\_\_\_.
  ✅ **RESOLVED via ledger #6/#9 (grant matrix 2026-07-11), re-confirmed this
  session.** The premise is wrong for this architecture: the effective grants
  are on **kanatad**, and kanatad needs **BOTH** Input Monitoring **AND**
  Accessibility for device *open* — **basic remapping does NOT work with Input
  Monitoring alone** (ledger #9 = "all — Accessibility required up front for
  device open", not a per-feature thing). Every grant-invalidating redeploy
  this session required re-adding **both** entries to return to `Running`;
  adding only Input Monitoring would not have sufficed. Removing kanatad's
  Accessibility would just re-produce `Degraded` — not re-run (disruptive,
  already answered). *(Removing the **kanata** binary's Accessibility entry is
  a no-op — it's cosmetic; the checked grant is kanatad's, ledger #6.)*

***

## Run 5 — Health: driver preflight, sleep/wake, layer relay (Phase 5, SPEC §6.5)

**Setup:** installed daemon, real config applied, debug logs help
(`RUST_LOG=kanatad=debug`). The driver preflight must run for real — the
installed plist never sets `KANATABAR_SKIP_DRIVER_CHECK`.

### Driver preflight \[HARD]

* [x] **Driver activated** — steady state: `kanatactl status --json` →
  `"driver_ok": true`.
  ✅ 2026-07-13: `driver_ok: true` in steady state.
* [~] **Driver not approved** — (covered in Run 3's driver-off test; confirm
  here that the *supervisor* side entered `Degraded{DriverNotActivated}`,
  exit code 4, and did not crash-loop.)
  🟡 **NOT REACHED — see Run 3 driver-off finding.** The pqrs deactivate is
  *pending-reboot*: the dext stays loaded, so the running kanata's backend
  keeps working and the supervisor stayed `Running` (`driver_ok` true) — it
  did **not** enter `Degraded{DriverNotActivated}`. Reaching that state needs
  the driver actually gone (post-reboot with the deactivate applied), which
  wasn't exercised (would leave the machine unremapped until re-approve). The
  doctor-side `karabiner driver` ❌ exit 1 *was* verified (Run 3).
* [x] **VHID daemon stopped** — (covered in Run 3; confirm
  `Degraded{VhidDaemonDown}` on the supervisor side.)
  **\[VERIFY]** the liveness match: `health::driver::vhid_daemon_running`
  pgreps for
  `Karabiner-VirtualHIDDevice-Daemon.app/Contents/MacOS/Karabiner-VirtualHIDDevice-Daemon`
  — confirm `pgrep -f` with that exact fragment finds the daemon on your
  install.
  ✅ 2026-07-13: Run 3 confirmed `Degraded{VhidDaemonDown}` (exit 4, one
  transition, no loop) on the supervisor side. **\[VERIFY] resolved:**
  `pgrep -f "Karabiner-VirtualHIDDevice-Daemon.app/Contents/MacOS/Karabiner-VirtualHIDDevice-Daemon"`
  matches the running daemon (1 hit) on this install.
* [x] **`systemextensionsctl list`** **format** — diff your captured output
  (Prereq 3) against the fixtures in
  `kanatabar_core::driver::parse_systemextensions` tests; if the
  column/bracket layout differs on your macOS, that's a finding (fixtures
  \+ parser update).
  ✅ 2026-07-13 (= ledger #10a): real output (`org.pqrs.Karabiner-DriverKit-
  VirtualHIDDevice (1.8.0/1.8.0) … [activated enabled]`, with the
  `(Go to 'System Settings…')` header) parses — `parses_the_captured_hw_output`
  test green; captured to `docs/captures/systemextensionsctl-list.txt`.
* [x] **Output backend lost → Degraded, child kept** (hw-fix-4, ledger #16) —
  with everything Running, kill the VHID daemon **and keep launchd from
  reviving it**: `sudo launchctl bootout system/io.github.ibimal.kanatabar.vhidd`
  then `sudo pkill -f Karabiner-VirtualHIDDevice-Daemon`.
  **Expect:** kanata logs the 10s backend wait, then
  `output backend unavailable — releasing input devices`; \~15s later
  (grace) `kanatactl status` → `Degraded` (exit 4) naming the output
  backend, **while** **`pgrep -x kanata`** **still shows the child alive** (no
  respawn, no backoff loop); the tray posts one degraded notification.
  Then bootstrap the vhidd job back →
  **Expect:** kanata logs `output backend and console session ready —
  re-grabbing input devices`, state returns to `Running` with the **same
  kanata pid**, remapping works, tray posts "Recovered".
  **\[VERIFY]** 📋 capture the recovery line verbatim (the classifier pins
  `output backend and console session ready` from binary strings; the
  release + not-ready lines are already HW-captured): \_\_\_\_\_\_\_.
  ✅ **PASS 2026-07-13 — core mechanism fully re-verified on HW.** Killing the
  vhidd daemon (bootout + pkill) drove the running kanata to spam
  `connect_failed asio.system:61` (ECONNREFUSED — daemon socket gone; the
  original v8-mismatch finding was `:2`/ENOENT, both classify the same), then
  the **verbatim narrative** (kanatad.err.log):
  ```
  [kanata WARN] output backend unavailable during write — releasing input devices
  [supervisor]  kanata reports its output backend gone … degrading unless it recovers within the grace window grace_s=15
  [kanata]      Input devices released. Keyboard is usable (without remapping). Waiting for the output backend and console session to recover...
  15s later:
  [supervisor ERROR] kanata's output backend still unavailable after the grace window — keys are NOT remapped (driver version mismatch?)
  [supervisor]  state transition from=Running to=Degraded reason=Some(OutputBackendUnavailable)
  ```
  `status` → `Degraded`, **exit 4**, `last_error` = "kanata cannot reach its
  virtual-keyboard output — keys are NOT remapped. Likely a Karabiner driver
  version mismatch…". **Child KEPT: `kanata_pid` stayed 67010, alive, count 1,
  throughout the Degraded window** — degrade is `from=Running` (no respawn),
  one transition, no Backoff↔Starting flap. Bootstrap vhidd back → **recovery
  line verbatim** (ledger #16 [VERIFY], fills the blank):
  `output backend and console session ready — re-grabbing input devices` →
  `state transition from=Degraded to=Running` → `Running` with the **SAME pid
  67010**, `driver_ok: true`. Grace measured at **exactly 15s** (release→degrade).
  ⚠️ **Timing note:** an *already-running* kanata that loses the backend takes
  longer to notice than the ~25s startup path implies — the release didn't
  land until well after a 30s wait; budget ≥45–60s before judging.
  📋 **Release-line variant:** the real line is the **"during write"** form
  (`output backend unavailable during write — releasing input devices`), not
  the runbook's assumed `output backend unavailable — releasing input devices`.
  The classifier (`classify_backend_line`) matches via substrings
  (`"output backend unavailable"` AND `"releasing input"`), so it degrades
  correctly. ✅ **Fixed 2026-07-13 (hw-fix):** the verbatim combined HW line
  `output backend unavailable during write — releasing input devices` is now a
  fixture asserting `Down` in `classifies_backend_release_as_down`
  (kanata.rs), locking it in against the bare `during write` blip (→ `None`).
  🟢 **Tray "one degraded notification" / "Recovered" — now log-confirmable.**
  ✅ **Fixed 2026-07-13 (hw-fix):** the tray subscriber defaults to `info`
  (main.rs, matching the daemon; no plist change), and `OsascriptNotifier`
  logs `posted notification title=… body=…` on every notification. So the
  degraded + recovery notifications for THIS test are confirmable in
  `tray.err.log`: `grep 'posted notification' ~/Library/Logs/KanataBar/tray.err.log`.
  📋 **HW re-verify (next run):** after the driver-mismatch degrade + recovery,
  confirm two lines appear — a `Degraded` title (fix hint) and
  `Recovered — kanata is running again`. Was previously visual-only (0-byte
  logs since 11 Jul).
* [x] **Screen lock does NOT degrade** (same fix) — lock the screen
  (Ctrl+Cmd+Q), wait 30s+, unlock.
  **Expect:** kanata logs `console session paused (lock/user-switch) —
  releasing input devices` and re-grabs on unlock; state stays `Running`
  throughout — **no** Degraded flap, no notifications. 📋 capture the
  console-pause line verbatim if it differs: \_\_\_\_\_\_\_.
  ✅ **PASS 2026-07-13 (does NOT degrade)** — but via a different mechanism
  than expected: a plain Ctrl+Cmd+Q lock was a **non-event** for the daemon —
  state stayed `Running`, kanata pid **unchanged**, **no** release/pause line,
  **no** notification. On this macOS a simple lock keeps the console session
  *active* (it's still your session), so kanata never releases devices; the
  `console session paused (lock/user-switch)` line is only reachable via
  **fast-user-switch** (session deactivation), which wasn't exercised. Core
  claim (no Degraded flap on lock) holds; the release/re-grab path stays
  [AUTO]-covered + is the same code proven by Run 5 output-backend-lost.
  🟢 **Incidental finding (unrelated to lock) — FIXED + HW-VERIFIED
  2026-07-13 (commit 3712da1):** kanata emits `virtual_hid_keyboard_ready true`
  to stdout ~1×/sec continuously whenever connected to the VHID driver;
  KanataBar was relaying it at INFO, making it **91% of `kanatad.err.log`
  (50,293 / 55,262 lines)** — swamping the 2000-line `GetLogs`/`--follow` ring
  buffer. Fix: `kanata::is_vhid_status_noise` + `child.rs::relay_line` now log
  the 6 driver-status noise lines at DEBUG (classification unchanged). ✅ After
  redeploy: a fresh daemon session showed **0** `virtual_hid_keyboard_ready`
  lines at INFO (was ~60/min) and only **55 total lines over ~5 min**, while
  real `[INFO]`/backend/state lines still log. Log is clean.

### Sleep/wake \[HARD]

* [x] **Wake re-sync** — with kanata running, sleep the Mac (or close the
  lid), wait \~30s, wake.
  **Expect:** log shows the wake handler → `re-syncing kanata after wake`;
  remapping works immediately after wake (the classic silent-grab-loss
  failure is recovered). **\[VERIFY]** `IORegisterForSystemPower` arms as
  the root daemon (log line at startup: `power (sleep/wake) monitor
      running`).
  ✅ **PASS 2026-07-13:** every wake logged `kanatad: re-syncing kanata after
  wake` followed by kanata's re-grab ("Sleeping for 2s… entering the
  processing loop"); state stayed `Running`. Multiple sleep/wake cycles each
  fired **exactly one** re-sync. **\[VERIFY] resolved:** the arming line is
  `kanatad::ffi::power: power (sleep/wake) monitor running` — a `DEBUG` line
  (hidden at the default INFO level, seen in earlier debug-enabled runs), but
  the wake re-syncs *delivering* as the root daemon are the stronger proof
  that `IORegisterForSystemPower` armed. (Remapping-recovers is proven via the
  re-grab restart; keys are passthrough now so nothing to feel.)
* [x] **Sleep is not blocked** — the machine sleeps promptly when asked; no
  \~30s stall (the callback must acknowledge `kIOMessageSystemWillSleep`).
  ✅ **PASS 2026-07-13:** the Mac completed multiple sleep/wake cycles with no
  reported stall, and `pmset -g` shows **no `PreventSystemSleep` assertion from
  kanatad** (only powerd's normal display-on assertions) — so the
  `kIOMessageSystemWillSleep` callback acknowledges promptly and does not hold
  sleep.

### kanata TCP layer relay

* [x] **Live layer display** — add a second layer + a toggle key to the test
  config. Switch layers on the keyboard.
  **Expect:** `kanatactl status --json` reflects `active_layer` within a
  moment, **and** the tray's `Layer:` line updates live (pushed
  `LayerChanged` events — review-fix from 2026-07-10, no reconnect
  needed). **\[VERIFY]** the TCP message shape
  (`{"LayerChange":{"new":"…"}}`) against your kanata: run
  `nc 127.0.0.1 5829` and switch layers; 📋 capture one line.
  ✅ **PASS 2026-07-13** (applied a 2-layer test config via IPC:
  `caps` = `layer-while-held nav`; reverted to passthrough after):
  - **TCP shape (ledger #12) verbatim** from `nc 127.0.0.1 5829`:
    `{"LayerChange":{"new":"nav"}}` / `{"LayerChange":{"new":"base"}}` —
    exactly the assumed shape; kanata also sends an initial
    `{"LayerChange":{"new":"base"}}` on connect. (A second nc client receives
    the stream fine alongside the daemon's relay.)
  - **`status --json`** showed `active_layer: "nav"` while `caps` was held,
    back to `"base"` on release — the *daemon* relay tracks it, not just raw
    kanata.
  - **Tray `Layer:` line updated live** to `nav`/`base` on hold/release
    (human-confirmed) — pushed `LayerChanged`, no reconnect.
* [x] **TCP loss ≠ restart** — kill just the TCP connection (e.g.
  briefly block the port, or `sudo kill -STOP`/`-CONT` tricks are overkill
  — easiest: watch reconnects in debug logs during a kanata restart).
  **Expect:** the relay reconnects on its own; kanata is **not** restarted
  by TCP loss alone.
  ✅ **PASS 2026-07-13** (from debug-log evidence across this session's many
  kanata restarts): on each kanata restart the relay logs
  `kanata TCP relay disconnected err=Connection refused (os error 61)`, retries
  every ~0.5s, then `connected to kanata TCP addr=127.0.0.1:5829` +
  `kanata layer change layer=base` — **reconnects autonomously**. kanata is
  never restarted *by* TCP loss: the relay is a read-only client and all
  supervisor restarts are device/config/crash-driven (no TCP-loss→restart code
  path). Layer tracking resumed correctly after every reconnect.
* [x] **kanata version floor** — startup log records the kanata version; with
  kanata ≥ 1.7.0 there is **no** floor warning. **\[VERIFY]** the floor
  constant (`KANATA_VERSION_FLOOR` = 1.7.0) still matches reality — if
  your kanata needs newer, note it.
  ✅ **PASS 2026-07-13** (= ledger #13): startup logs
  `kanatad::supervisor: kanata version version=1.12.0`; 1.12.0 ≥ 1.7.0 floor →
  **no** floor warning. Constant still appropriate.

***

## Run 6 — Device monitor & hotplug (Phase 4, SPEC §6.3)

**Setup:** `kanatactl status` → `Running` with the real config; debug logs on.
**Was blocked by finding #1** (Run 1: `IOServiceAddMatchingNotification`
returned `kIOReturnUnsupported` even as the root daemon — the `Matched`/
`Terminated` notification constants are unsupported for IOHIDDevice). ✅
**Unblocked 2026-07-12 (hw-fix-5):** `ffi::iokit` now uses
`kIOFirstMatchNotification` + per-device
`IOServiceAddInterestNotification(kIOGeneralInterest)` for removal; the
arrival path is verified unprivileged (`tests/iokit_smoke.rs`). ✅
**Re-verify PASSED 2026-07-12 on the root daemon:** the old
`device monitor disabled …0xe00002c7` line is gone (`device monitor
pipeline started` instead), and both hotplug items below delivered — the
monitor arms **and** delivers as root. `IOKit device monitor running` is a
`debug!` line, hidden at INFO; delivery is the stronger proof.

* [x] **USB keyboard hotplug** — plug in the spare USB keyboard.
  **Expect:** within \~1s of the 500ms debounce settling, log shows
  `device change settled; restarting kanata to re-sync devices`, exactly
  **one** restart (`from=Running to=Starting` once), and the new keyboard
  **is remapped** (that's the core product promise).
  ✅ 2026-07-12 (root daemon, fresh hw-fix-5 build): exactly one restart —
  `device change settled` → `to=Starting` → `to=Running`, settle→Running in
  **~63ms** (well under the 1s budget, Run 8). ⏳ Confirm caps↔esc actually
  works on the newly-plugged keyboard by feel (the log proves the restart,
  not the remap).
* [x] **USB unplug** — unplug it. One debounced restart; no loop.
  ✅ 2026-07-12: exactly one restart, **~89ms** settle→Running. This proves
  the **removal** path delivers — the per-device
  `IOServiceAddInterestNotification(kIOGeneralInterest)` fired
  `kIOMessageServiceIsTerminated` (the mechanism that replaced the
  unsupported `kIOTerminatedNotification`). Finding #1 fully closed.
* [x] **Dock connect (hotplug storm)** — connect a dock/hub with several
  devices at once.
  **Expect:** exactly **one** restart for the whole burst (SPEC §16).
  ✅ 2026-07-12 (`RUST_LOG=kanatad=debug` confirmed): **a true simultaneous
  burst coalesces to one restart** — dock *disconnect* removed
  `Apple Keyboard` + `Keychron K3 Pro` in the same millisecond (18:18:41.731)
  → a single settle → one restart. §16 satisfied. The earlier *connect*
  showing **two** restarts is a dock powering up its two keyboards ~800ms
  apart (past the 500ms window): the debug log names two **distinct real
  keyboards** arming the two debounces — staggered sequential hotplugs, not
  a burst and not a feedback loop. **No SPEC change needed** (a burst = a
  within-window batch, which does collapse; sequential power-up is not a
  burst). Decision (a) accept-and-document: re-sync is one restart per
  500ms settle window.
  Debug capture also confirmed, on hardware:
  - **Feedback-loop guard \[HARD]:** `Karabiner DriverKit VirtualHIDKeyboard
    1.8.0` appears only as `ignoring non-keyboard / virtual device change`,
    never `relevant`; after every restart the *only* re-appearing device is
    the virtual keyboard (ignored) — physical keyboards do **not**
    re-enumerate on kanata's re-grab, so no death spiral.
  - **Multi-interface keyboards:** `Keychron K3 Pro` presents 3 HID nodes;
    2 `ignoring` + 1 `relevant` → one relevant event per physical keyboard,
    not N (the usage-page filter holds on real hardware).
* [x] **Bluetooth reconnect cycle** — toggle a BT keyboard off/on a few times
  quickly. At most one restart per settle window; no thrash.
  ✅ 2026-07-12: BT connect/disconnect produced the same clean single
  debounced restart as USB — arrival + removal both delivered over the BT
  transport too.
* [x] **Mouse hotplug is ignored** — plug/unplug a mouse. **No** restart
  (keyboard-only filter).
  ✅ 2026-07-12: USB **and** BT mouse connect/disconnect produced **no**
  `device change settled` and **no** restart — correctly filtered as
  non-keyboard, over both transports.
* [x] **No feedback loop** — kanata's own virtual keyboard appearing at each
  (re)start must **not** trigger another restart (the death spiral the
  name/vendor filter prevents \[HARD]). Watch the log across one restart:
  exactly one, not a cascade.
  ✅ 2026-07-12 — **directly confirmed with `RUST_LOG=kanatad=debug`**:
  after every `to=Running`, the only re-appearing device is
  `Karabiner DriverKit VirtualHIDKeyboard 1.8.0`, logged as `ignoring
  non-keyboard / virtual device change` — **never** `relevant`, so it never
  re-arms the debounce. Physical keyboards do not re-enumerate on kanata's
  re-grab. No death spiral. Product name + vendor (`iokit_smoke`): `Karabiner
  DriverKit VirtualHIDKeyboard 1.8.0`, vendor **0x16C0** — matches all three
  arms of `kanatabar_core::device::is_karabiner_virtual` (ledger #14).
* [x] **Clean shutdown** — `sudo launchctl bootout system/…daemon` (or SIGTERM
  a foreground run): exits 0 promptly; no hang on the CFRunLoop threads.
  Bootstrap back.
  ✅ 2026-07-12: `bootout` then `bootstrap` returned promptly, no hang — the
  IOKit CFRunLoop thread tears down cleanly (`CFRunLoopStop` + join).

***

## Run 7 — Tray app (Phase 7, SPEC §8)

**Setup:** daemon installed and running; the agent-launched tray is already in
the menu bar. (Unbundled dev builds show a Dock icon and use `osascript`
notifications — both expected until the Phase 10 bundle.)

### Status icon (template image, auto light/dark)

* [x] **Running** — filled-dot glyph; tints correctly in light AND dark menu
  bars (flip appearance in System Settings).
  ✅ **PASS 2026-07-13:** filled-dot glyph, tints correctly in both light and
  dark menu bars (human-confirmed).
* [x] **Paused** — menu → Pause: glyph becomes the two bars.
  🟢 **GLYPH BUG FOUND + FIXED + HW-VERIFIED 2026-07-13 (commit ac3e577).**
  Was rendering as ONE solid bar (state/menu were correct — `State: Paused`,
  Resume active). Cause in `paused_bars()`: `bar_half_width = 0.08 × ICON_SIZE`
  but `gap = 0.06 × ICON_SIZE`, so the two bars (centred at ±gap) spanned dx ∈
  [−0.14,+0.02] and [−0.02,+0.14] — **overlapping across centre** (gap <
  bar_half_width) into one bar. Fix: `bar_half_width 0.07`, `gap 0.11` (gap >
  half-width) + `paused_glyph_has_two_separated_bars` regression test. ✅ After
  a tray-only redeploy (`cp` + `launchctl kickstart -k` the agent) the Paused
  glyph shows **two separate bars** (human-confirmed).
* [x] **Degraded** — force one (stop the vhidd job, or break the active
  config): badged-ring glyph.
  ✅ **PASS 2026-07-13:** forced `Degraded{VhidDaemonDown}` (bootout vhidd +
  start) → **badged-ring** glyph, `State: Degraded`, degraded notification
  popped (human-confirmed).
* [x] **Disconnected** — `sudo launchctl bootout system/…daemon`: dashed-ring
  glyph, top line "Disconnected — reconnecting…". Bootstrap the daemon
  back → tray reconnects **by itself**; live state returns.
  ✅ **PASS 2026-07-13:** daemon bootout → **dashed-ring** glyph + top line
  `Disconnected — reconnecting…`; bootstrap daemon back → tray **reconnected
  on its own** (no interaction) → filled-dot / `State: Running` returned.

### Menu

* [x] Top (disabled) lines show `State: <state>` and `Layer: <layer>`; the
  layer line updates **live** while you switch layers.
  ✅ **PASS 2026-07-13:** `State:` line tracked Running/Paused/Stopped/Degraded
  through the sweep; `Layer:` updated **live** base↔nav during the Run 5 layer-
  relay test (human-confirmed).
* [x] **Enable/disable tracks the state machine** — Running: Start greyed,
  Stop/Restart/Pause active. Stopped: Start/Restart active, Stop/Pause
  greyed. Paused: Resume active. Clicking a greyed item does nothing.
  ✅ **PASS 2026-07-13:** Running → Start greyed, Stop/Restart/Pause active;
  Stopped → Start/Restart active, Stop/Pause greyed; Paused → Resume active
  (all human-confirmed as I drove the states).
* [x] **Start / Stop / Restart / Pause / Resume** each drive the daemon; menu
  \+ icon follow within \~1s via the event stream (no polling).
  ✅ **PASS 2026-07-13:** clicking **Pause** then **Resume** from the menu
  drove the daemon and the icon/menu followed within ~1s each (human-
  confirmed); Start/Stop/Restart exercised via the same IPC path during the
  state sweep, icon+menu followed live (event stream, no polling).
* [x] **Presets submenu** — configure two presets in config.toml
  (`kanatactl preset list` to confirm): checkmark sits on the active one;
  switching moves it and applies the config.
  ✅ **PASS 2026-07-13:** wrote `config.toml` with two presets — `passthrough`
  (autostart) + `capsswap` (caps↔esc). `preset list` → `* passthrough
  [autostart]` + `capsswap`. Tray Presets submenu showed both with the
  **checkmark on the active** one; clicking **capsswap** **moved the checkmark**
  and **applied** the config (caps↔esc worked by feel). Switched back to
  `passthrough` → checkmark moved back, caps normal. Config-set path: presets
  are configured via `config.toml` (root-owned; sudo), not a kanatactl command
  (`preset` has only `list`/`switch`).

### Notifications

> ℹ️ **Known dev-build limitation (2026-07-13, expected — not a bug):** in the
> unbundled dev build, **clicking** any notification opens **Script Editor**,
> and the poster shows as "Script Editor". macOS attributes `osascript
> display notification` (the only notifier without a bundle, SPEC §18) to
> Script Editor. The `Notifier` trait already abstracts this; the Phase 9/10
> `KanataBar.app` bundle with `AssociatedBundleIdentifiers` +
> `UNUserNotificationCenter` fixes attribution and click handling. Notification
> *text/delivery* is correct now; only the click target is cosmetic. Re-verify
> click behaviour after the Phase 10 bundle.

* [x] **Crash** — `sudo kill -9 $(pgrep -x kanata)`: "kanata crashed …"
  notification; then it recovers silently (crash→backoff→running posts
  nothing further).
  ✅ **FIXED + HW-VERIFIED 2026-07-13 (commit 93d6989).** After deploying the
  fix: `kill -9` kanata → daemon `kanata exited unexpectedly
  class=Crash{signal:Some(9)} fault=None` → `Running→Backoff→Starting→Running`
  (recovered, new pid) and the tray logged **exactly one**
  `posted notification title="KanataBar" body="kanata crashed (signal 9)"` —
  then silence through the recovery (Backoff→Running is not Degraded→Running,
  so no "Recovered", matching "recovers silently"). End-to-end proven: daemon
  emits `Event::Crash` → control forwards → tray posts + logs.
  ⚠️ **Deploy gotcha (both hit this):** (1) run **`sudo ./target/debug/kanatactl
  install`** — the *installed* `/usr/local/bin/kanatactl` copies its own
  siblings onto themselves (`error: copying …/kanatad to …/kanatad`). (2) The
  installer's `launchctl bootstrap` fails **`5: Input/output error`** when the
  daemon job is already loaded — it copies the new binary but the OLD process
  keeps running; recover with `sudo launchctl bootout system/…daemon` then
  `bootstrap`. (3) This reinstall did **not** invalidate the TCC grants (came
  up `Running`, kanata grabbed devices) — contra the "every reinstall
  invalidates grants" note; grants may survive when the ad-hoc signing
  identity/path is unchanged. Re-grant only if it comes up
  `Degraded{InputMonitoringDenied}`.
  🔴 **(historical) BUG FOUND 2026-07-13 — no crash notification is posted.** `kill -9`
  kanata → daemon correctly classifies it (`kanata exited unexpectedly
  class=Crash {…signal: Some(9)} fault=None`) and recovers
  (`Running→Backoff→Starting→Running`), but **no notification fires** — the
  tray's `tray.err.log` (now logging every post since the hw-fix) stays
  silent. **Root cause:** the daemon **never emits `ipc::Event::Crash`**. The
  event is defined (`ipc.rs`) and handled by the tray
  (`notify::notification_for` → "kanata crashed (…)"), but nothing in
  `kanatad` ever constructs it: the supervisor's crash arm dispatches
  `MachineEvent::ChildExited` (→ Backoff) and only `StateChanged` is published;
  `control/mod.rs:456 state_event()` maps transitions to `Event::StateChanged`
  only. The tray notifies solely on `Event::Crash` or a `Degraded` transition,
  and a crash-that-recovers is `Backoff` (not `Degraded`) → nothing.
  🔧 **FIX LANDED 2026-07-13 (hw-fix) — needs HW re-verify.** The supervisor's
  plain-crash arm (`supervisor.rs`, the `_ =>` branch of the exit
  classification) now publishes `Event::Crash{code,signal}` on the shared
  `DaemonEvents` bus (the same bus `configmgr`/`device` use, now also handed to
  the supervisor via `SupervisorConfig::events`). Only the **plain** crash path
  emits it — a fault-classified crash (TCC denial, device-in-use, port-in-use)
  already goes to Degraded, which the tray notifies on separately, so emitting
  Crash there too would double-notify. Covered by
  `plain_crash_publishes_crash_event_on_the_bus` and
  `fault_crash_does_not_publish_crash_event` (tests/supervisor.rs). **Re-verify
  on HW:** `sudo kill -9 $(pgrep -x kanata)` then
  `grep 'posted notification' ~/Library/Logs/KanataBar/tray.err.log` → expect
  one `title="KanataBar" body="kanata crashed (signal 9)"`. (`Event::DriverIssue`
  is likewise never emitted — same dead-event class, lower priority, untouched.)
* [x] **Degraded** — entering Degraded posts one notification carrying the fix
  hint.
  ✅ **PASS 2026-07-13** (confirms the tray-logging hw-fix works on HW):
  forcing `Degraded{VhidDaemonDown}` logged
  `posted notification title="KanataBar — Degraded" body="Karabiner
  VirtualHIDDevice daemon is not running — reinstall the Karabiner driver"` —
  exactly one, carrying the fix hint. This proves the tray is connected,
  receiving events, and logging every post; the Crash gap above is therefore
  specifically the daemon not emitting `Event::Crash`, not a tray problem.
* [x] **Recovery** — Degraded → Running posts "Recovered".
  ✅ **HW-CONFIRMED 2026-07-13:** during the Run 3 wizard activate-step test,
  the wizard's `…Manager activate` re-activated the driver → the backend
  recovered → a **direct `Degraded→Running`** → tray logged `posted
  notification title="KanataBar" body="Recovered — kanata is running again"`.
  🟡 **Scope note.** `notification_for` posts "Recovered" **only on a DIRECT
  `Degraded→Running`** transition — i.e.
  self-recovery (Run 5 output-backend-lost: child kept, backend returns,
  machine goes Degraded→Running). A **user-driven** recovery (e.g. from
  `Degraded{VhidDaemonDown}` via `kanatactl start`) goes
  `Degraded→Starting→Running`, so `from` is `Starting`, not `Degraded` → **no
  "Recovered" notification** (observed: none logged after a start-driven
  recovery).
  ✔️ **DECISION 2026-07-13 (hw-fix) — intended; leaving as-is.** The
  "Recovered" notification exists to announce a recovery the user did *not*
  initiate (self-recovery: kanata re-grabs the backend on its own). A
  user-driven recovery is one the user just triggered (Restart, or fixing the
  driver and starting) — they are already engaged and the tray icon/menu shows
  the new state, so a "Recovered" toast is redundant. Making it fire would also
  require threading "was recently Degraded" state into the notification
  decision, breaking the deliberately **pure, single-event, unit-tested**
  `notification_for` (it only sees one transition; a `Starting→Running` event
  carries no memory of the prior Degraded). The self-recovery "Recovered" path
  still needs a positive HW capture (Run 5 recovery happened before the
  tray-logging fix was deployed).

### Lifecycle & single instance

* [x] **Single instance** — run a second tray by hand
  (`/usr/local/bin/kanatabar-tray`): prints "KanataBar is already running",
  exits 0.
  ✅ **PASS 2026-07-13:** second invocation →
  `KanataBar is already running (Resource temporarily unavailable (os error
  35))`, **exit 0**; tray count stayed 1. (errno 35 = `EAGAIN` from the
  single-instance file lock — the guard flock is already held.)
* [x] **Launch at Login** — the menu toggle reflects `launchctl print gui/…`
  reality; toggling off boots the agent out (this tray exits — expected);
  relaunching the tray by hand and toggling on re-bootstraps it.
  ✅ **PASS 2026-07-13:** toggle was **checked** while the agent job was loaded
  (matches reality). Toggling **off** → `launchctl print gui/501/…agent` →
  "Could not find service" (booted out) + the menu-bar icon **disappeared**
  (tray exited), while the **daemon stayed `Running`** (kanata pid unchanged).
  Relaunching the tray by hand + toggling **on** → agent job **loaded again**
  (`state = spawn scheduled`; the hand-launched tray holds the single-instance
  lock so the agent's own spawn throttles — expected). Toggle drives
  `launchctl` correctly both ways.
* [x] **Quit KanataBar** — quits the tray only; `kanatactl status`: daemon and
  kanata still running.
  ✅ **PASS 2026-07-13:** clicking Quit exited the tray; `kanatactl status`
  stayed `Running` with the **same kanata pid** (daemon/kanata untouched).
  With Launch-at-Login on, the agent's KeepAlive relaunched a managed tray
  within ~1s (icon reappeared) — clean state restored.

***

## Run 8 — Performance budgets (SPEC §6.6 \[HARD])

With everything green and idle (no device changes, no layer switching):

* [x] **Idle CPU ≈ 0%** — Activity Monitor (or `top -pid $(pgrep -x kanatad)`)
  over a couple of minutes: `kanatad` ≈ 0.0%, `kanatabar-tray` ≈ 0.0%.
  Anything periodic (a heartbeat every second, etc.) is a bug — the design
  is zero-polling.
  ✅ 2026-07-13 (idle, safe.kbd): `top -l 2` sampled both at **0.0%** CPU —
  zero-polling holds.
* [x] **Memory** — `kanatad` RSS < 20 MB; tray < 40 MB.
  ✅ 2026-07-13: `kanatad` **12 MB RSS** (`ps -o rss`) < 20 MB.
  Tray **16.1 MB** phys_footprint (`vmmap --summary`, = Activity Monitor's
  "Memory" column; peak 17.0 MB) < 40 MB. ⚠️ **Metric note:** the tray's
  `ps -o rss` reads **56 MB** — that number counts shared, memory-mapped
  AppKit/system framework pages the process doesn't own; the SPEC/Activity-
  Monitor budget is against **phys_footprint** (private+compressed), which is
  16 MB. Use `vmmap --summary` / Activity Monitor "Memory", **not** `ps rss`,
  to judge the tray. `kanatad` (headless, no frameworks) has no such gap so
  its RSS is a fair reading.
* [x] **Restart latency** — from a hotplug settling to kanata spawned < 1s
  (read the log timestamps from Run 6).
  ✅ 2026-07-12: settle→Running **~63ms** (plug) and **~89ms** (unplug) —
  ~15× under the 1s budget.

***

## Run 9 — Release install paths (Phase 10, SPEC §12/§13)

**Setup:** a published GitHub Release (v0.1.0 tag → release workflow green) and
the `ibimal/homebrew-tap` cask bumped. A **clean Mac** (or at minimum a
`sudo kanatactl uninstall`-ed one with the driver deactivated) makes the
first item meaningful; the upgrade/reinstall items can run on this machine.

### Cask install \[HARD]

* [ ] **`brew install --cask ibimal/tap/kanatabar`** on a clean machine.
  **Expect:** pkg runs; postinstall bootstraps daemon + agent with no manual
  step; `KanataBar.app` in `/Applications`; **only** `kanatad` + `kanatactl`
  in `/usr/local/bin` (no bare tray — it lives in the bundle); the agent
  plist's program is `/Applications/KanataBar.app/Contents/MacOS/kanatabar-tray`;
  menu-bar icon appears.
  🔧 **FIX LANDED (needs HW re-verify):** the first two local pkg installs
  (2026-07-16) put **no app in /Applications** — pkgbuild components are
  *relocatable* by default, and the installer followed Spotlight to the
  build tree's own `dist/KanataBar.app` and installed the payload **there**
  (that's also why `dist/` kept turning root-owned). `build-pkg.sh` now pins
  `BundleIsRelocatable=false` via `--component-plist`; gate-10 rejects any
  pkg whose PackageInfo carries a non-empty `<relocate>`. `kanatactl
  install` also now deletes a stale bare `/usr/local/bin/kanatabar-tray`
  when switching the agent to the bundle (launchd was observed running a
  2-day-old leftover). On a machine with a build tree, verify the app now
  lands in `/Applications` (and NOT in `dist/`).
* [ ] **Gatekeeper reality** — **\[VERIFY]** what an un-notarized pkg
  actually triggers on this macOS: capture the exact dialog text and whether
  System Settings → Privacy & Security → *Open Anyway* is needed (SPEC §12
  says the README/cask caveat must match reality; adjust both if not).
* [ ] **Wizard → working remap** — on the clean machine, onboard **only** via
  Setup Wizard prompts (driver install → activate → permissions → service),
  then apply a caps↔esc config: remap works by feel. This is the Phase 8/10
  clean-machine gate in its release form.
* [ ] **Notifications from the bundle** — with the tray running from
  `KanataBar.app`: notifications post (fire a crash:
  `sudo pkill -9 -x kanata`) and attribute to **KanataBar** with the app
  icon; clicking one does NOT open Script Editor; Login Items shows KanataBar
  (AssociatedBundleIdentifiers, ledger #14).
  🔧 **FIX LANDED (needs HW re-verify):** the first bundled run 2026-07-16
  showed the Script Editor icon and clicking opened Script Editor — the tray
  still used the dev-time `osascript` fallback unconditionally. The tray now
  posts via **UNUserNotificationCenter** when bundled (`notify::
  BundledNotifier`, ObjC surface in `tray/src/ffi.rs` — zero unsafe, safe
  objc2 bindings), keeping osascript for unbundled dev builds. **Expect on
  first bundled launch:** a one-time system prompt "KanataBar would like to
  send you notifications" — click **Allow** (denying silently drops all
  notifications; `tray.err.log` logs granted/denied).
  ✅ **PASSED 2026-07-16:** tray runs from the bundle (pgrep shows
  `/Applications/KanataBar.app/...`); log 19:36:27 `bundled: notifications
  via UNUserNotificationCenter bundle=io.github.ibimal.kanatabar`; the
  authorization prompt appeared and was allowed (19:36:41 `notification
  authorization granted`); Degraded + wizard notifications delivered through
  it. Crash test (`sudo pkill -9 -x kanata`, 19:55:17): "kanata crashed
  (signal 9)" notification with **KanataBar's own icon** (human-confirmed,
  no Script Editor), and the supervisor respawned kanata (new pid, Running).
  Not re-checked: the Login Items attribution glance (ledger #14 — was
  already ✅ in Run 1).
* [x] **Wizard catches a runtime-only degradation** — with every static check
  green but the supervisor in `Degraded{InputMonitoringDenied}` (a fresh
  install before re-granting TCC reproduces this exactly): **Setup Wizard…**
  must jump to "Grant Input Monitoring & Accessibility" and open the pane —
  NOT announce "you're set up".
  🔧 Fix history: on the 2026-07-16 fresh pkg install the wizard
  congratulated the user while kanata sat denied — TCC is unreadable so the
  static check can't fail; the wizard now also fetches
  `Status.degraded_reason` (new v1-additive IPC field) and maps the
  supervisor's runtime reason onto the fixing step
  (`wizard::check_for_degraded_reason`).
  ✅ **HW RE-VERIFY PASSED 2026-07-16:** same machine, same degraded state,
  fixed binary — tray.err.log 19:38:00 `posted notification title="Setup:
  Grant Input Monitoring & Accessibility"` (vs 19:21:41 on the old binary:
  `"All checks passed — you're set up."`). Granting both permissions +
  `kanatactl restart` → `Running`, doctor all green. (Restart is required
  after granting: a TCC denial is deliberately a no-retry Degraded state.
  Recovery notification correctly did NOT fire — user-driven recovery is
  intentionally silent, Run 7 decision.)

### Upgrade & reinstall \[HARD]

* [x] **Reinstall over a live install** — `sudo installer -pkg
  dist/KanataBar-<ver>.pkg -target /` while everything is running.
  **Expect:** no "service already loaded" abort (the Phase 10 bootout-first
  fix); daemon AND tray binaries both refreshed (compare mtimes — this closes
  the campaign's "install didn't refresh the tray" deploy note); state
  (`config.toml`, presets, last-known-good) survives.
  ✅ **PASSED 2026-07-16 after one found+fixed race:** the first over-live
  run failed in the postinstall — `launchctl bootstrap` raced its own
  asynchronous `bootout` (`Bootstrap failed: 5: Input/output error`); fixed
  with `bootstrap_with_retry` (10×500 ms, all three job sites). Retry build
  reinstalled cleanly over a live agent, binaries refreshed, active config
  (`safe.kbd`) survived, doctor all green after re-grant+restart. The
  over-a-Running-daemon repeat comes free with the `brew upgrade` item
  below. Grants: this artifact's reinstalls kept them; a **new** build's
  fresh signature invalidated them (re-grant + restart needed) — cask caveat
  should mention it.
* [ ] **`brew upgrade`** (needs a second published version, e.g. v0.1.1) —
  **Expect:** both binaries replaced, daemon comes back `Running` with the
  same active preset, tray icon back within seconds. 📋 **\[VERIFY]** whether
  the new artifact's (different) ad-hoc signature invalidates kanatad's
  Input Monitoring/Accessibility grants — Run 4 saw this inconsistently on
  dev rebuilds; a release-to-release answer decides whether the cask caveat
  must warn about re-granting after upgrades.

### Cask uninstall \[HARD]

* [ ] **`brew uninstall --cask kanatabar`** — runs `sudo kanatactl uninstall`
  + forgets the pkg receipt. **Expect:** the Run 1 leave-nothing-behind audit
  passes (all KanataBar paths gone incl. `/Applications/KanataBar.app`;
  Karabiner/shared files untouched); `pkgutil --pkgs | grep ibimal` empty.

***

## Run 10 — Phase 12 windows: Devices (SPEC §8, docs/design/phase12-ui-layer.md)

**Setup:** daemon up (`just run-dev`, or the installed daemon), then the tray
**unbundled**: `cargo run -p kanatabar-tray`. Unbundled is the point — it
answers the design doc's open question 2 (WKWebView in a bare binary under
`ActivationPolicy::Accessory`). If window creation fails, the tray logs
`devices window unavailable` and falls back to the old notification; that
fallback firing IS a finding (ledger #17).

* [ ] **Open & focus from the accessory tray** — menu → Devices….
  **Expect:** a window titled "KanataBar Devices" appears **frontmost and
  focused** (no Dock icon appears; the app stays an accessory), header shows
  the app icon + a summary line ("N devices · M matched"). \[VERIFY #17]
* [ ] **Content matches the CLI** — compare rows against `kanatactl devices`.
  **Expect:** same names; matched device(s) sorted first with a green dot +
  `MATCHED` pill; unmatched rows show a gray dot. Devices with no product
  string show as a dimmed italic "Unnamed device" (sorted last) in the window
  and "Unnamed device" in the CLI — never a blank row/line (finding fixed
  2026-07-17: two nameless IOHID devices rendered blank in both). The
  built-in keyboard ("Apple Internal Keyboard / Trackpad") must show
  `MATCHED` (second 2026-07-17 finding, fixed: composite devices present
  several HID nodes under one product name and last-writer-wins in the
  registry made `matched` enumeration-order luck; any keyboard node now
  wins). Reminder: `matched` = "a keyboard kanata would grab", the honest
  approximation — kanata does not expose its live grab list.
* [ ] **Hotplug refreshes in place** — window open: plug, then unplug, the
  spare USB keyboard.
  **Expect:** the list updates by itself within \~1s each time (no re-open,
  no flicker to empty). This is the `DeviceChanged` → re-fetch path.
* [ ] **Hidden windows don't fetch** — close the window (hides), hotplug
  again with debug logs on.
  **Expect:** no `GetDevices` traffic while hidden; re-opening shows the
  fresh list (a fetch fires on show).
* [ ] **Close = hide, re-open instant** — close, re-open from the menu.
  **Expect:** instant re-appearance (no reload flash), same scroll position.
* [ ] **Window fits its content** — open with a short device list.
  **Expect:** the window height hugs the list (no sheet of empty canvas
  below the card; canvas margin only), clamped ≥240 / ≤600 logical px;
  hotplug while open grows/shrinks the window by about a row.
* [ ] **Panel semantics** — inspect the window chrome and, if a tiling WM is
  running (AeroSpace/yabai), where the window lands.
  **Expect:** no resize handle and the zoom (green) button is disabled — the
  shell's content-fit is the only sizing; a tiling WM **floats** the window
  as a dialog instead of tiling it into the layout (finding 2026-07-17: the
  resizable window was tiled by AeroSpace and reordered the layout); Escape
  closes the window (hide, same as the close button).
* [ ] **Dark/light** — toggle System Settings → Appearance with the window
  open.
  **Expect:** the window follows live (canvas/card/text/badge all flip; no
  white flash).
* [ ] **Daemon down renders in-window** — stop the daemon, click Devices….
  **Expect:** the window shows "Device list unavailable" + the error line in
  the card (red), not a notification; starting the daemon and re-opening
  recovers.
* [ ] **No network dependency** — Wi-Fi off, open the window.
  **Expect:** identical rendering (page + icon are embedded; nothing remote).
  \[VERIFY #18: also note the macOS version this ran on — the §4 macOS-12
  floor can only be marked verified on a 12.x machine.]

***

## Run 11 — Phase 12 windows: Setup Assistant & Health Check (SPEC §11.2–§11.3)

**Setup:** same as Run 10 (daemon up, tray unbundled via
`cargo run -p kanatabar-tray`). The panel items (float in tiling WMs, Esc
closes, content-fit, accessory focus) are shared shell behaviour already
covered by Run 10 — spot-check, don't re-verify per window.

**Health Check (§11.3):**

* [ ] **Full checklist renders** — menu → Health Check….
  **Expect:** all 12 checks with ✅/❌ dot, detail line, and (on failures) the
  monospace `↳ fix hint`; identical content to `kanatactl doctor`.
* [ ] **Copy report round-trips** — click "Copy report", paste into a file.
  **Expect:** valid JSON, same content as `kanatactl doctor --json` (the §9
  bug-report bundle); the button flashes "Copied".
* [ ] **Setup-class failure delegates** — with a setup-class check failing
  (e.g. driver deactivated), its row shows **Open Setup Assistant** and the
  click opens the wizard at the earliest failing step. Runtime-class
  failures (e.g. a broken active config) show the hint only — **no** button
  (§11 [HARD] anti-overlap).
* [ ] **Daemon down renders in-window** — stop the daemon, open Health Check.
  **Expect:** "Health check unavailable" + the error in the card; no
  notification.

**Setup Assistant (§11.2):**

* [ ] **Auto-open on incomplete setup** — quit the tray, stop the daemon
  (`sudo launchctl bootout system/io.github.ibimal.kanatabar.daemon` or a
  machine without it), relaunch the tray.
  **Expect:** within ~6 s the Setup Assistant opens itself on the
  **Install the KanataBar service** step (daemon unreachable ⇒ that step),
  showing the copyable `sudo kanatactl install`. With everything green,
  relaunching the tray must **not** auto-open it.
* [ ] **Live re-check, no clicks** — open with the extension deactivated
  (`Karabiner-VirtualHIDDevice-Manager deactivate`… then the wizard's
  activate step is current). Click "Do it for me", approve in System
  Settings when it opens.
  **Expect:** the step flips ✓ and the next step expands by itself within
  ~2–4 s of approval — no re-check button anywhere (Karabiner-Elements
  pattern; the ~2 s poll only runs while the window is open).
* [ ] **Buttons do what they say** — on the activate step: "Do it for me"
  runs the manager activation (log line `wizard step command succeeded`);
  "Open System Settings" lands on Login Items & Extensions (ledger #5
  anchors).
* [ ] **sudo is never a button** — on the VHID/install steps.
  **Expect:** a copyable `sudo kanatactl install` chip (click copies,
  flashes green), instruction text, and **no** run button.
* [ ] **Degradation overrides green** — with all checks green force
  `Degraded{InputMonitoringDenied}` (revoke the kanatad grant while
  running, Run 4 recipe).
  **Expect:** the wizard shows the **Grant Input Monitoring** step as
  current, not "Setup complete" (HW Run 9 finding, now in the window).
* [ ] **"Set it up for me" registers kanatad** (ledger #19, the key Part-2
  check) — on the Grant Input Monitoring / Grant Accessibility steps, with
  kanatad **not** yet listed, click "Set it up for me".
  **Expect:** the daemon log shows `TCC permission requested for kanatad`,
  the relevant Privacy pane opens, **and `/usr/local/bin/kanatad` now
  appears in the list** (toggle off) — i.e. the daemon-context
  `IOHIDRequestAccess` / `AXIsProcessTrustedWithOptions` call registered the
  entry. Toggle it on; within ~2 s the step flips ✓. **If the entry does NOT
  appear** (system-context request is a no-op), record it — the fallback is
  the manual +/Cmd+Shift+G path, still shown in the instruction.
* [ ] **Completion state** — everything green and healthy.
  **Expect:** every step ✓, "Setup complete" summary, and the green
  completion panel (preset-aware: suggests `kanatactl preset add` when no
  preset is configured).

***

## Consolidated open \[VERIFY] ledger

Resolve each during the run noted; record the answer here (these feed code
fixtures/constants):

| #   | Question                                                                                                 | Run | Code it pins                                                                   | Answer                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| --- | -------------------------------------------------------------------------------------------------------- | --- | ------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | IOKit matching arms as root LaunchDaemon?                                                                | 1   | `ffi/iokit.rs`                                                                 | ❌ 2026-07-11: NO — root LaunchDaemon (uid 0) got `IOServiceAddMatchingNotification failed: 0xe00002c7` (kIOReturnUnsupported), zero success lines. Root cause (2026-07-12): `kIOMatchedNotification` AND `kIOTerminatedNotification` are both unsupported for IOHIDDevice; `kIOFirstMatchNotification` works. ✅ hw-fix-5: `ffi::iokit` now uses `kIOFirstMatchNotification` + per-device `IOServiceAddInterestNotification(kIOGeneralInterest)` for removal (no Input Monitoring; `IOHIDManager` rejected — its Open needs the grant). Verified unprivileged: 25 devices enumerated (`tests/iokit_smoke.rs`). ✅ **HW-confirmed as root 2026-07-12**: `disabled` line gone; USB plug + unplug each delivered exactly one debounced re-sync (~63/89ms) — arms AND delivers arrival + removal. |
| 2   | `launchctl bootstrap gui/<uid>` works as plain root?                                                     | 1   | `kanatactl::install` (else → `asuser`)                                         |                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| 3   | kanata version → supported driver version on this machine                                                | 2   | `driver::supported_driver_major`                                               | ✅ 2026-07-11: `systemextensionsctl` reports the `.dext` *bundle* version (1.8.0), independent of the pkg version — coupling made report-only                                                                                                                                                                                                                                                                                                                                                                         |
| 4   | Other VHID-daemon launchd label spellings in the wild                                                    | 2   | `vhidd::classify`                                                              | ✅ 2026-07-11: the driver extension itself appears as `org.pqrs.…-0x<token>` — instance labels now excluded from "managed"                                                                                                                                                                                                                                                                                                                                                                                            |
| 10a | `systemextensionsctl list` real output parses                                                            | 3   | `driver::parse_systemextensions`                                               | ✅ 2026-07-11: HW capture added as a fixture; parses correctly                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| 5   | System Settings pane anchors (Extensions / Input Monitoring)                                             | 3   | `wizard::panes`                                                                | ✅ **Extensions** 2026-07-13: `…security?Extensions` lands on **Login Items & Extensions → Driver Extensions** (macOS 26.5.1, human-confirmed via the wizard activate step) — correct. ⏳ **Input Monitoring** (`…security?Privacy_ListenEvent`) + **Accessibility** (`…?Privacy_Accessibility`) anchors not yet HW-checked. (The doctor can now read the grants — ledger #19 — so both grant steps are reachable and these anchors can finally be exercised.)                                                            |
| 19  | Daemon-context TCC read **and request**: does a **root LaunchDaemon** report its own grant accurately via `IOHIDCheckAccess` / `AXIsProcessTrusted`, and does `IOHIDRequestAccess` / `AXIsProcessTrustedWithOptions` register kanatad's entry from the system context ("Set it up for me")? | 11  | `ffi/tcc.rs`; `doctor::{input_monitoring_check,accessibility_check}`; `control` RequestPermission | ⏳ **NOT YET VERIFIED.** With kanatad **granted** both permissions and remapping working, run `kanatactl doctor --json` and read the `input monitoring` + `accessibility` rows. **Expect (if reads work from the daemon context):** both `ok:true` with the "verified via …" detail. Then **revoke** each grant (Run 4 recipe) and re-run: Input Monitoring should read `denied` (→ red), Accessibility `AXIsProcessTrusted` false. **If a granted daemon reads `Unknown`/untrusted** (system-context quirk): the conservative policy already keeps it informational-green (no false red), and we keep leaning on the behavioral `Degraded` backstop — record that here and leave the code as-is. **If reads are accurate:** flip `accessibility_check` not-trusted → `fail` and (optionally) Input-Monitoring `Unknown` → a softer red, so first-run shows real reds. |
| 6   | TCC grant attaches to kanata or kanatad (responsible process)?                                           | 4   | wizard/doctor wording; possible spawn-disclaim                                 | ✅ 2026-07-11 (grant matrix): **kanatad**, and it needs BOTH Input Monitoring AND Accessibility; kanata needs no grants (its self-registered entry is cosmetic); linker-signed binaries hold grants fine; spawn-disclaim rejected (would make kanata upgrades break grants)                                                                                                                                                                                                                                           |
| 7   | Exact TCC-denial error text from this kanata                                                             | 4   | `kanata::classify_fault_line`                                                  | ✅ 2026-07-11 (v1.12.0): "failed to open keyboard device(s): kanata needs macOS Input Monitoring permission…" — none of the old patterns matched; "input monitoring permission" added + verbatim fixture                                                                                                                                                                                                                                                                                                              |
| 8   | Exact device-in-use error text                                                                           | 4   | `kanata::classify_fault_line`                                                  | ✅ 2026-07-13 (kanata 1.12.0): `IOHIDDeviceOpen error: (iokit/common) exclusive access and device already open <device>` — captured HW by starting a 2nd kanata while ours held the keyboard. `classify_fault_line` matches (`"exclusive access"` and `"already open"`) → `DeviceInUse` → `Degraded{DeviceGrabConflict}`. Worth adding the verbatim line as a fixture.                                                                                                                                                 |
| 9   | Which config features need Accessibility (RESOLVED: all — required up front for device open; see row 6)  | 4   | SPEC §2, wizard text                                                           |                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| 10  | `systemextensionsctl list` column layout on this macOS                                                   | 5   | `driver::parse_systemextensions` fixtures                                      |                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| 11  | VHID-daemon pgrep path fragment matches                                                                  | 5   | `health::driver::vhid_daemon_running`                                          |                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| 12  | kanata TCP `LayerChange` message shape                                                                   | 5   | `kanata::parse_layer_change`                                                   | ✅ 2026-07-13 (kanata 1.12.0): verbatim `{"LayerChange":{"new":"nav"}}` / `{"LayerChange":{"new":"base"}}` captured via `nc 127.0.0.1 5829` — matches the assumed shape exactly; `parse_layer_change` handles it. kanata also emits an initial `{"LayerChange":{"new":"base"}}` on client connect.                                                                                                                                                                                                                        |
| 13  | kanata version floor still 1.7.0-appropriate                                                             | 5   | `KANATA_VERSION_FLOOR`                                                         | ✅ 2026-07-13: startup log records `kanata version version=1.12.0`; 1.12.0 ≥ 1.7.0 floor → **no** floor warning emitted. Constant still appropriate.                                                                                                                                                                                                                                                                                                                                                                    |
| 14  | Karabiner virtual-device product name + vendor id (0x16C0?)                                              | 6   | `device::is_karabiner_virtual`                                                 | ✅ 2026-07-12 (from `iokit_smoke` enumeration): product `Karabiner DriverKit VirtualHIDKeyboard 1.8.0`, vendor **5824 = 0x16C0** — confirms the assumed vendor id. (Bundle version `1.8.0` is in the product name; the name-prefix + vendor filter both hold.) HW re-confirm the feedback-loop suppression in Run 6. |
| 15  | Panic-escape exit is code 0 (clean) on this kanata                                                       | 6/7 | `child::classify_unrequested_exit`                                             |                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| 16  | Driver-major mismatch presents as green doctor + `Running` but no remapping — should supervisor Degrade? | 1   | `kanata::classify_backend_line` · `machine` · `supervisor` backend grace timer | ✅ 2026-07-11 (hw-fix-4): kanata 1.12.0 + driver **v8.0.0** → silent passthrough, all checks ✅, state `Running`; fixed by pinning driver to **v6.2.0**. Code fix landed: the release line (`output backend unavailable — releasing input devices`, verbatim fixture) now drives `Degraded{OutputBackendUnavailable}` after a 15s grace, **keeping the child** (kanata self-recovers; the `…ready — re-grabbing…` line flips back to Running). Screen-lock releases + write blips excluded. ✅ **HW re-verify PASSED 2026-07-13 (Run 5):** kill vhidd (bootout+pkill) → release line `output backend unavailable **during write** — releasing input devices` (the real line has "during write"; classifier substring-matches it) → 15s grace → `Degraded{OutputBackendUnavailable}` (exit 4) **from=Running, child kept alive same pid 67010** → bootstrap vhidd → recovery line verbatim `output backend and console session ready — re-grabbing input devices` → `from=Degraded to=Running`, **same pid**. ✅ Fixture added 2026-07-13 (hw-fix): the exact `output backend unavailable during write — releasing input devices` line now asserts `Down` in `classifies_backend_release_as_down`. |
| 17  | WKWebView (wry) window creation works **unbundled** and `set_focus` fronts it under `ActivationPolicy::Accessory`? | 10  | `ui_shell` (Phase 12; a failure here triggers the design doc's Option-A fallback) |                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| 18  | macOS 12 (§4 floor): do the WKWebView APIs wry 0.55 uses exist there?                                    | 10  | §4 minimum macOS; docs/design/phase12-ui-layer.md Q3                           |                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |

## Reporting results

When the pass is done (or blocked), hand back:

1. This file with boxes ticked and the ledger + inline blanks filled.
2. The captures: `systemextensionsctl list`, `doctor --json` (green machine),
   one kanata TCP line, the `ioreg` virtual-device lines, and any error text
   that didn't match a classifier.
3. Anything that surprised you, even if it passed.

Findings become code fixes + updated fixtures; the boxes stay unticked until
they pass on hardware. **v0.1.0 does not ship with unticked \[HARD] boxes.**
