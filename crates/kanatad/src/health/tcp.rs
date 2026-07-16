//! kanata TCP layer relay (SPEC §6.5): connect to `kanata --port <p>`, read
//! NDJSON layer events, and reflect the active layer in [`HealthState`].
//!
//! TCP loss alone is not a supervision signal (SPEC §6.5): the relay just
//! reconnects while kanata is `Running`. The supervisor owns restart decisions.

use std::net::SocketAddr;
use std::time::Duration;

use kanatabar_core::kanata::parse_layer_change;
use kanatabar_core::state::SupervisorState;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use crate::health::HealthState;
use crate::supervisor::SupervisorClient;
use kanatabar_core::machine::StateChanged;

/// Default connect timeout and reconnect delay.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const RECONNECT_DELAY: Duration = Duration::from_millis(500);

/// Relay kanata layer events into `health` for as long as kanata runs. Connects
/// while `Running`, reconnecting on loss; clears the active layer otherwise.
/// Returns when the supervisor's event stream closes (daemon shutdown).
pub async fn run(port: u16, client: SupervisorClient, health: HealthState) {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let mut states = client.subscribe();
    info!(%addr, "kanata TCP layer relay started");

    loop {
        if client.snapshot().state == SupervisorState::Running {
            // Relay until the connection drops OR kanata leaves Running —
            // without the state arm, a still-open connection would keep
            // reporting a dead kanata's (or a port squatter's) layer forever
            // (HW finding 2026-07-11: `layer: base` shown while Degraded).
            tokio::select! {
                result = connect_and_relay(addr, &health) => {
                    if let Err(err) = result {
                        debug!(%err, "kanata TCP relay disconnected");
                    }
                }
                changed = wait_not_running(&mut states, &client) => {
                    debug!("kanata left Running; dropping the TCP relay connection");
                    if !changed { health.set_active_layer(None); break; }
                }
            }
            health.set_active_layer(None);

            // Brief pause before reconnecting, unless the state changes first.
            if client.snapshot().state == SupervisorState::Running {
                tokio::select! {
                    _ = tokio::time::sleep(RECONNECT_DELAY) => {}
                    changed = wait_change(&mut states) => {
                        if !changed { break; }
                    }
                }
            }
        } else {
            health.set_active_layer(None);
            if !wait_change(&mut states).await {
                break;
            }
        }
    }

    info!("kanata TCP layer relay stopped");
}

/// Connect and relay until EOF or error.
async fn connect_and_relay(addr: SocketAddr, health: &HealthState) -> std::io::Result<()> {
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
        .await
        .map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::TimedOut, "kanata TCP connect timed out")
        })??;
    debug!(%addr, "connected to kanata TCP");
    read_layers(stream, health).await
}

/// Read NDJSON layer events, updating `health` on each `LayerChange`.
pub async fn read_layers<S>(stream: S, health: &HealthState) -> std::io::Result<()>
where
    S: tokio::io::AsyncRead + Unpin,
{
    let mut lines = BufReader::new(stream).lines();
    while let Some(line) = lines.next_line().await? {
        if let Some(layer) = parse_layer_change(&line) {
            debug!(%layer, "kanata layer change");
            health.set_active_layer(Some(layer));
        }
    }
    Ok(())
}

/// Await the next supervisor transition; `false` when the stream closes.
async fn wait_change(states: &mut broadcast::Receiver<StateChanged>) -> bool {
    loop {
        match states.recv().await {
            Ok(_) => return true,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => {
                warn!("supervisor event stream closed; stopping relay");
                return false;
            }
        }
    }
}

/// Await until the supervisor is no longer `Running` (re-checking the
/// snapshot on every transition so a lagged receiver can't miss it);
/// `false` when the stream closes.
async fn wait_not_running(
    states: &mut broadcast::Receiver<StateChanged>,
    client: &SupervisorClient,
) -> bool {
    loop {
        match states.recv().await {
            Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {
                if client.snapshot().state != SupervisorState::Running {
                    return true;
                }
            }
            Err(broadcast::error::RecvError::Closed) => {
                warn!("supervisor event stream closed; stopping relay");
                return false;
            }
        }
    }
}
