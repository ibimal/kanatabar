//! kanatad — KanataBar root daemon (SPEC §6).
//!
//! `kanatad run` supervises kanata in the foreground; launchd keeps *us*
//! alive. Graceful shutdown [HARD]: SIGTERM/SIGINT → stop child (SIGTERM →
//! ≤3s → SIGKILL) → persist state.json → exit 0 so launchd sees clean exits.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use kanatabar_core::backoff::BackoffConfig;
use kanatad::config::{ActiveConfig, SupervisorConfig};
use kanatad::configmgr::{self, ConfigManager, ConfigPaths};
use kanatad::control::{self, ControlConfig};
use kanatad::device::{self, DeviceRegistry, IoKitMonitor, Monitor};
use kanatad::events::DaemonEvents;
use kanatad::health::{self, HealthState};
use kanatad::logbuf::LogBuffer;
use kanatad::{statefile, supervisor, watch};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc;
use tracing::info;

/// Default control-socket path (SPEC §3.2).
const DEFAULT_SOCKET: &str = "/var/run/kanatabar.sock";
/// Default support dir when `--state-dir` is unset (SPEC §3.2).
const DEFAULT_SUPPORT_DIR: &str = "/Library/Application Support/KanataBar";

#[derive(Parser)]
#[command(
    name = "kanatad",
    version,
    about = "KanataBar supervisor daemon for kanata"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the supervisor in the foreground (launchd invokes this).
    Run(RunArgs),
}

/// Phase 1 wiring: flags/env. Phase 3 moves defaults into the TOML config
/// (SPEC §7.3); the env overrides stay for tests (CLAUDE.md).
#[derive(Args)]
struct RunArgs {
    /// kanata binary to supervise. Omit to resolve from config.toml
    /// `defaults.kanata_bin`, then the well-known install locations
    /// (/usr/local/bin, /opt/homebrew/bin — SPEC §7.3; never $PATH, §14).
    #[arg(long, env = "KANATABAR_KANATA_BIN")]
    kanata_bin: Option<PathBuf>,

    /// kanata .kbd config passed as --cfg. Omit to let the daemon pick: the
    /// config.toml autostart preset if one exists, else the built-in safe
    /// config materialized at `<state-dir>/safe.kbd` — so the launchd job
    /// (SPEC §10, `kanatad run` with no arguments) always has something to
    /// spawn on a fresh install.
    #[arg(long, env = "KANATABAR_CFG")]
    cfg: Option<PathBuf>,

    /// Extra argument appended to every kanata invocation (repeatable).
    #[arg(long = "extra-arg", value_name = "ARG", allow_hyphen_values = true)]
    extra_args: Vec<String>,

    /// Directory for state.json; omitted → no persistence (dev mode).
    #[arg(long, env = "KANATABAR_STATE")]
    state_dir: Option<PathBuf>,

    /// Control socket path (SPEC §3.2, §7.1).
    #[arg(long, env = "KANATABAR_SOCK", default_value = DEFAULT_SOCKET)]
    socket: PathBuf,

    /// Numeric gid to own the control socket (SPEC §7.1 `root:staff`);
    /// best-effort, needs root. Install (Phase 6) sets this.
    #[arg(long, env = "KANATABAR_SOCK_GID")]
    socket_gid: Option<u32>,

    /// Skip the Karabiner driver preflight (SPEC §6.5). For dev/CI without the
    /// real driver installed; production runs the check.
    #[arg(long, env = "KANATABAR_SKIP_DRIVER_CHECK")]
    skip_driver_check: bool,

    /// First crash-retry delay in milliseconds.
    #[arg(long, default_value_t = BackoffConfig::default().base_ms)]
    backoff_base_ms: u64,

    /// Ceiling for the crash-retry delay in milliseconds.
    #[arg(long, default_value_t = BackoffConfig::default().cap_ms)]
    backoff_cap_ms: u64,

    /// Consecutive failures before Degraded.
    #[arg(long, default_value_t = BackoffConfig::default().budget)]
    backoff_budget: u32,

    /// Healthy seconds that reset the failure budget.
    #[arg(long, default_value_t = BackoffConfig::default().reset_after_s)]
    backoff_reset_after_s: u64,
}

impl RunArgs {
    fn into_config(self, kanata_bin: PathBuf, cfg: PathBuf) -> SupervisorConfig {
        let backoff = BackoffConfig {
            base_ms: self.backoff_base_ms,
            cap_ms: self.backoff_cap_ms,
            budget: self.backoff_budget,
            reset_after_s: self.backoff_reset_after_s,
        };
        SupervisorConfig {
            extra_args: self.extra_args,
            state_dir: self.state_dir,
            healthy_window: Duration::from_secs(backoff.reset_after_s),
            backoff,
            ..SupervisorConfig::new(kanata_bin, cfg)
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    // In-memory ring mirroring the stderr log, served by GetLogs/FollowLogs
    // (SPEC §6.6, §7.2). Same filter as the visible log.
    let logbuf = LogBuffer::default();
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                // Plain text when piped (launchd log files, tests); color on a TTY.
                .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr())),
        )
        .with(logbuf.layer())
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Run(args) => run(args, logbuf).await,
    }
}

/// Default group for the control socket when none is given: `staff` under a
/// real root run, so the socket comes up `root:staff 0660` (SPEC §3.2, §7.1)
/// and console users can connect without sudo. HW finding (2026-07-11): the
/// launchd plist is bare `kanatad run` (SPEC §10 template), so without this
/// default the socket inherited root's `daemon` group and every unprivileged
/// `kanatactl` got EACCES before peer-cred auth even ran. Unprivileged
/// dev/test runs keep the creating user's group as before (`None`).
fn default_socket_gid() -> Option<u32> {
    if !nix::unistd::geteuid().is_root() {
        return None;
    }
    match nix::unistd::Group::from_name("staff") {
        Ok(Some(group)) => Some(group.gid.as_raw()),
        // `staff` is gid 20 on every macOS; fall back if lookup fails.
        _ => Some(20),
    }
}

async fn run(args: RunArgs, logbuf: LogBuffer) -> Result<()> {
    let started = Instant::now();

    // Install signal handlers *before* the supervisor can spawn kanata and
    // reach Running: launchd (or a test) may SIGTERM us the instant we come up,
    // and until the tokio handler is registered the default disposition would
    // terminate the process uncleanly (exit code None) instead of the graceful
    // exit 0 launchd must see (SPEC §6.1 [HARD]).
    let mut sigterm = signal(SignalKind::terminate()).context("installing SIGTERM handler")?;
    let mut sigint = signal(SignalKind::interrupt()).context("installing SIGINT handler")?;

    let control_config = ControlConfig {
        socket_path: args.socket.clone(),
        socket_gid: args.socket_gid.or_else(default_socket_gid),
    };
    let support_dir = args
        .state_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_SUPPORT_DIR));
    let paths = ConfigPaths::under(&support_dir);

    // Load config.toml (presets/defaults) before building the config so the TCP
    // port and autostart preset are known.
    let config_file = configmgr::load_config_file(&paths.config_toml);
    let tcp_port = config_file
        .as_ref()
        .map_or(5829u16, |f| f.defaults.tcp_port);
    let skip_driver_check = args.skip_driver_check;

    // No --cfg/env and no autostart preset (a bare `kanatad run`, as launchd
    // invokes it — SPEC §10): fall back to the built-in safe config so the
    // daemon always has a valid spawn target on a fresh install.
    let resolved_cfg = match args.cfg.clone() {
        Some(cfg) => cfg,
        None => {
            configmgr::materialize_safe_config(&paths.safe_kbd).with_context(|| {
                format!(
                    "materializing built-in safe config at {} — the default support dir \
                     needs root (launchd runs kanatad as root); for an unprivileged \
                     foreground run use `just run-dev` or set --state-dir/KANATABAR_STATE \
                     to a writable directory",
                    paths.safe_kbd.display()
                )
            })?;
            paths.safe_kbd.clone()
        }
    };

    // Resolve the kanata binary (SPEC §7.3): CLI/env, then config.toml
    // `defaults.kanata_bin`, then the first existing well-known location
    // (never $PATH — the daemon runs this as root, §14). Logged below and
    // surfaced by `doctor`; the resolution is deterministic so TCC grants
    // stay attached to the same binary across restarts (SPEC §2).
    let explicit_bin = args.kanata_bin.clone().or_else(|| {
        config_file
            .as_ref()
            .map(|f| f.defaults.kanata_bin.trim())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
    });
    let kanata_bin = kanatad::config::resolve_kanata_bin(explicit_bin, |path| path.is_file());

    let health = HealthState::default();
    let mut config = args.into_config(kanata_bin, resolved_cfg);
    config.tcp_port = Some(tcp_port);
    config.health = health.clone();
    // Driver preflight on in production; skippable for dev/CI (SPEC §6.5).
    config.driver_probe = if skip_driver_check {
        None
    } else {
        Some(health::driver::system_probe())
    };

    // An autostart preset picks the initial active config; otherwise fall back
    // to --cfg (dev/tests) (§6.4).
    let mut target = config.initial_target();
    let mut initial_preset = None;
    if let Some((name, def)) = config_file.as_ref().and_then(|f| f.autostart_preset()) {
        target.kanata_cfg = PathBuf::from(&def.config);
        if let Some(bin) = def.kanata_bin.as_deref().filter(|s| !s.is_empty()) {
            target.kanata_bin = PathBuf::from(bin);
        }
        if !def.extra_args.is_empty() {
            target.extra_args = def.extra_args.clone();
        }
        initial_preset = Some(name.to_string());
    }
    let debounce = std::time::Duration::from_millis(
        config_file.as_ref().map_or(500, |f| f.defaults.debounce_ms),
    );

    let active = ActiveConfig::new(target.clone());
    if let Some(preset) = initial_preset {
        active.set_active(target.kanata_cfg.clone(), Some(preset), None, None, None);
    }

    info!(
        version = env!("CARGO_PKG_VERSION"),
        kanata_bin = %target.kanata_bin.display(),
        cfg = %target.kanata_cfg.display(),
        socket = %control_config.socket_path.display(),
        "kanatad starting"
    );

    // Orphan sweep before the first spawn: kill kanata left by a previous
    // daemon (kill -9) so the single-instance invariant holds (SPEC §6.1b).
    if let Some(state_dir) = &config.state_dir {
        if let Some(persisted) = statefile::load(state_dir) {
            health::orphan::sweep(persisted.kanata_pid, &config.kanata_bin, config.kill_grace)
                .await;
        }
    }
    // Stray sweep (§6.1b "also scan"): catches a surviving kanata even when
    // state.json was lost/corrupt. Root-only — in dev/tests, parallel daemons
    // share one mock-kanata binary and must not kill each other's children.
    if nix::unistd::geteuid().is_root() {
        health::orphan::sweep_strays(&config.kanata_bin, config.kill_grace).await;
    }

    // Bus for DeviceChanged/ConfigApplied pushes (SPEC §7.2) and, from the
    // supervisor, Event::Crash on a plain crash (SPEC §8). Created before the
    // supervisor so it shares the exact bus the control server subscribes to.
    let bus = DaemonEvents::default();
    let registry = DeviceRegistry::default();
    config.events = bus.clone();

    let default_bin = config.kanata_bin.clone();
    let preflight_timeout = config.preflight_timeout;
    // Cloned before the supervisor takes ownership so `doctor` can re-run the
    // same driver preflight on demand (SPEC §9); `None` when disabled.
    let doctor_probe = config.driver_probe.clone();
    let handle = supervisor::start_with(config, active.clone());
    handle
        .send(supervisor::Command::Start)
        .await
        .context("supervisor loop died at startup")?;

    let configmgr = ConfigManager::load(
        handle.client(),
        active.clone(),
        paths,
        default_bin,
        preflight_timeout,
        bus.clone(),
    );

    // Watch the active config for on-disk edits (SPEC §6.4).
    let watch_handle = match watch::spawn(configmgr.clone(), active, debounce) {
        Ok(handle) => Some(handle),
        Err(err) => {
            info!(%err, "config file watch disabled");
            None
        }
    };

    // Device monitor → debounced re-sync restart (SPEC §6.3). The IOKit source
    // may be unavailable (permissions, headless); degrade to no re-sync rather
    // than fail to start.
    let (device_tx, device_rx) = mpsc::unbounded_channel();
    let device_task = tokio::spawn(device::run(
        device_rx,
        handle.client(),
        debounce,
        registry.clone(),
        bus.clone(),
    ));
    let device_monitor = match Box::new(IoKitMonitor).start(device_tx) {
        Ok(monitor) => Some(monitor),
        Err(err) => {
            info!(%err, "device monitor disabled (no automatic re-sync on hotplug)");
            None
        }
    };

    // kanata TCP layer relay → active_layer in Status (SPEC §6.5).
    let relay_task = tokio::spawn(health::tcp::run(tcp_port, handle.client(), health.clone()));

    // Sleep/wake monitor: on wake, re-sync kanata if it's running (SPEC §6.5).
    // The wake callback runs on the CFRunLoop thread, so it just nudges an
    // async task through a channel.
    let (wake_tx, mut wake_rx) = mpsc::unbounded_channel::<()>();
    let wake_client = handle.client();
    let wake_task = tokio::spawn(async move {
        while wake_rx.recv().await.is_some() {
            if wake_client.snapshot().state == kanatabar_core::state::SupervisorState::Running {
                info!("re-syncing kanata after wake");
                let _ = wake_client.send(supervisor::Command::Restart).await;
            }
        }
    });
    let power_monitor = match health::start_power_monitor(Box::new(move || {
        let _ = wake_tx.send(());
    })) {
        Ok(monitor) => Some(monitor),
        Err(err) => {
            info!(%err, "sleep/wake monitor disabled (no wake re-sync)");
            None
        }
    };

    // Serve the control socket concurrently; it runs until we abort it.
    let mut control_task = tokio::spawn(control::serve(
        control_config.clone(),
        handle.client(),
        configmgr,
        health.clone(),
        control::auth::system_policy(),
        started,
        doctor_probe,
        logbuf,
        registry,
        bus,
    ));

    tokio::select! {
        _ = sigterm.recv() => info!("SIGTERM received"),
        _ = sigint.recv() => info!("SIGINT received"),
        result = &mut control_task => match result {
            Ok(Ok(())) => info!("control server stopped"),
            Ok(Err(err)) => info!(%err, "control server error"),
            Err(err) => info!(%err, "control server task panicked"),
        },
    }

    control_task.abort();
    if let Some(watch_handle) = watch_handle {
        watch_handle.abort();
    }
    drop(device_monitor); // stops the IOKit CFRunLoop thread
    drop(power_monitor); // stops the power CFRunLoop thread
    device_task.abort();
    relay_task.abort();
    wake_task.abort();
    handle.shutdown().await?;
    // Leave nothing behind (SPEC §7.1: recreated each start; §10 uninstall).
    let _ = std::fs::remove_file(&control_config.socket_path);
    info!("kanatad exited cleanly");
    Ok(())
}
