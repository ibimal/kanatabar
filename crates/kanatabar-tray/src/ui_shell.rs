//! Phase 12 window shell: tao windows hosting wry WKWebViews
//! (docs/design/phase12-ui-layer.md). This is bin-side glue — the
//! [AUTO]-tested logic lives in the lib view-models (`devwin`, …); nothing
//! here computes what to display.
//!
//! Shell contract (shared by every Phase 12 window):
//! - Pages and the app icon are **embedded assets** served over the `kbasset`
//!   custom protocol; the navigation handler denies everything else, so the
//!   webview can never leave the binary's own content.
//! - Windows are created lazily and **hidden on close**, not destroyed —
//!   re-open is instant and single-instance by construction.
//! - Rust pushes state with `evaluate_script("__render(<json>)")`; the page
//!   signals `ready` over the ipc channel and the shell replays the latest
//!   view, so a push can never race the page load.
//! - Windows are **panels, not documents**: fixed-size (the shell alone sizes
//!   them, fitting the page's reported content height). Non-resizable also
//!   makes the AX role read as a dialog, so tiling window managers
//!   (AeroSpace/yabai) float these windows instead of tiling them into the
//!   layout (HW finding 2026-07-17). Pages send `close` on Escape — the
//!   panel convention.

use anyhow::{Context, Result};
use std::borrow::Cow;
use tao::dpi::LogicalSize;
use tao::event_loop::EventLoopWindowTarget;
use tao::window::{Window, WindowBuilder, WindowId};
use wry::http::header::CONTENT_TYPE;
use wry::http::{Request, Response, StatusCode};
use wry::{WebView, WebViewBuilder};

/// The app icon shown in every window header (served as
/// `kbasset://app/icon.png`) — the committed 128px iconset entry, so the
/// windows and the Dock/notification identity can never drift apart.
const ICON_PNG: &[u8] = include_bytes!("../assets/appicon/KanataBar.iconset/icon_128x128.png");

/// Design tokens shared by every page (docs/design/phase12-ui-layer.md).
const SHARED_CSS: &str = include_str!("../assets/ui/shared.css");

/// The devices page (SPEC §8), with the shared sheet spliced in at build time.
const DEVICES_HTML: &str = include_str!("../assets/ui/devices.html");

/// The Health Check page (SPEC §11.3).
const HEALTH_HTML: &str = include_str!("../assets/ui/health.html");

/// The Setup Assistant page (SPEC §11.2).
const WIZARD_HTML: &str = include_str!("../assets/ui/wizard.html");

/// The placeholder each page's `<style>` block carries for [`SHARED_CSS`].
const CSS_SLOT: &str = "/*__SHARED_CSS__*/";

/// Content-fit bounds (logical px). Pages report their natural content
/// height over ipc (`height:<px>`) and the shell fits the window to it,
/// clamped here — so short lists don't leave a sheet of empty canvas and
/// long ones don't run off the screen. The windows are non-resizable panels,
/// so the shell's fit is the only sizing there is.
const MIN_FIT_HEIGHT: f64 = 240.0;
const MAX_FIT_HEIGHT: f64 = 600.0;

/// One shell window (a tao window + its webview) plus the render replay state.
pub struct ShellWindow {
    window: Window,
    webview: WebView,
    /// The page ran its inline script and can accept `__render` calls.
    ready: bool,
    /// Latest serialized view, replayed on `ready` (and kept for re-shows).
    last_view: Option<String>,
}

impl ShellWindow {
    /// Create the devices window, hidden. `on_ipc` receives the page's ipc
    /// messages (marshal them into the event loop; never do UI work inline).
    pub fn devices<T>(
        target: &EventLoopWindowTarget<T>,
        on_ipc: impl Fn(String) + 'static,
    ) -> Result<Self> {
        Self::build(
            target,
            "KanataBar Devices",
            // Near the fit minimum: the first `height:` report right after
            // the first render grows it to content, which beats shrinking.
            LogicalSize::new(400.0, 280.0),
            DEVICES_HTML,
            on_ipc,
        )
    }

    /// Create the Health Check window, hidden (SPEC §11.3). Wider than the
    /// devices panel: check details and fix hints are sentences.
    pub fn health<T>(
        target: &EventLoopWindowTarget<T>,
        on_ipc: impl Fn(String) + 'static,
    ) -> Result<Self> {
        Self::build(
            target,
            "KanataBar Health Check",
            LogicalSize::new(480.0, 320.0),
            HEALTH_HTML,
            on_ipc,
        )
    }

    /// Create the Setup Assistant window, hidden (SPEC §11.2).
    pub fn wizard<T>(
        target: &EventLoopWindowTarget<T>,
        on_ipc: impl Fn(String) + 'static,
    ) -> Result<Self> {
        Self::build(
            target,
            "KanataBar Setup Assistant",
            LogicalSize::new(480.0, 320.0),
            WIZARD_HTML,
            on_ipc,
        )
    }

    fn build<T>(
        target: &EventLoopWindowTarget<T>,
        title: &str,
        size: LogicalSize<f64>,
        page_html: &str,
        on_ipc: impl Fn(String) + 'static,
    ) -> Result<Self> {
        let window = WindowBuilder::new()
            .with_title(title)
            .with_inner_size(size)
            // Panel semantics: the shell sizes the window (content fit); the
            // user doesn't. Non-resizable also reads as a dialog to tiling
            // WMs (AeroSpace/yabai), which float it instead of tiling it.
            .with_resizable(false)
            .with_visible(false)
            .build(target)
            .context("creating window")?;

        let html: Cow<'static, [u8]> = page_html.replace(CSS_SLOT, SHARED_CSS).into_bytes().into();
        let webview = WebViewBuilder::new()
            .with_custom_protocol("kbasset".to_string(), move |_id, request| {
                serve_asset(&request, html.clone())
            })
            // The page may only ever be (re)loaded from our own embed.
            .with_navigation_handler(|url| url.starts_with("kbasset://"))
            .with_ipc_handler(move |request| on_ipc(request.into_body()))
            .with_url("kbasset://app/index.html")
            .build(&window)
            .context("creating webview")?;

        Ok(Self {
            window,
            webview,
            ready: false,
            last_view: None,
        })
    }

    /// This window's id, for matching `WindowEvent`s in the event loop.
    pub fn id(&self) -> WindowId {
        self.window.id()
    }

    /// Show and focus the window (re-opens count as user intent: focus).
    pub fn show(&mut self) {
        self.window.set_visible(true);
        self.window.set_focus();
    }

    /// Fit the window height to the page's reported content height (logical
    /// px, clamped). Width is left as-is. Programmatic sizing works fine on a
    /// non-resizable window — `resizable(false)` only removes the *user's*
    /// resize affordance.
    pub fn fit_content_height(&mut self, content: f64) {
        let scale = self.window.scale_factor();
        let width = self.window.inner_size().to_logical::<f64>(scale).width;
        let size = LogicalSize::new(width, content.clamp(MIN_FIT_HEIGHT, MAX_FIT_HEIGHT));
        self.window.set_inner_size(size);
    }

    /// Hide on close — state (webview, scroll, last view) survives re-open.
    pub fn hide(&self) {
        self.window.set_visible(false);
    }

    /// Whether the window is currently on screen (event-driven refreshes are
    /// gated on this: no fetches for a hidden window).
    pub fn is_visible(&self) -> bool {
        self.window.is_visible()
    }

    /// The page finished loading; replay the latest view if one arrived early.
    pub fn page_ready(&mut self) {
        self.ready = true;
        if let Some(json) = self.last_view.clone() {
            self.push(&json);
        }
    }

    /// Render a serde-serialized view-model. Safe to call before the page is
    /// ready — the JSON is kept and replayed on `ready`.
    pub fn render(&mut self, view_json: String) {
        if self.ready {
            self.push(&view_json);
        }
        self.last_view = Some(view_json);
    }

    fn push(&self, view_json: &str) {
        if let Err(err) = self
            .webview
            .evaluate_script(&format!("window.__render({view_json})"))
        {
            tracing::warn!(%err, "webview render failed");
        }
    }
}

/// Serve an embedded asset for a `kbasset://app/<path>` request.
fn serve_asset(
    request: &Request<Vec<u8>>,
    page_html: Cow<'static, [u8]>,
) -> Response<Cow<'static, [u8]>> {
    let path = request.uri().path();
    let (body, mime): (Cow<'static, [u8]>, &str) = match path {
        "/index.html" => (page_html, "text/html; charset=utf-8"),
        "/icon.png" => (ICON_PNG.into(), "image/png"),
        _ => {
            return Response::builder()
                .status(StatusCode::NOT_FOUND)
                .header(CONTENT_TYPE, "text/plain")
                .body(Cow::Borrowed(b"not found".as_slice()))
                .unwrap_or_else(|_| Response::new(Cow::Borrowed(b"".as_slice())));
        }
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, mime)
        .body(body)
        .unwrap_or_else(|_| Response::new(Cow::Borrowed(b"".as_slice())))
}
