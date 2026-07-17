# Phase 12 design doc — UI layer for the wizard & doctor windows

Status: **draft for review** (SPEC §19 Phase 12 requires this doc before implementation).
Scope: pick the windowing/widget toolkit for SPEC §11.2 (Setup Assistant window), §11.3
(Health Check window), and the §8 Devices window, and sketch the integration so the
[AUTO]-testable part stays toolkit-free.

## 1. Requirements & constraints

What the three windows actually need (SPEC §11.2–§11.3, §8) — deliberately modest:

- A vertical checklist (✅/❌ + name + detail + hint), one step/check expanded with text.
- Buttons: "Do it for me", "Open System Settings", "Open Setup Assistant at this step",
  "Copy report". A clipboard write. No text input, no tables, no custom drawing.
- **Live re-check** (wizard): rows flip green while the window is open (~2 s poll).
- Auto-open on tray start while setup is incomplete (§11.1).
- **Devices** (§8): a read-only list (name + matched badge, summary line) that updates in
  place on `Event::DeviceChanged` — event-driven, not polled; the subscription already
  delivers these to the tray. Replaces the v0.1.x one-line notification, which truncates a
  12-device list to a banner. (A temp-file-in-editor report was considered and rejected: it
  is the pattern §11.3 retires for doctor, and a static file cannot reflect hotplug — the one
  thing the devices view is *for*.)

Hard constraints the toolkit must fit:

- [HARD] The **tao event loop owns the main thread** (SPEC §3.1; tray `main.rs`): the status
  item, menu events, and `UserEvent` marshaling (via `EventLoopProxy`) all live on it. Any UI
  layer must share that loop — a second event-loop owner in-process is a non-starter.
- [HARD] Unsafe stays confined per CLAUDE.md (today: `kanatad/src/ffi` only; the tray's
  `ffi.rs` is `#![deny(unsafe_code)]` and must remain so, or the rule needs an explicit
  amendment — treated as a cost below).
- [HARD] Never render or log `.kbd` contents or keystrokes (CLAUDE.md); check `detail` strings
  come from the daemon's doctor report only.
- §4: MSRV pinned, universal binary via `lipo`, `cargo-deny` green (licenses/advisories/bans),
  minimum macOS 12 [VERIFY].
- §15/§19 testability split (Phase 7 precedent): the window's *model* lives in the
  `kanatabar_tray` lib, display-free and unit-tested; the binary is the thin shell exercised
  only by the [HW] checklist.
- Must work **unbundled** during dev (bare cargo binary, no `.app`), like the rest of the tray
  (SPEC §18; the notification fallback exists for the same reason).

## 2. Options evaluated

### A. Native AppKit programmatically (`objc2-app-kit`: NSWindow/NSStackView/NSButton)

- ✅ Native look; zero new rendering deps (objc2 stack already in the tree); NSWindows attach
  to tao's NSApplication runloop directly.
- ❌ Button target-action requires an ObjC subclass (`define_class!`) → `unsafe impl` blocks in
  the tray, breaking the CLAUDE.md confinement rule; the amendment ("unsafe also allowed in
  `kanatabar-tray/src/ffi` with SAFETY comments") is possible but pays a review-surface cost
  for every widget wired.
- ❌ Highest per-widget effort (layout, autolayout or manual frames, dark-mode assets); the
  live-flip UX means hand-rolled view diffing.

### B. egui in-process (eframe)

- ❌ Rejected outright: eframe owns a **winit** event loop; tao and winit cannot both pump the
  main-thread `NSApplication` loop. There is no maintained egui↔tao glue; hand-rolling input
  translation + a glow/wgpu painter is a large, permanent maintenance liability.

### C. Migrate the tray from tao to winit, then egui (egui-winit + egui-glow)

- ✅ Technically coherent (tray-icon supports winit loops too).
- ❌ Rewrites a shipped, HW-verified tray shell (Phases 7–10) to unblock three simple windows;
  adds a GPU/render dependency tree (~MBs, large `cargo-deny` delta); non-native look.
  Disproportionate.

### D. Separate helper binary (`kanatabar-ui`, eframe) spawned by the tray

- ✅ No event-loop conflict (own process); crash isolation; tray untouched.
- ❌ A third shipped binary: universal build, pkg payload, brew formula, uninstall audit
  (SPEC §10 "leave nothing behind") all grow; process lifecycle management (single instance,
  focus hand-off) is new failure surface. Overkill for three simple windows.

### E. **wry (WKWebView) inside tao windows** ← recommended

- ✅ tao + wry is the designed pairing (the Tauri stack); the webview renders inside a
  `tao::window::Window` on the existing loop — no second loop, no migration.
- ✅ **Zero `unsafe` in our code**; the CLAUDE.md confinement rule holds as-is.
- ✅ Uses the **system WKWebView** — no bundled engine; the dependency delta is the `wry`
  crate over the objc2 stack already in the tree.
- ✅ A checklist with live-flipping rows, buttons, and copy-to-clipboard is trivial in
  HTML/CSS; dark mode via `prefers-color-scheme` for free.
- ❌ UI written as embedded HTML/JS (a second language in the repo) — mitigated by keeping it
  to three static, asset-embedded pages with no framework, no build step.
- ❌ Version coupling: the `wry` release must support our pinned `tao` 0.35 via
  raw-window-handle. **[VERIFY at implementation: exact wry version against tao 0.35 & MSRV.]**
- ❌ WKWebView in an unbundled process: believed fine (unlike UNUserNotificationCenter, no
  bundle requirement is documented). **[VERIFY unbundled + `ActivationPolicy::Accessory`.]**

## 3. Decision

**Option E: wry webviews in tao windows.** It is the only option that simultaneously keeps the
tao loop, the unsafe-confinement rule, and the shipped tray intact, at the smallest dependency
and packaging delta. Option A is the documented fallback if the wry/tao pairing fails
verification — accepting the CLAUDE.md amendment as its price.

## 4. Design sketch

### Module layout (testability split, Phase 7 precedent)

```
kanatabar-tray/src/
  wizard.rs        (exists) step model, first_unsatisfied, degraded mapping
  healthwin.rs     NEW lib: doctor-window view-model — rows from DoctorCheck +
                   fix-tier classification (§11.3 tiers 1–3), serializable to JSON
  wizardwin.rs     NEW lib: wizard-window view-model — steps × doctor results →
                   per-row state (done/current/pending), auto-open predicate
                   (setup_complete(), §11.1 — classification lives in core::doctor)
  devwin.rs        NEW lib: devices-window view-model — Vec<DeviceInfo> → rows +
                   summary line ("12 devices, 1 matched")
  ui/              NEW bin-side shell: tao window + wry webview glue
  ui/health.html   static, embedded via include_str!, no external resources
  ui/wizard.html   static, embedded, ditto
  ui/devices.html  static, embedded, ditto (no buttons — read-only)
```

Core gains the §11.1 check classification next to `ALL_CHECKS` (`SETUP_CHECKS: [&str; 6]`) and
`setup_complete(&[DoctorCheck]) -> bool` — single source, [AUTO]-tested.

### Data flow (reuses the existing marshaling, no new patterns)

- **Rust → JS**: the tokio side fetches doctor (`conn::fetch_doctor`, ~2 s poll while a window
  is open), builds the view-model in the lib, marshals it to the main thread via the existing
  `EventLoopProxy`, and pushes it with `webview.evaluate_script("render(<json>)")` —
  serde-serialized, so escaping is not hand-rolled.
- **Devices window refresh**: `GetDevices` on open, then re-fetch whenever the existing
  subscription delivers `Event::DeviceChanged` while the window is visible — no poll; the
  wizard's ~2 s poll exists only because TCC approval produces no event.
- **JS → Rust**: buttons post opaque action ids over wry's IPC handler (`window.ipc`), which
  forwards them into the loop as `UserEvent` — the same shape as menu-click ids today
  (`ids::*`). No payload beyond the id + check name.
- **Clipboard** ("Copy report"): JS-side `navigator.clipboard.writeText` on the button's user
  gesture — avoids any new Rust clipboard dependency. [VERIFY inside WKWebView.]

### Webview hardening (SPEC §14 posture)

- Pages are **static embedded assets**; navigation handler denies everything except the
  initial load; no remote content, ever; devtools off in release builds.
- All dynamic text (check names/details/hints) enters via the serde-JSON `render()` call and
  is inserted with `textContent` (never `innerHTML`) — no interpolation into markup.
- The daemon remains the trust boundary: the window only displays doctor output and emits the
  same request ids the menu already can; no new daemon surface.

### Visual design (one system, every page)

All pages embed the same token sheet (`assets/ui/shared.css`) unchanged, so the
windows read as one product:

- **Native-feel**: system font stack (`-apple-system`), 13px base, macOS
  light/dark palettes switched by `prefers-color-scheme` (`color-scheme: light
  dark` so WKWebView's own backgrounds match — no white flash in dark mode).
- **Layout**: card-on-canvas — a 16px padded canvas (`--bg`), content in
  rounded 10px cards (`--card`) with hairline separators; header = 40px app
  icon + title + secondary summary line. The icon is the committed 128px
  iconset entry served as `kbasset://app/icon.png`, so windows, Dock, and
  notifications share one identity.
- **Status colors**: `--ok` green dot + `MATCHED` pill for active rows,
  `--err` for failures, secondary-label gray for inert rows. Text selection
  and cursors are disabled (these are windows, not documents).
- **Fast**: no framework, no build step, no network, assets embedded in the
  binary; windows are created once and hidden on close so re-open is instant;
  pushes are single `__render(json)` calls into an already-loaded page.

### Window behavior

- Windows are created lazily on first open and hidden (not destroyed) on close, so re-open is
  instant and single-instance by construction; the ~2 s poll runs only while a window is
  visible.
- **Panels, not documents**: every Phase 12 window is **fixed-size** (non-resizable) — these
  are glanceable utility surfaces, and the wizard is a fixed-size assistant by convention
  (Karabiner/LuLu/installers). Non-resizable also makes the AX role read as a dialog, so
  tiling window managers (AeroSpace/yabai) **float** them instead of tiling them into the
  layout (HW finding 2026-07-17: a resizable devices window was tiled and reordered the
  user's layout). Escape closes (page sends `close` over ipc; the shell hides). A true
  `NSPanel` would need unsafe AppKit access — non-resizable gets the same practical behavior
  within the toolkit.
- **Content-fit height**: after each render the page reports its natural content height over
  ipc (`height:<px>`) and the shell fits the window to it, clamped to [240, 600] logical px —
  short lists don't leave empty canvas, hotplug grows/shrinks by a row. With the windows
  non-resizable, the shell's fit is the only sizing there is (no user-drag arbitration
  needed). Cards hug their content; the canvas shows beneath.
- Auto-open (wizard): after the tray's first successful doctor fetch, if `!setup_complete`.
- Focus with `ActivationPolicy::Accessory`: showing a window from an accessory app needs an
  explicit activate; tao exposes this. [VERIFY in the HW checklist — first item.]
- sudo steps render as copyable command text only (§11.2 [HARD]): the "Do it for me" button
  exists solely for steps with a non-sudo `run` argv (today: driver activation).

## 5. Gate impact (SPEC §19 row 12)

- **[AUTO]**: core classification + `setup_complete` tests; `healthwin`/`wizardwin`/`devwin`
  view-model tests (render given fixed `DoctorCheck`/`DeviceInfo` fixtures, fix-tier
  assignment, auto-open predicate, step↔check inversion, device summary line); doctor JSON
  schema test unchanged. No test touches wry/tao.
- **[HW]** (append to docs/HW-TESTS.md at implementation): accessory-app window focus; clean
  machine onboarding driven by the wizard window alone; live flip on System-Settings approval;
  doctor window delegation into the wizard; Copy-report paste-back; dark mode; unbundled dev
  run; hotplug (unplug/replug a matched keyboard) flips the open devices window in place.

Implementation order (SPEC §19 row 12): **devices first** — it is the smallest window
(read-only, no IPC actions, event-driven refresh) and proves the whole shell (tao window +
wry + embed + focus + render pipe) before the button/action plumbing lands with doctor, then
the wizard adds the poll + auto-open.

## 6. Open questions (resolve before/at implementation, [VERIFY])

1. ~~Exact `wry` version compatible with `tao` 0.35 + MSRV; `cargo-deny` result for its
   tree.~~ **Resolved (2026-07-17, devices window):** `wry` 0.55.1 builds against `tao`
   0.35.3 with the `rwh_06` feature enabled (our `default-features = false` had dropped
   tao's raw-window-handle impl) on MSRV 1.96. `cargo-deny` needed one addition: a
   crate-scoped MPL-2.0 exception for `option-ext` (wry → dirs → dirs-sys), see deny.toml.
2. WKWebView behavior unbundled and under `ActivationPolicy::Accessory` (focus, clipboard).
   → docs/HW-TESTS.md Phase 12.
3. Whether macOS 12 (§4 minimum) constrains any WKWebView API wry uses.
   → verify alongside (2) on hardware.

If (2)–(3) fail on hardware, fall back to Option A and amend CLAUDE.md's unsafe-confinement
rule to include `kanatabar-tray/src/ffi` with per-call SAFETY comments.
