# KanataBar icons

## App icon (full-color) — used from Phase 10 (bundling)
- `icon.svg` — vector master.
- `KanataBar.iconset/` — the per-size PNGs (the bundling source).
- Build the `.icns` during packaging (not committed — it's derived):
  ```
  iconutil -c icns KanataBar.iconset -o KanataBar.icns
  ```
  This icon becomes the notification / Notification Center / System Settings /
  About icon once `KanataBar.app` is bundled.

## Menu-bar status icons (monochrome template) — used now
Live in `../menubar/`. Designed as template SVGs (black shape on transparent);
`src/icons.rs` embeds the pre-rasterized `*.png` (18pt @2x) and macOS tints
them white/black. To re-rasterize after editing an SVG, use a real SVG
renderer (resvg) — `qlmanage` renders these blank. `running-preset.*` is a
spare active-preset badge variant, not yet wired.
