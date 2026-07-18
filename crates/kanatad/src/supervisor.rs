//! The supervisor loop: drives the core state machine, owns the kanata child
//! and the timers, publishes transitions to subscribers (SPEC §6.1, §6.2).
//!
//! Zero polling [HARD]: the loop is a single `select!` over the command
//! channel, the child's exit, and the one-shot timers. **Scoped exception
//! (SPEC §6.5, HW ledger #19):** while `Degraded{InputMonitoringDenied}` —
//! and only then — a grant watch polls the fresh-child TCC probe every few
//! seconds, because macOS offers no notification API for TCC changes (even
//! GUI apps poll; Thaw ticks `AXIsProcessTrusted` every 3 s). The watch
//! disarms the moment the state changes, so steady state stays event-driven.

use std::collections::VecDeque;
use std::pin::Pin;

use kanatabar_core::kanata::{BackendEvent, StderrFault};
use kanatabar_core::machine::{Action, Machine, MachineEvent, StateChanged};
use kanatabar_core::state::{DegradedReason, SupervisorState};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Sleep;
use tracing::{debug, error, info, warn};

use crate::child::{self, ChildWake, KanataChild, Preflight, SpawnError};
use crate::config::{ActiveConfig, SupervisorConfig};
use crate::statefile::{self, PersistedState};

/// Retry-on-grant poll cadence (SPEC §6.5): how often the fresh-child TCC
/// probe runs while `Degraded{InputMonitoringDenied}`. Matches Thaw's 3 s
/// permission poll; each tick is one short-lived `kanatad tcc-status` spawn.
const GRANT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);

/// User/IPC commands the supervisor accepts (the tray and CLI map onto these
/// in Phase 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Start kanata.
    Start,
    /// Stop kanata; the daemon keeps running.
    Stop,
    /// Restart kanata (config/device change or user request).
    Restart,
    /// Pause remapping by stopping the child.
    Pause,
    /// Resume from pause.
    Resume,
    /// Terminate the daemon gracefully (SIGTERM path, SPEC §6.1 [HARD]).
    Shutdown,
}

impl Command {
    fn into_event(self) -> Option<MachineEvent> {
        match self {
            Command::Start => Some(MachineEvent::Start),
            Command::Stop => Some(MachineEvent::Stop),
            Command::Restart => Some(MachineEvent::Restart),
            Command::Pause => Some(MachineEvent::Pause),
            Command::Resume => Some(MachineEvent::Resume),
            Command::Shutdown => None,
        }
    }
}

/// Point-in-time status published on a watch channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// Current state machine state.
    pub state: SupervisorState,
    /// Why we are `Degraded`, when applicable.
    pub degraded_reason: Option<DegradedReason>,
    /// Pid of the live kanata child.
    pub kanata_pid: Option<u32>,
}

/// A cheap, cloneable client to the supervisor: the control server hands one
/// to every connection so they share the single loop (SPEC §3).
#[derive(Clone)]
pub struct SupervisorClient {
    commands: mpsc::Sender<Command>,
    events: broadcast::Sender<StateChanged>,
    snapshot: watch::Receiver<Snapshot>,
}

impl SupervisorClient {
    /// Send a command; errors only if the loop is gone.
    pub async fn send(&self, command: Command) -> anyhow::Result<()> {
        self.commands
            .send(command)
            .await
            .map_err(|_| anyhow::anyhow!("supervisor loop has exited"))
    }

    /// Subscribe to state transitions (SPEC §6.2: pushed to subscribers).
    pub fn subscribe(&self) -> broadcast::Receiver<StateChanged> {
        self.events.subscribe()
    }

    /// Latest status snapshot.
    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.borrow().clone()
    }
}

/// Handle to a running supervisor task: a [`SupervisorClient`] plus ownership
/// of the task for graceful shutdown.
pub struct SupervisorHandle {
    client: SupervisorClient,
    active: ActiveConfig,
    task: JoinHandle<()>,
}

impl SupervisorHandle {
    /// A cloneable client sharing this supervisor's loop.
    pub fn client(&self) -> SupervisorClient {
        self.client.clone()
    }

    /// The shared, mutable spawn target (the config manager updates it).
    pub fn active_config(&self) -> ActiveConfig {
        self.active.clone()
    }

    /// Send a command; errors only if the loop is gone.
    pub async fn send(&self, command: Command) -> anyhow::Result<()> {
        self.client.send(command).await
    }

    /// Subscribe to state transitions (SPEC §6.2: pushed to subscribers).
    pub fn subscribe(&self) -> broadcast::Receiver<StateChanged> {
        self.client.subscribe()
    }

    /// Latest status snapshot.
    pub fn snapshot(&self) -> Snapshot {
        self.client.snapshot()
    }

    /// Graceful shutdown: stop the child, persist state, wait for the loop.
    pub async fn shutdown(self) -> anyhow::Result<()> {
        // A closed channel already means the loop is gone; that's fine.
        let _ = self.client.commands.send(Command::Shutdown).await;
        self.task.await.map_err(anyhow::Error::from)
    }
}

/// Spawn the supervisor loop. The caller decides whether to autostart by
/// sending [`Command::Start`].
pub fn start(config: SupervisorConfig) -> SupervisorHandle {
    let active = ActiveConfig::new(config.initial_target());
    start_with(config, active)
}

/// Like [`start`], but with a caller-provided [`ActiveConfig`] so the config
/// manager and supervisor share the exact same handle.
pub fn start_with(config: SupervisorConfig, active: ActiveConfig) -> SupervisorHandle {
    let (cmd_tx, cmd_rx) = mpsc::channel(64);
    let (event_tx, _) = broadcast::channel(256);
    let (snap_tx, snap_rx) = watch::channel(Snapshot {
        state: SupervisorState::Stopped,
        degraded_reason: None,
        kanata_pid: None,
    });

    let task = tokio::spawn(
        Task {
            machine: Machine::new(config.backoff),
            active: active.clone(),
            config,
            commands: cmd_rx,
            child: None,
            backoff_timer: None,
            healthy_timer: None,
            backend_timer: None,
            grant_timer: None,
            events: event_tx.clone(),
            snapshot: snap_tx,
        }
        .run(),
    );

    SupervisorHandle {
        client: SupervisorClient {
            commands: cmd_tx,
            events: event_tx,
            snapshot: snap_rx,
        },
        active,
        task,
    }
}

enum Wake {
    Command(Option<Command>),
    ChildExited(std::io::Result<std::process::ExitStatus>),
    ChildBackend(BackendEvent),
    BackoffElapsed,
    HealthyElapsed,
    BackendGraceElapsed,
    GrantPollElapsed,
}

struct Task {
    config: SupervisorConfig,
    active: ActiveConfig,
    machine: Machine,
    commands: mpsc::Receiver<Command>,
    child: Option<KanataChild>,
    backoff_timer: Option<Pin<Box<Sleep>>>,
    healthy_timer: Option<Pin<Box<Sleep>>>,
    /// Grace window between the child reporting its output backend gone and
    /// the `Degraded{OutputBackendUnavailable}` transition; a recovery line
    /// inside the window cancels it (SPEC §6.5, HW 2026-07-11).
    backend_timer: Option<Pin<Box<Sleep>>>,
    /// Retry-on-grant watch (SPEC §6.5; HW ledger #19): armed only while
    /// `Degraded{InputMonitoringDenied}` — each tick asks the fresh-child TCC
    /// probe whether BOTH grants are now present, and restarts kanata the
    /// moment they are. A TCC denial used to be a terminal no-retry state
    /// because the grant was unobservable; the probe makes it observable, so
    /// no-retry becomes retry-exactly-when-granted (never a blind loop).
    grant_timer: Option<Pin<Box<Sleep>>>,
    events: broadcast::Sender<StateChanged>,
    snapshot: watch::Sender<Snapshot>,
}

impl Task {
    async fn run(mut self) {
        info!("supervisor loop started");
        loop {
            self.sync_grant_watch();
            let wake = {
                // Split borrows so the select arms don't fight over `self`.
                let Task {
                    commands,
                    child,
                    backoff_timer,
                    healthy_timer,
                    backend_timer,
                    grant_timer,
                    ..
                } = &mut self;

                tokio::select! {
                    biased;
                    cmd = commands.recv() => Wake::Command(cmd),
                    wake = async {
                        match child.as_mut() {
                            Some(c) => c.next_wake().await,
                            None => std::future::pending().await,
                        }
                    } => match wake {
                        ChildWake::Exited(status) => Wake::ChildExited(status),
                        ChildWake::Backend(event) => Wake::ChildBackend(event),
                    },
                    () = async {
                        match backoff_timer.as_mut() {
                            Some(t) => t.as_mut().await,
                            None => std::future::pending().await,
                        }
                    } => Wake::BackoffElapsed,
                    () = async {
                        match healthy_timer.as_mut() {
                            Some(t) => t.as_mut().await,
                            None => std::future::pending().await,
                        }
                    } => Wake::HealthyElapsed,
                    () = async {
                        match backend_timer.as_mut() {
                            Some(t) => t.as_mut().await,
                            None => std::future::pending().await,
                        }
                    } => Wake::BackendGraceElapsed,
                    () = async {
                        match grant_timer.as_mut() {
                            Some(t) => t.as_mut().await,
                            None => std::future::pending().await,
                        }
                    } => Wake::GrantPollElapsed,
                }
            };

            match wake {
                // Channel closed (all handles dropped) behaves like Shutdown.
                Wake::Command(None) | Wake::Command(Some(Command::Shutdown)) => {
                    info!("shutdown requested");
                    self.dispatch(MachineEvent::Stop).await;
                    self.persist();
                    info!("supervisor loop exiting cleanly");
                    return;
                }
                Wake::Command(Some(cmd)) => {
                    debug!(?cmd, "command received");
                    if let Some(event) = cmd.into_event() {
                        self.dispatch(event).await;
                    }
                }
                Wake::ChildExited(status) => {
                    // The child is gone; a pending backend grace window with it.
                    self.backend_timer = None;
                    // Drain its final output (bounded) so a fault printed just
                    // before death is classified, then drop the handle before
                    // dispatching.
                    let fault = match self.child.take() {
                        Some(mut child) => {
                            child
                                .drain_output(std::time::Duration::from_millis(250))
                                .await;
                            child.fault()
                        }
                        None => None,
                    };
                    let class = child::classify_unrequested_exit(status);
                    warn!(?class, ?fault, "kanata exited unexpectedly");
                    match (class, fault) {
                        // A crash whose output names a condition a respawn
                        // cannot fix (SPEC §2: TCC denial, device held by
                        // another remapper) → actionable Degraded, no futile
                        // backoff loop. Panic escape stays an intentional stop.
                        (
                            kanatabar_core::state::ExitClass::Crash { .. },
                            Some(StderrFault::PermissionDenied),
                        ) => {
                            error!("kanata was denied device access (Input Monitoring)");
                            self.dispatch(MachineEvent::Fault(
                                DegradedReason::InputMonitoringDenied,
                            ))
                            .await;
                        }
                        (
                            kanatabar_core::state::ExitClass::Crash { .. },
                            Some(StderrFault::DeviceInUse),
                        ) => {
                            error!("kanata could not grab devices (held by another process)");
                            self.dispatch(MachineEvent::Fault(DegradedReason::DeviceGrabConflict))
                                .await;
                        }
                        (
                            kanatabar_core::state::ExitClass::Crash { .. },
                            Some(StderrFault::PortInUse),
                        ) => {
                            error!("kanata's TCP port is already in use");
                            self.dispatch(MachineEvent::Fault(DegradedReason::TcpPortConflict))
                                .await;
                        }
                        _ => {
                            // A plain crash with no actionable fault → normal
                            // crash→backoff recovery. Publish Event::Crash so
                            // the tray posts the "kanata crashed" notification
                            // (SPEC §8): this path settles in Backoff/Running,
                            // never Degraded, so it is the ONLY crash the tray
                            // wouldn't otherwise surface. Fault-classified
                            // crashes above go to Degraded (tray notifies on
                            // that) — emitting here too would double-notify.
                            // Requested/PanicEscape aren't `Crash`, so nothing
                            // fires for them (HW 2026-07-13 Run 7).
                            if let kanatabar_core::state::ExitClass::Crash { code, signal } = class
                            {
                                self.config
                                    .events
                                    .publish(kanatabar_core::ipc::Event::Crash { code, signal });
                            }
                            self.dispatch(MachineEvent::ChildExited(class)).await;
                        }
                    }
                    // The machine may have ignored the exit (e.g. a live child
                    // dying while Degraded{OutputBackendUnavailable}); publish
                    // anyway so `kanata_pid` never goes stale.
                    self.publish_snapshot();
                    self.persist();
                }
                // Live backend health from the child's log stream (SPEC §6.5;
                // HW 2026-07-11: a driver version mismatch leaves kanata alive
                // but unremapping — all green, keyboard dead). kanata's own 10s
                // wait precedes the "down" line; the extra grace absorbs VHID-
                // daemon restarts (launchd revives it in ~1s) without flapping.
                Wake::ChildBackend(BackendEvent::Down) => {
                    if self.machine.state() == SupervisorState::Running
                        && self.backend_timer.is_none()
                    {
                        warn!(
                            grace_s = self.config.backend_grace.as_secs(),
                            "kanata reports its output backend gone (keys unremapped); \
                             degrading unless it recovers within the grace window"
                        );
                        self.backend_timer =
                            Some(Box::pin(tokio::time::sleep(self.config.backend_grace)));
                    }
                }
                Wake::ChildBackend(BackendEvent::Up) => {
                    if self.backend_timer.take().is_some() {
                        info!("output backend recovered within the grace window");
                    }
                    if self.machine.state() == SupervisorState::Degraded
                        && self.machine.degraded_reason()
                            == Some(DegradedReason::OutputBackendUnavailable)
                    {
                        info!("output backend recovered; kanata re-grabbed the devices");
                        self.dispatch(MachineEvent::OutputBackendRecovered).await;
                    }
                }
                Wake::BackendGraceElapsed => {
                    self.backend_timer = None;
                    error!(
                        "kanata's output backend still unavailable after the grace window — \
                         keys are NOT remapped (driver version mismatch?)"
                    );
                    self.dispatch(MachineEvent::OutputBackendLost).await;
                }
                Wake::BackoffElapsed => {
                    self.backoff_timer = None;
                    self.dispatch(MachineEvent::BackoffElapsed).await;
                }
                Wake::HealthyElapsed => {
                    self.healthy_timer = None;
                    info!("healthy window elapsed; retry budget reset");
                    self.dispatch(MachineEvent::HealthyElapsed).await;
                }
                Wake::GrantPollElapsed => {
                    // Disarm; sync_grant_watch re-arms next iteration if the
                    // state still calls for it (i.e. the grants aren't there
                    // yet). The probe is a ~10 ms fork/exec; its 2 s timeout
                    // bounds the worst case, and `biased` puts commands first
                    // on the next iteration either way.
                    self.grant_timer = None;
                    if crate::doctor::tcc_grants_ready().await == Some(true) {
                        info!(
                            "Input Monitoring + Accessibility now granted \
                             (fresh probe); restarting kanata"
                        );
                        self.dispatch(MachineEvent::Restart).await;
                    }
                }
            }
        }
    }

    /// Arm/disarm the retry-on-grant watch to match the current state: armed
    /// exactly while `Degraded{InputMonitoringDenied}` (and the permission
    /// checks aren't test-skipped). Idempotent; called each loop iteration.
    fn sync_grant_watch(&mut self) {
        let wanted = self.machine.state() == SupervisorState::Degraded
            && self.machine.degraded_reason() == Some(DegradedReason::InputMonitoringDenied)
            && !crate::doctor::skip_permission_checks();
        match (wanted, self.grant_timer.is_some()) {
            (true, false) => {
                debug!("TCC denial: watching for the grant (fresh probe every 3s)");
                self.grant_timer = Some(Box::pin(tokio::time::sleep(GRANT_POLL_INTERVAL)));
            }
            (false, true) => {
                self.grant_timer = None;
            }
            _ => {}
        }
    }

    /// Feed one event through the machine, executing the resulting actions.
    /// Actions can produce follow-up events (spawn results), so drain a queue.
    async fn dispatch(&mut self, event: MachineEvent) {
        let mut queue = VecDeque::from([event]);
        while let Some(event) = queue.pop_front() {
            let outcome = self.machine.handle(event);

            if let Some(change) = outcome.transition {
                // Every transition: log + push to subscribers (§6.2).
                info!(
                    from = ?change.from,
                    to = ?change.to,
                    reason = ?change.reason,
                    "state transition"
                );
            }

            // One-shot timers only make sense in the state that armed them.
            if self.machine.state() != SupervisorState::Backoff {
                self.backoff_timer = None;
            }
            if self.machine.state() != SupervisorState::Running {
                self.healthy_timer = None;
                // A pending backend grace window belongs to the Running child
                // that armed it (a restart spawns a fresh child, a stop ends
                // the story); Degraded-by-that-timer already cleared it.
                self.backend_timer = None;
            }

            for action in outcome.actions {
                if let Some(follow_up) = self.execute(action).await {
                    queue.push_back(follow_up);
                }
            }

            if let Some(change) = outcome.transition {
                // Snapshot/state.json after the actions so they reflect the
                // settled reality (child pid gone after StopChild, present
                // after SpawnChild). Subscribers get the event afterwards, so
                // a snapshot read on receipt is consistent.
                self.publish_snapshot();
                self.persist();
                let _ = self.events.send(change); // no subscribers is fine
            }
        }
    }

    /// Execute one side effect; spawn attempts report back as events.
    async fn execute(&mut self, action: Action) -> Option<MachineEvent> {
        match action {
            Action::SpawnChild => Some(self.spawn_child().await),
            Action::StopChild => {
                if let Some(child) = self.child.take() {
                    child.terminate(self.config.kill_grace).await;
                    self.publish_snapshot();
                    self.persist();
                }
                None
            }
            Action::ArmBackoff { delay_ms } => {
                debug!(delay_ms, "backoff armed");
                self.backoff_timer = Some(Box::pin(tokio::time::sleep(
                    std::time::Duration::from_millis(delay_ms),
                )));
                None
            }
            Action::ArmHealthyTimer => {
                self.healthy_timer = Some(Box::pin(tokio::time::sleep(self.config.healthy_window)));
                None
            }
        }
    }

    /// Preflight (§6.1, §6.5) then spawn; returns the machine event describing
    /// how it went. Reads the current spawn target from [`ActiveConfig`], so a
    /// preset switch or config apply takes effect on the next spawn.
    async fn spawn_child(&mut self) -> MachineEvent {
        let target = self.active.spawn_target();

        // Driver preflight [HARD] (§6.5): never spawn (or crash-loop) when the
        // Karabiner driver/daemon is unavailable — go Degraded with a reason.
        if let Some(probe) = &self.config.driver_probe {
            use crate::health::driver::DriverHealth;
            match probe().await {
                DriverHealth::Ok => self.config.health.set_driver_ok(Some(true)),
                DriverHealth::DriverNotActivated => {
                    self.config.health.set_driver_ok(Some(false));
                    error!("driver not activated — run the Setup Assistant");
                    return MachineEvent::Fault(DegradedReason::DriverNotActivated);
                }
                DriverHealth::VhidDaemonDown => {
                    self.config.health.set_driver_ok(Some(false));
                    error!("Karabiner VirtualHIDDevice daemon not running");
                    return MachineEvent::Fault(DegradedReason::VhidDaemonDown);
                }
            }
        }

        // Log kanata's version and warn if below the known-good floor (§6.5).
        // Queried once (not on every restart/backoff retry).
        if self.config.health.snapshot().kanata_version.is_none() {
            if let Some(version) = child::query_version(&target.kanata_bin).await {
                self.config.health.set_kanata_version(Some(version.clone()));
                if let Some(parsed) = kanatabar_core::kanata::parse_version(&version) {
                    if parsed < crate::config::KANATA_VERSION_FLOOR {
                        warn!(%version, floor = %crate::config::KANATA_VERSION_FLOOR,
                            "kanata is older than the known-good floor");
                    }
                }
                info!(%version, "kanata version");
            }
        }

        // Re-vet IPC-applied config paths right before the spawn (§6.4, §14):
        // validation fstat'd the opened inode at apply time, but kanata re-opens
        // the path — re-checking here shrinks that TOCTOU window to the spawn
        // itself. Daemon-selected targets (vetted_uid None) are not re-vetted.
        if let Some(uid) = target.vetted_uid {
            if let Err(err) = crate::configmgr::vet_path(&target.kanata_cfg, uid) {
                error!(cfg = %target.kanata_cfg.display(), %err,
                    "active config failed path safety at spawn; refusing to spawn");
                return MachineEvent::Fault(DegradedReason::ConfigBroken);
            }
        }

        match child::preflight_config_check(&target, self.config.preflight_timeout).await {
            Preflight::Ok => {}
            Preflight::BinMissing => {
                error!(bin = %target.kanata_bin.display(),
                    "kanata binary not found — install kanata or fix kanata_bin");
                return MachineEvent::Fault(DegradedReason::KanataBinMissing);
            }
            Preflight::ConfigBroken { detail } => {
                error!(cfg = %target.kanata_cfg.display(), %detail,
                    "config rejected by `--check`; refusing to spawn");
                return MachineEvent::Fault(DegradedReason::ConfigBroken);
            }
        }

        match KanataChild::spawn(&target) {
            Ok(child) => {
                info!(pid = ?child.pid(), cfg = %target.kanata_cfg.display(), "kanata spawned");
                self.child = Some(child);
                MachineEvent::SpawnSucceeded
            }
            Err(SpawnError::BinMissing) => {
                error!(bin = %target.kanata_bin.display(), "kanata binary vanished at spawn");
                MachineEvent::Fault(DegradedReason::KanataBinMissing)
            }
            Err(SpawnError::Io(err)) => {
                error!(%err, "failed to spawn kanata");
                MachineEvent::SpawnFailed
            }
        }
    }

    fn publish_snapshot(&self) {
        let _ = self.snapshot.send(Snapshot {
            state: self.machine.state(),
            degraded_reason: self.machine.degraded_reason(),
            kanata_pid: self.child.as_ref().and_then(KanataChild::pid),
        });
    }

    fn persist(&self) {
        let Some(dir) = &self.config.state_dir else {
            return;
        };
        let active = self.active.snapshot();
        let state = PersistedState::now(
            self.machine.state(),
            self.machine.degraded_reason(),
            self.child.as_ref().and_then(KanataChild::pid),
            active.preset,
            active
                .last_known_good
                .map(|p| p.to_string_lossy().into_owned()),
        );
        if let Err(err) = statefile::persist(dir, &state) {
            warn!(%err, dir = %dir.display(), "failed to persist state.json");
        }
    }
}
