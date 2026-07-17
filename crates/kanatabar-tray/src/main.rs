//! kanatabar-tray — KanataBar menu-bar app (SPEC §8).
//!
//! The thin GUI shell around the toolkit-free client logic in the library
//! (`kanatabar_tray::*`). Structure per SPEC §3.1: the macOS event loop (tao)
//! owns the **main thread** and the status item; socket I/O runs on a
//! background tokio runtime and marshals UI updates back via an
//! `EventLoopProxy`. This file is exercised by the `[HW]` visual/menu checklist
//! (docs/HW-TESTS.md), not by CI — the `[AUTO]` gate tests the library.
//!
//! `muda` (SPEC §4) is used via `tray_icon::menu`, tray-icon's own re-export,
//! so the tray and its menu are guaranteed to share one `muda` instance (the
//! menu-event global channel must match the tray that owns the menu).

mod ui_shell;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use kanatabar_core::backoff::BackoffConfig;
use kanatabar_core::ipc::RequestPayload;
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
use tokio::sync::mpsc::UnboundedReceiver;
use tray_icon::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use kanatabar_tray::conn::{self, Update};
use kanatabar_tray::menu::{self, ids};
use kanatabar_tray::model::MenuModel;
use kanatabar_tray::notify::{BundledNotifier, Notifier, OsascriptNotifier};
use kanatabar_tray::single_instance::{self, SingleInstanceLock};
use kanatabar_tray::{devwin, icons, login, wizard};

use ui_shell::ShellWindow;

/// Events marshaled into the main-thread event loop.
enum UserEvent {
    /// The daemon-facing model changed; rebuild the menu and icon. Boxed to
    /// keep the enum small (clippy `large_enum_variant`).
    Model(Box<MenuModel>),
    /// A menu item with this id was clicked.
    MenuClick(String),
    /// A fresh device list arrived for the devices window (SPEC §8).
    Devices(Box<devwin::DevicesView>),
    /// The devices page loaded and can accept renders.
    DevicesPageReady,
    /// Hotplug: re-fetch the device list if the window is showing.
    DevicesChanged,
    /// The devices page reported its content height (logical px) for the
    /// shell's window fit.
    DevicesContentHeight(f64),
    /// The devices page asked to close (Escape — panel convention).
    DevicesCloseRequested,
}

/// Resolve the control-socket path (SPEC §3.2), overridable for dev/test.
fn socket_path() -> PathBuf {
    std::env::var_os("KANATABAR_SOCK")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(kanatactl::DEFAULT_SOCKET))
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    // Single instance per user (SPEC §8): a second tray must bail, not fight
    // over the same socket.
    let _lock = match SingleInstanceLock::acquire(&single_instance::default_path()) {
        Ok(lock) => lock,
        Err(err) => {
            eprintln!("KanataBar is already running ({err})");
            return Ok(());
        }
    };

    let socket = socket_path();
    let uid = nix::unistd::getuid().as_raw();

    // Background tokio runtime for all socket I/O (SPEC §3.1).
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .context("building tokio runtime")?;

    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    // Menu-bar-only: no Dock icon, no app-switcher entry (SPEC §8
    // `LSUIElement = true`). As an unbundled launchd binary there's no
    // Info.plist to carry that key, so set the equivalent activation policy in
    // code (must be before `run`) — this also survives the Phase 9 bundle.
    event_loop.set_activation_policy(ActivationPolicy::Accessory);

    // Channels: updates flow daemon → UI; commands flow UI → daemon.
    let (update_tx, update_rx) = tokio::sync::mpsc::unbounded_channel::<Update>();
    let (command_tx, command_rx) = tokio::sync::mpsc::unbounded_channel::<RequestPayload>();

    // Forward menu clicks into the loop as user events.
    let menu_proxy = event_loop.create_proxy();
    tray_icon::menu::MenuEvent::set_event_handler(Some(
        move |event: tray_icon::menu::MenuEvent| {
            let _ = menu_proxy.send_event(UserEvent::MenuClick(event.id.0));
        },
    ));

    // Spawn the socket tasks on the runtime.
    let update_proxy = event_loop.create_proxy();
    // Bundled (pkg install): real Notification Center, KanataBar's identity.
    // Unbundled (dev/tarball): osascript fallback (SPEC §18).
    let notifier: Arc<dyn Notifier> = match BundledNotifier::new() {
        Some(bundled) => Arc::new(bundled),
        None => Arc::new(OsascriptNotifier),
    };
    // A clone for the main-thread doctor/wizard actions (below), plus a handle
    // to spawn those one-shot tasks onto the same runtime.
    let action_notifier = Arc::clone(&notifier);
    let action_handle = runtime.handle().clone();
    {
        let socket = socket.clone();
        runtime.spawn(conn::run_event_stream(
            socket.clone(),
            update_tx,
            BackoffConfig::default(),
        ));
        runtime.spawn(forward_updates(update_rx, update_proxy, notifier));
        runtime.spawn(run_commands(socket, command_rx));
    }

    // Main-thread UI state. The tray item must be created after the event loop
    // starts on macOS, so it is built lazily on the Init event.
    let mut tray: Option<TrayIcon> = None;
    let mut model = MenuModel::disconnected();
    let mut login_loaded = login::is_loaded(uid);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    // Phase 12 windows: created lazily on first open, hidden on close.
    let mut devices_window: Option<ShellWindow> = None;
    let ipc_proxy = event_loop.create_proxy();
    let devices_proxy = event_loop.create_proxy();

    event_loop.run(move |event, target, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::NewEvents(StartCause::Init) => {
                tray = build_tray(&model, login_loaded);
            }
            Event::UserEvent(UserEvent::Model(new_model)) => {
                model = *new_model;
                if let Some(tray) = tray.as_ref() {
                    apply_model(tray, &model, login_loaded);
                }
            }
            Event::UserEvent(UserEvent::MenuClick(id)) => {
                if id == ids::QUIT {
                    // Quit the tray only; the daemon keeps kanata running
                    // (SPEC §8).
                    *control_flow = ControlFlow::Exit;
                } else if id == ids::LOGIN {
                    login_loaded = toggle_login(uid, home.as_deref(), login_loaded);
                    if let Some(tray) = tray.as_ref() {
                        apply_model(tray, &model, login_loaded);
                    }
                } else if id == ids::EDIT_CONFIG {
                    // Open in the default text editor (SPEC §8; a config
                    // *editor* GUI is a non-goal, SPEC §1).
                    if let Some(path) = model.active_config.clone() {
                        open_in_editor(&path);
                    }
                } else if id == ids::VALIDATE_CONFIG {
                    if let Some(path) = model.active_config.clone() {
                        run_validate_action(
                            &action_handle,
                            socket.clone(),
                            Arc::clone(&action_notifier),
                            path,
                        );
                    }
                } else if id == ids::DEVICES {
                    // Open (or re-show) the devices window (SPEC §8, Phase 12)
                    // and fetch a fresh list. If the webview can't be created,
                    // fall back to the v0.1.x notification so the menu item
                    // still answers.
                    if devices_window.is_none() {
                        let proxy = ipc_proxy.clone();
                        match ShellWindow::devices(target, move |message| {
                            if message == "ready" {
                                let _ = proxy.send_event(UserEvent::DevicesPageReady);
                            } else if message == "close" {
                                let _ = proxy.send_event(UserEvent::DevicesCloseRequested);
                            } else if let Some(height) = message
                                .strip_prefix("height:")
                                .and_then(|v| v.parse::<f64>().ok())
                            {
                                let _ = proxy.send_event(UserEvent::DevicesContentHeight(height));
                            }
                        }) {
                            Ok(window) => devices_window = Some(window),
                            Err(err) => {
                                tracing::warn!(%err, "devices window unavailable");
                            }
                        }
                    }
                    match devices_window.as_mut() {
                        Some(window) => {
                            window.show();
                            fetch_devices_for_window(
                                &action_handle,
                                socket.clone(),
                                devices_proxy.clone(),
                            );
                        }
                        None => run_devices_action(
                            &action_handle,
                            socket.clone(),
                            Arc::clone(&action_notifier),
                        ),
                    }
                } else if id == ids::VIEW_LOGS {
                    // The daemon's logs; the folder also holds vhidd logs.
                    open_url("/Library/Logs/KanataBar");
                } else if id == ids::DOCTOR {
                    run_doctor_action(
                        &action_handle,
                        socket.clone(),
                        Arc::clone(&action_notifier),
                        DoctorAction::Report,
                    );
                } else if id == ids::WIZARD {
                    run_doctor_action(
                        &action_handle,
                        socket.clone(),
                        Arc::clone(&action_notifier),
                        DoctorAction::Wizard,
                    );
                } else if let Some(payload) = menu::payload_for(&id) {
                    // Fire-and-forget: the event stream reflects the result.
                    if command_tx.send(payload).is_err() {
                        tracing::warn!("command channel closed");
                    }
                }
            }
            Event::UserEvent(UserEvent::Devices(view)) => {
                if let Some(window) = devices_window.as_mut() {
                    match serde_json::to_string(&*view) {
                        Ok(json) => window.render(json),
                        Err(err) => tracing::warn!(%err, "devices view serialization failed"),
                    }
                }
            }
            Event::UserEvent(UserEvent::DevicesPageReady) => {
                if let Some(window) = devices_window.as_mut() {
                    window.page_ready();
                }
            }
            Event::UserEvent(UserEvent::DevicesContentHeight(height)) => {
                if let Some(window) = devices_window.as_mut() {
                    window.fit_content_height(height);
                }
            }
            Event::UserEvent(UserEvent::DevicesCloseRequested) => {
                if let Some(window) = devices_window.as_ref() {
                    window.hide();
                }
            }
            Event::UserEvent(UserEvent::DevicesChanged) => {
                // Hotplug while the window is showing: refresh in place
                // (SPEC §8). Hidden windows don't fetch.
                if devices_window.as_ref().is_some_and(ShellWindow::is_visible) {
                    fetch_devices_for_window(&action_handle, socket.clone(), devices_proxy.clone());
                }
            }
            Event::WindowEvent {
                window_id,
                event: WindowEvent::CloseRequested,
                ..
            } => {
                // Phase 12 windows hide on close (instant re-open, state kept).
                if let Some(window) = devices_window.as_ref() {
                    if window.id() == window_id {
                        window.hide();
                    }
                }
            }
            _ => {}
        }
    })
}

/// Fetch the device list off-thread and marshal the view-model back into the
/// event loop for the devices window (SPEC §8, Phase 12). Errors render in
/// the window (devwin::error), not as notifications.
fn fetch_devices_for_window(
    handle: &tokio::runtime::Handle,
    socket: PathBuf,
    proxy: EventLoopProxy<UserEvent>,
) {
    handle.spawn(async move {
        let view = match conn::fetch_devices(&socket).await {
            Ok(devices) => devwin::view(&devices),
            Err(err) => devwin::error(&err.to_string()),
        };
        let _ = proxy.send_event(UserEvent::Devices(Box::new(view)));
    });
}

/// Bridge `conn::Update`s to the main thread; deliver notifications off-thread
/// so `osascript` never stalls the UI.
async fn forward_updates(
    mut updates: UnboundedReceiver<Update>,
    proxy: EventLoopProxy<UserEvent>,
    notifier: Arc<dyn Notifier>,
) {
    while let Some(update) = updates.recv().await {
        match update {
            Update::Model(model) => {
                if proxy.send_event(UserEvent::Model(Box::new(model))).is_err() {
                    return; // Event loop gone.
                }
            }
            Update::Notify { title, body } => {
                let notifier = Arc::clone(&notifier);
                tokio::task::spawn_blocking(move || notifier.notify(&title, &body));
            }
            Update::DevicesChanged => {
                if proxy.send_event(UserEvent::DevicesChanged).is_err() {
                    return; // Event loop gone.
                }
            }
        }
    }
}

/// Drain queued menu commands, each on its own short-lived connection.
async fn run_commands(socket: PathBuf, mut commands: UnboundedReceiver<RequestPayload>) {
    while let Some(payload) = commands.recv().await {
        let socket = socket.clone();
        tokio::spawn(async move {
            if let Err(err) = conn::send_command(&socket, payload).await {
                tracing::warn!(%err, "command failed");
            }
        });
    }
}

/// Which doctor-backed action a menu click triggers.
#[derive(Debug, Clone, Copy)]
enum DoctorAction {
    /// "Run Doctor": notify a pass/fail summary; full checklist to the log.
    Report,
    /// "Setup Wizard…": open the pane for the next unmet step (SPEC §11).
    Wizard,
}

/// Fetch `doctor` on the runtime and act on it (SPEC §9, §11) without blocking
/// the UI thread. The full checklist always goes to the log (tray.err.log);
/// the user gets a notification and, for the wizard, the relevant pane opens.
fn run_doctor_action(
    handle: &tokio::runtime::Handle,
    socket: PathBuf,
    notifier: Arc<dyn Notifier>,
    action: DoctorAction,
) {
    handle.spawn(async move {
        let checks = match conn::fetch_doctor(&socket).await {
            Ok(checks) => checks,
            Err(err) => {
                notify(
                    &notifier,
                    "KanataBar Doctor",
                    &format!("doctor failed: {err}"),
                );
                return;
            }
        };
        for check in &checks {
            if check.ok {
                tracing::info!(check = %check.name, "ok: {}", check.detail);
            } else {
                tracing::warn!(
                    check = %check.name,
                    "FAIL: {} ({})",
                    check.detail,
                    check.fix_hint.as_deref().unwrap_or("no fix hint"),
                );
            }
        }

        match action {
            DoctorAction::Report => {
                match kanatabar_core::doctor::format_failures_summary(&checks) {
                    // All green: a headline is enough.
                    None => notify(&notifier, "KanataBar Doctor", "All checks passed."),
                    // Something's wrong: name the failing checks in the banner,
                    // and open the full report (details + fix hints) — the tray
                    // has no terminal, so this mirrors `kanatactl doctor`.
                    Some(summary) => {
                        open_doctor_report(&kanatabar_core::doctor::format_report(&checks));
                        notify(&notifier, "KanataBar Doctor", &summary);
                    }
                }
            }
            DoctorAction::Wizard => {
                // The checklist alone can miss runtime-only failures (TCC is
                // unreadable, so a denial never fails a static check); ask
                // the supervisor for its structured degraded reason too.
                // Best-effort: a fetch failure just means checklist-only.
                let degraded = conn::fetch_status_once(&socket)
                    .await
                    .ok()
                    .and_then(|status| status.degraded_reason);
                match wizard::first_unsatisfied(&checks, degraded) {
                    Some(step) => {
                        if let Some(argv) = step.run {
                            run_step_command(argv).await;
                        }
                        if let Some(url) = step.open {
                            open_url(url);
                        }
                        notify(
                            &notifier,
                            &format!("Setup: {}", step.title),
                            step.instruction,
                        );
                    }
                    None => {
                        // Setup complete. If no preset is configured yet, help
                        // the user turn an existing kanata config into one so
                        // they don't finish onboarding still remapping nothing.
                        let (title, body) = wizard_completion(&socket).await;
                        notify(&notifier, &title, &body);
                    }
                }
            }
        }
    });
}

/// The wizard's terminal message. All checks pass; if no preset is configured,
/// scan `~/.config/kanata` and offer to import an existing config (the seamless
/// path for an existing kanata user), otherwise show how to add one.
async fn wizard_completion(socket: &std::path::Path) -> (String, String) {
    let has_preset = conn::fetch_presets_once(socket)
        .await
        .map(|p| !p.is_empty())
        .unwrap_or(true); // on error, don't nag — assume set up
    if has_preset {
        return (
            "KanataBar Setup".to_string(),
            "All checks passed — you're set up.".to_string(),
        );
    }
    let found = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| existing_kanata_configs(&home))
        .unwrap_or_default();
    if let Some(first) = found.first() {
        let name = first.file_stem().and_then(|s| s.to_str()).unwrap_or("main");
        (
            "KanataBar is ready".to_string(),
            format!(
                "All checks passed. Found your kanata config — add it as a preset to \
                 start remapping:\n  kanatactl preset add {name} {}",
                first.display()
            ),
        )
    } else {
        (
            "KanataBar is ready".to_string(),
            "All checks passed. Add a preset to start remapping:\n  \
             kanatactl preset add <name> <path/to.kbd>"
                .to_string(),
        )
    }
}

/// The `.kbd` files in the user's `~/.config/kanata`, sorted. Empty when the
/// directory is absent. Runs in the tray, as the user (no daemon/root).
fn existing_kanata_configs(home: &std::path::Path) -> Vec<PathBuf> {
    let dir = kanatabar_core::kanata::kanata_config_dir(home);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut kbds: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "kbd"))
        .collect();
    kbds.sort();
    kbds
}

/// Deliver a notification off the async runtime (osascript blocks).
fn notify(notifier: &Arc<dyn Notifier>, title: &str, body: &str) {
    let notifier = Arc::clone(notifier);
    let (title, body) = (title.to_string(), body.to_string());
    tokio::task::spawn_blocking(move || notifier.notify(&title, &body));
}

/// `open(1)` a URL or System Settings pane (SPEC §11: openable, not clickable).
fn open_url(url: &str) {
    if let Err(err) = std::process::Command::new("open").arg(url).status() {
        tracing::warn!(%err, url, "failed to open");
    }
}

/// `open -t`: the user's default *text* editor, not whatever app claims the
/// `.kbd` extension (SPEC §8 "Edit Config (opens in default editor)").
fn open_in_editor(path: &str) {
    if let Err(err) = std::process::Command::new("open")
        .args(["-t", path])
        .status()
    {
        tracing::warn!(%err, path, "failed to open config in editor");
    }
}

/// Write the full doctor report to a temp file and open it in the default text
/// viewer — the tray has no terminal, so "Run Doctor" shows the same detail
/// `kanatactl doctor` prints (SPEC §9). Best-effort; a write/open failure just
/// leaves the notification summary.
fn open_doctor_report(report: &str) {
    let path = std::env::temp_dir().join("kanatabar-doctor.txt");
    match std::fs::write(&path, report) {
        Ok(()) => open_in_editor(&path.display().to_string()),
        Err(err) => tracing::warn!(%err, "could not write doctor report"),
    }
}

/// "Validate Config": re-check the active preset's `.kbd` over a fresh
/// connection and report the verdict as a notification (SPEC §8).
fn run_validate_action(
    handle: &tokio::runtime::Handle,
    socket: PathBuf,
    notifier: Arc<dyn Notifier>,
    path: String,
) {
    handle.spawn(async move {
        let payload = RequestPayload::ValidateConfig { path: path.clone() };
        match conn::send_command(&socket, payload).await {
            Ok(()) => notify(&notifier, "KanataBar", &format!("Config OK: {path}")),
            Err(err) => notify(&notifier, "KanataBar", &format!("Config invalid: {err}")),
        }
    });
}

/// "Devices": fetch the daemon's device list and summarize it (SPEC §8); the
/// full list goes to the tray log.
fn run_devices_action(
    handle: &tokio::runtime::Handle,
    socket: PathBuf,
    notifier: Arc<dyn Notifier>,
) {
    handle.spawn(async move {
        match conn::fetch_devices(&socket).await {
            Ok(devices) => {
                let matched: Vec<&str> = devices
                    .iter()
                    .filter(|d| d.matched)
                    .map(|d| d.name.as_str())
                    .collect();
                for device in &devices {
                    tracing::info!(
                        name = %device.name,
                        matched = device.matched,
                        "device"
                    );
                }
                let body = if devices.is_empty() {
                    "No devices recorded yet.".to_string()
                } else {
                    format!(
                        "{} device(s); matched: {}",
                        devices.len(),
                        if matched.is_empty() {
                            "none".to_string()
                        } else {
                            matched.join(", ")
                        }
                    )
                };
                notify(&notifier, "KanataBar Devices", &body);
            }
            Err(err) => notify(&notifier, "KanataBar Devices", &format!("failed: {err}")),
        }
    });
}

/// Run a wizard step's command (SPEC §11 — e.g. the Karabiner manager's
/// `activate` request) off the UI thread; failure is logged, not fatal — the
/// step's instruction still tells the user what to do by hand.
async fn run_step_command(argv: &'static [&'static str]) {
    let result = tokio::task::spawn_blocking(move || {
        std::process::Command::new(argv[0])
            .args(&argv[1..])
            .status()
    })
    .await;
    match result {
        Ok(Ok(status)) if status.success() => {
            tracing::info!(cmd = ?argv, "wizard step command succeeded");
        }
        Ok(Ok(status)) => tracing::warn!(cmd = ?argv, ?status, "wizard step command failed"),
        Ok(Err(err)) => tracing::warn!(cmd = ?argv, %err, "wizard step command not runnable"),
        Err(err) => tracing::warn!(cmd = ?argv, %err, "wizard step command panicked"),
    }
}

/// Run the "Launch at Login" toggle and return the new loaded state. Disabling
/// boots out our own agent, which terminates this process — expected (SPEC §8).
fn toggle_login(uid: u32, home: Option<&std::path::Path>, loaded: bool) -> bool {
    let Some(home) = home else {
        tracing::warn!("HOME unset; cannot locate the agent plist");
        return loaded;
    };
    let plist = login::default_agent_plist(home);
    let args = login::toggle_args(loaded, uid, &plist);
    match std::process::Command::new(login::LAUNCHCTL)
        .args(&args)
        .status()
    {
        Ok(status) if status.success() => !loaded,
        Ok(status) => {
            tracing::warn!(?status, "launchctl toggle failed");
            loaded
        }
        Err(err) => {
            tracing::warn!(%err, "launchctl not runnable");
            loaded
        }
    }
}

/// Build the status item with the current model.
fn build_tray(model: &MenuModel, login_loaded: bool) -> Option<TrayIcon> {
    let menu = build_menu(model, login_loaded);
    let mut builder = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("KanataBar")
        .with_icon_as_template(true);
    if let Some(icon) = tray_icon_image(model) {
        builder = builder.with_icon(icon);
    }
    match builder.build() {
        Ok(tray) => Some(tray),
        Err(err) => {
            tracing::error!(%err, "failed to create the status item");
            None
        }
    }
}

/// Push a fresh menu + icon onto an existing status item.
fn apply_model(tray: &TrayIcon, model: &MenuModel, login_loaded: bool) {
    tray.set_menu(Some(Box::new(build_menu(model, login_loaded))));
    if let Some(icon) = tray_icon_image(model) {
        // `set_icon` hard-codes the template flag to `false` (tray-icon 0.24),
        // so a plain update drops template tinting and the glyph renders as raw
        // black instead of tinting white/black to the menu bar. Set both.
        if let Err(err) = tray.set_icon_with_as_template(Some(icon), true) {
            tracing::warn!(%err, "failed to update the status icon");
        }
    }
}

fn tray_icon_image(model: &MenuModel) -> Option<Icon> {
    Icon::from_rgba(
        icons::rgba_for(model.icon),
        icons::ICON_SIZE,
        icons::ICON_SIZE,
    )
    .map_err(|err| tracing::warn!(%err, "bad icon buffer"))
    .ok()
}

/// Translate a [`MenuModel`] into a `muda` menu (SPEC §8 layout).
fn build_menu(model: &MenuModel, login_loaded: bool) -> Menu {
    let menu = Menu::new();

    // Disabled status lines at the top.
    let _ = menu.append(&MenuItem::with_id("status", &model.state_line, false, None));
    if let Some(layer) = &model.layer_line {
        let _ = menu.append(&MenuItem::with_id("layer", layer, false, None));
    }
    let _ = menu.append(&PredefinedMenuItem::separator());

    // Lifecycle controls, enabled per the state machine's capabilities.
    let caps = model.caps;
    let _ = menu.append(&MenuItem::with_id(ids::START, "Start", caps.start, None));
    let _ = menu.append(&MenuItem::with_id(ids::STOP, "Stop", caps.stop, None));
    let _ = menu.append(&MenuItem::with_id(
        ids::RESTART,
        "Restart",
        caps.restart,
        None,
    ));
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&MenuItem::with_id(ids::PAUSE, "Pause", caps.pause, None));
    let _ = menu.append(&MenuItem::with_id(ids::RESUME, "Resume", caps.resume, None));
    let _ = menu.append(&PredefinedMenuItem::separator());

    // Presets submenu with a checkmark on the active one (SPEC §8).
    let presets = Submenu::new("Presets", model.connected && !model.presets.is_empty());
    for preset in &model.presets {
        let _ = presets.append(&CheckMenuItem::with_id(
            menu::preset_id(&preset.name),
            &preset.name,
            true,
            preset.active,
            None,
        ));
    }
    let _ = menu.append(&presets);
    let _ = menu.append(&PredefinedMenuItem::separator());

    // Config & inspection (SPEC §8). Edit/Validate need an active preset path.
    let has_config = model.active_config.is_some();
    let _ = menu.append(&MenuItem::with_id(
        ids::EDIT_CONFIG,
        "Edit Config",
        has_config,
        None,
    ));
    let _ = menu.append(&MenuItem::with_id(
        ids::VALIDATE_CONFIG,
        "Validate Config",
        has_config,
        None,
    ));
    let _ = menu.append(&MenuItem::with_id(
        ids::DEVICES,
        "Devices…",
        model.connected,
        None,
    ));
    let _ = menu.append(&MenuItem::with_id(ids::VIEW_LOGS, "View Logs", true, None));
    let _ = menu.append(&PredefinedMenuItem::separator());

    // Diagnostics & onboarding (SPEC §9, §11).
    let _ = menu.append(&MenuItem::with_id(ids::WIZARD, "Setup Wizard…", true, None));
    let _ = menu.append(&MenuItem::with_id(ids::DOCTOR, "Run Doctor", true, None));
    let _ = menu.append(&PredefinedMenuItem::separator());

    // Per-user agent toggle (SPEC §8).
    let _ = menu.append(&CheckMenuItem::with_id(
        ids::LOGIN,
        "Launch at Login",
        true,
        login_loaded,
        None,
    ));
    let _ = menu.append(&PredefinedMenuItem::separator());

    // Quit the tray only (the daemon keeps running — say so, SPEC §8).
    let _ = menu.append(&MenuItem::with_id(
        ids::QUIT,
        "Quit KanataBar (daemon keeps running)",
        true,
        None,
    ));

    menu
}
