//! Control IPC server: a Unix-domain-socket, NDJSON, peer-authenticated
//! control channel (SPEC §7).
//!
//! The tray and CLI are thin clients of this protocol (SPEC §3). Every accepted
//! connection is peer-cred authorized (§7.1), must greet with `Hello`, and then
//! issues requests that are dispatched to the supervisor and answered on the
//! same connection. All decoded payloads are untrusted input: serde-validated,
//! size-bounded, never shell-interpolated (§14).

pub mod auth;
mod frame;

use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Instant;

use kanatabar_core::ipc::{
    negotiate, ErrorKind, Event, Request, RequestPayload, Response, ResponsePayload, Status,
    MAX_LINE_BYTES,
};
use kanatabar_core::machine::StateChanged;
use kanatabar_core::PROTOCOL_VERSION;
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use crate::configmgr::{ConfigError, ConfigManager};
use crate::device::DeviceRegistry;
use crate::events::DaemonEvents;
use crate::ffi::peercred::peer_uid;
use crate::health::driver::DriverProbe;
use crate::health::HealthState;
use crate::logbuf::LogBuffer;
use crate::supervisor::{Command, SupervisorClient};
use auth::AuthPolicy;

/// Where and how to expose the control socket (SPEC §3.2, §7.1).
#[derive(Debug, Clone)]
pub struct ControlConfig {
    /// Socket path; unlinked and recreated on start.
    pub socket_path: PathBuf,
    /// Group to own the socket (SPEC §7.1: `root:staff`); best-effort, needs
    /// privilege. `None` leaves the creating user's primary group (dev/tests).
    pub socket_gid: Option<u32>,
}

/// Bind the control socket and serve connections until the task is dropped.
///
/// `started` anchors the uptime reported in `Status`.
#[allow(clippy::too_many_arguments)] // the daemon's shared handles, wired once in main
pub async fn serve(
    config: ControlConfig,
    supervisor: SupervisorClient,
    configmgr: ConfigManager,
    health: HealthState,
    auth: AuthPolicy,
    started: Instant,
    driver_probe: Option<DriverProbe>,
    logs: LogBuffer,
    devices: DeviceRegistry,
    bus: DaemonEvents,
) -> io::Result<()> {
    let listener = bind(&config)?;
    info!(socket = %config.socket_path.display(), "control socket listening");

    loop {
        let stream = match listener.accept().await {
            Ok((stream, _addr)) => stream,
            Err(err) => {
                warn!(%err, "accept failed");
                continue;
            }
        };

        let uid = match peer_uid(stream.as_raw_fd()) {
            Ok(uid) => uid,
            Err(err) => {
                warn!(%err, "could not read peer credentials; dropping connection");
                continue;
            }
        };

        if !auth(uid) {
            // Reject and log; dropping the stream closes it (SPEC §7.1).
            warn!(uid, "rejecting unauthorized peer");
            continue;
        }

        let supervisor = supervisor.clone();
        let configmgr = configmgr.clone();
        let health = health.clone();
        let socket_path = config.socket_path.clone();
        let driver_probe = driver_probe.clone();
        let logs = logs.clone();
        let devices = devices.clone();
        let bus = bus.clone();
        tokio::spawn(async move {
            debug!(uid, "control connection accepted");
            let conn = Conn {
                supervisor,
                configmgr,
                health,
                peer_uid: uid,
                started,
                socket_path,
                driver_probe,
                logs,
                devices,
                bus,
            };
            if let Err(err) = handle_conn(stream, conn).await {
                debug!(%err, "control connection ended");
            }
        });
    }
}

/// Per-connection context passed to the request handlers.
struct Conn {
    supervisor: SupervisorClient,
    configmgr: ConfigManager,
    health: HealthState,
    /// Authenticated peer uid, used for config path-ownership checks (§6.4).
    peer_uid: u32,
    started: Instant,
    /// Control-socket path, for the `doctor` socket-permission check (§9).
    socket_path: PathBuf,
    /// Driver preflight probe, re-run by `doctor`; `None` when the check is
    /// disabled (dev/CI, SPEC §6.5).
    driver_probe: Option<DriverProbe>,
    /// Log ring + follower feed (`GetLogs`/`FollowLogs`, SPEC §6.6).
    logs: LogBuffer,
    /// Live device list (`GetDevices`, SPEC §7.2).
    devices: DeviceRegistry,
    /// DeviceChanged/ConfigApplied event bus (SPEC §7.2).
    bus: DaemonEvents,
}

/// Unlink any stale socket, bind, set mode 0660, and best-effort group-own it.
fn bind(config: &ControlConfig) -> io::Result<UnixListener> {
    if config.socket_path.exists() {
        // Recreated each start (SPEC §7.1); ignore a missing file.
        let _ = std::fs::remove_file(&config.socket_path);
    }
    if let Some(parent) = config.socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let listener = UnixListener::bind(&config.socket_path)?;
    std::fs::set_permissions(&config.socket_path, std::fs::Permissions::from_mode(0o660))?;

    if let Some(gid) = config.socket_gid {
        // Only root can chgrp to an arbitrary group; log and continue otherwise
        // (peer-cred auth, not ownership, is the security boundary — §7.1/§14).
        if let Err(err) = std::os::unix::fs::chown(&config.socket_path, None, Some(gid)) {
            warn!(%err, gid, "could not set socket group (needs root); continuing");
        }
    }

    Ok(listener)
}

/// Drive one authorized connection: greet, then loop over requests and, once
/// subscribed, pushed events.
async fn handle_conn(stream: UnixStream, conn: Conn) -> io::Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = frame::LineReader::new(read_half);

    // The first frame must be Hello (SPEC §7.1).
    let Some(line) = reader.next_line(MAX_LINE_BYTES).await? else {
        return Ok(()); // client hung up before greeting
    };
    let hello: Request = match serde_json::from_slice(&line) {
        Ok(req) => req,
        Err(err) => {
            send(&mut write_half, &malformed(None, &err)).await?;
            return Ok(());
        }
    };
    let negotiated = match hello.payload {
        RequestPayload::Hello { min_v, max_v } => negotiate(min_v, max_v),
        _ => {
            send(
                &mut write_half,
                &Response::error(hello.id, ErrorKind::InvalidRequest, "expected Hello first"),
            )
            .await?;
            return Ok(());
        }
    };
    let Some(version) = negotiated else {
        send(
            &mut write_half,
            &Response::error(
                hello.id,
                ErrorKind::Incompatible,
                format!("daemon speaks protocol v{PROTOCOL_VERSION}"),
            ),
        )
        .await?;
        return Ok(());
    };
    send(&mut write_half, &Response::hello_ack(version, hello.id)).await?;

    // Main loop: interleave client requests and, once subscribed, the pushed
    // streams (state events, layer changes, bus events, followed logs).
    let mut events: Option<broadcast::Receiver<StateChanged>> = None;
    let mut layers: Option<broadcast::Receiver<String>> = None;
    let mut bus: Option<broadcast::Receiver<Event>> = None;
    let mut follow: Option<broadcast::Receiver<String>> = None;
    loop {
        tokio::select! {
            biased;
            line = reader.next_line(MAX_LINE_BYTES) => {
                let Some(line) = line? else { return Ok(()); };
                match serde_json::from_slice::<Request>(&line) {
                    // GetLogs is the one request answered with multiple frames
                    // (N LogLine + a terminating Ack, SPEC §7.2) — handled
                    // here where the writer is available.
                    Ok(Request { id, payload: RequestPayload::GetLogs { lines } }) => {
                        for logline in conn.logs.last(lines as usize) {
                            send(&mut write_half, &Response {
                                v: PROTOCOL_VERSION,
                                id,
                                payload: ResponsePayload::LogLine { line: logline },
                            }).await?;
                        }
                        send(&mut write_half, &Response::ack(id)).await?;
                    }
                    // FollowLogs arms the log stream on this connection.
                    Ok(Request { id, payload: RequestPayload::FollowLogs }) => {
                        follow = Some(conn.logs.subscribe());
                        send(&mut write_half, &Response::ack(id)).await?;
                    }
                    Ok(req) => {
                        let response =
                            handle_request(req, &conn, &mut events, &mut layers, &mut bus).await;
                        send(&mut write_half, &response).await?;
                    }
                    Err(err) => send(&mut write_half, &malformed(None, &err)).await?,
                }
            }
            event = recv_broadcast(&mut events) => {
                match event {
                    EventStep::Deliver(change) => {
                        send(&mut write_half, &Response::event(state_event(change))).await?;
                    }
                    // Lagged: a slow client missed events; keep going (SPEC: best-effort push).
                    EventStep::Skip => {}
                    // The supervisor is gone; stop pushing but keep serving reads.
                    EventStep::Closed => events = None,
                }
            }
            layer = recv_broadcast(&mut layers) => {
                match layer {
                    EventStep::Deliver(layer) => {
                        send(&mut write_half, &Response::event(Event::LayerChanged { layer }))
                            .await?;
                    }
                    EventStep::Skip => {}
                    EventStep::Closed => layers = None,
                }
            }
            misc = recv_broadcast(&mut bus) => {
                match misc {
                    EventStep::Deliver(event) => {
                        send(&mut write_half, &Response::event(event)).await?;
                    }
                    EventStep::Skip => {}
                    EventStep::Closed => bus = None,
                }
            }
            logline = recv_broadcast(&mut follow) => {
                match logline {
                    EventStep::Deliver(line) => {
                        send(&mut write_half, &Response {
                            v: PROTOCOL_VERSION,
                            id: None,
                            payload: ResponsePayload::LogLine { line },
                        }).await?;
                    }
                    EventStep::Skip => {}
                    EventStep::Closed => follow = None,
                }
            }
        }
    }
}

/// Turn one request into its response, performing any side effect.
async fn handle_request(
    request: Request,
    conn: &Conn,
    events: &mut Option<broadcast::Receiver<StateChanged>>,
    layers: &mut Option<broadcast::Receiver<String>>,
    bus: &mut Option<broadcast::Receiver<Event>>,
) -> Response {
    let id = request.id;
    let supervisor = &conn.supervisor;
    match request.payload {
        RequestPayload::Hello { .. } => {
            Response::error(id, ErrorKind::InvalidRequest, "already greeted")
        }
        RequestPayload::GetStatus => Response::status(
            id,
            build_status(
                supervisor,
                &conn.health,
                conn.configmgr.active_preset(),
                conn.configmgr.active_is_passthrough(),
                conn.started,
            ),
        ),
        RequestPayload::Subscribe => {
            *events = Some(supervisor.subscribe());
            *layers = Some(conn.health.subscribe_layers());
            *bus = Some(conn.bus.subscribe());
            Response::ack(id)
        }
        RequestPayload::Start => command(supervisor, Command::Start, id).await,
        RequestPayload::Stop => command(supervisor, Command::Stop, id).await,
        RequestPayload::Restart => command(supervisor, Command::Restart, id).await,
        RequestPayload::Pause => command(supervisor, Command::Pause, id).await,
        RequestPayload::Resume => command(supervisor, Command::Resume, id).await,

        // ── Config & presets (SPEC §6.4, §7.2). ─────────────────────────────
        RequestPayload::ListPresets => Response {
            v: PROTOCOL_VERSION,
            id,
            payload: ResponsePayload::Presets {
                presets: conn.configmgr.list_presets().await,
            },
        },
        RequestPayload::ValidateConfig { path } => {
            match conn
                .configmgr
                .validate(std::path::Path::new(&path), conn.peer_uid, None)
                .await
            {
                Ok(_) => Response::ack(id),
                Err(err) => config_error(id, &err),
            }
        }
        RequestPayload::ApplyConfig { path } => {
            match conn
                .configmgr
                .apply_path(std::path::Path::new(&path), conn.peer_uid)
                .await
            {
                Ok(()) => Response::ack(id),
                Err(err) => config_error(id, &err),
            }
        }
        RequestPayload::SwitchPreset { name } => {
            match conn.configmgr.switch_preset(&name, conn.peer_uid).await {
                Ok(()) => Response::ack(id),
                Err(err) => config_error(id, &err),
            }
        }
        RequestPayload::SetPresetList { presets } => {
            match conn.configmgr.set_preset_list(presets.presets).await {
                Ok(()) => Response::ack(id),
                Err(err) => config_error(id, &err),
            }
        }
        RequestPayload::AddPreset {
            name,
            config,
            autostart,
        } => match conn.configmgr.add_preset(&name, &config, autostart).await {
            Ok(()) => Response::ack(id),
            Err(err) => config_error(id, &err),
        },
        RequestPayload::RemovePreset { name } => match conn.configmgr.remove_preset(&name).await {
            Ok(()) => Response::ack(id),
            Err(err) => config_error(id, &err),
        },
        RequestPayload::ReloadConfig => {
            use kanatabar_core::config::ConfigStatus;
            match conn.configmgr.reload().await {
                ConfigStatus::Invalid { error } => Response::error(
                    id,
                    ErrorKind::ConfigInvalid,
                    format!("config.toml is invalid: {error} (previous presets kept)"),
                ),
                _ => Response::ack(id),
            }
        }

        // ── Diagnostics (SPEC §9, §11). ─────────────────────────────────────
        RequestPayload::Doctor => {
            let checks = crate::doctor::run(
                &conn.supervisor,
                &conn.health,
                &conn.configmgr,
                &conn.socket_path,
                conn.driver_probe.as_ref(),
                conn.peer_uid,
                conn.started,
            )
            .await;
            Response {
                v: PROTOCOL_VERSION,
                id,
                payload: ResponsePayload::DoctorReport { checks },
            }
        }

        // ── Devices & autostart (SPEC §7.2). ────────────────────────────────
        RequestPayload::GetDevices => Response {
            v: PROTOCOL_VERSION,
            id,
            payload: ResponsePayload::Devices {
                devices: conn.devices.snapshot(),
            },
        },
        RequestPayload::SetAutostart { enabled } => {
            match conn.configmgr.set_autostart(enabled).await {
                Ok(()) => Response::ack(id),
                Err(err) => config_error(id, &err),
            }
        }

        // Multi-frame requests are answered at the connection level
        // (`handle_conn`); reaching here would be a wiring bug — refuse
        // loudly rather than hang the client.
        RequestPayload::GetLogs { .. } | RequestPayload::FollowLogs => Response::error(
            id,
            ErrorKind::Internal,
            "log requests are handled per-connection",
        ),
    }
}

/// Map a [`ConfigError`] onto a stable IPC error (SPEC §7.2).
fn config_error(id: Option<u64>, err: &ConfigError) -> Response {
    let kind = match err {
        ConfigError::PathRejected(_) => ErrorKind::PathRejected,
        ConfigError::ConfigInvalid(_) => ErrorKind::ConfigInvalid,
        ConfigError::UnknownPreset(_) => ErrorKind::InvalidRequest,
        ConfigError::Internal(_) => ErrorKind::Internal,
    };
    Response::error(id, kind, err.to_string())
}

/// Forward a lifecycle command to the supervisor and acknowledge it.
async fn command(supervisor: &SupervisorClient, cmd: Command, id: Option<u64>) -> Response {
    match supervisor.send(cmd).await {
        Ok(()) => Response::ack(id),
        Err(err) => Response::error(id, ErrorKind::Internal, err.to_string()),
    }
}

/// Build a `Status` from the supervisor snapshot, the shared health facts,
/// and the config manager's active preset (SPEC §7.2).
fn build_status(
    supervisor: &SupervisorClient,
    health: &HealthState,
    active_preset: Option<String>,
    passthrough: bool,
    started: Instant,
) -> Status {
    let snapshot = supervisor.snapshot();
    let health = health.snapshot();
    Status {
        state: snapshot.state,
        active_preset,
        active_layer: health.active_layer,
        kanata_pid: snapshot.kanata_pid,
        kanata_version: health.kanata_version,
        driver_ok: health.driver_ok,
        last_error: snapshot.degraded_reason.map(|r| r.describe().to_string()),
        degraded_reason: snapshot.degraded_reason,
        passthrough,
        uptime_s: started.elapsed().as_secs(),
        daemon_version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

fn state_event(change: StateChanged) -> Event {
    Event::StateChanged {
        from: change.from,
        to: change.to,
        reason: change.reason,
    }
}

fn malformed(id: Option<u64>, err: &serde_json::Error) -> Response {
    Response::error(
        id,
        ErrorKind::InvalidRequest,
        format!("malformed request: {err}"),
    )
}

/// One step of event delivery, keeping the `select!` arm total.
enum EventStep<T> {
    Deliver(T),
    Skip,
    Closed,
}

/// Await the next item on an optional broadcast, or pend forever when not
/// subscribed (keeps the `select!` arm total without spinning).
async fn recv_broadcast<T: Clone>(rx: &mut Option<broadcast::Receiver<T>>) -> EventStep<T> {
    match rx.as_mut() {
        Some(rx) => match rx.recv().await {
            Ok(item) => EventStep::Deliver(item),
            Err(broadcast::error::RecvError::Lagged(_)) => EventStep::Skip,
            Err(broadcast::error::RecvError::Closed) => EventStep::Closed,
        },
        None => std::future::pending().await,
    }
}

/// Serialize one response as an NDJSON line and flush it.
async fn send<W: AsyncWriteExt + Unpin>(writer: &mut W, response: &Response) -> io::Result<()> {
    // Never log LogLine sends: with a FollowLogs subscriber, logging a log
    // delivery would itself be buffered and delivered — an infinite feedback
    // loop under RUST_LOG=debug. Events are just noisy.
    if !matches!(
        response.payload,
        ResponsePayload::Event(_) | ResponsePayload::LogLine { .. }
    ) {
        debug!(?response.payload, "control response");
    }
    let mut buf = serde_json::to_vec(response)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    buf.push(b'\n');
    writer.write_all(&buf).await?;
    writer.flush().await
}
