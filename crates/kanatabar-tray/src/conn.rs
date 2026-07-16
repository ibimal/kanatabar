//! The tray's control-socket client loop (SPEC §8): connect, `Hello` +
//! `Subscribe`, render state, and reconnect with backoff when the daemon
//! bounces. Runs on a background tokio runtime; it never touches UI toolkit
//! types — it emits [`Update`]s that the GUI shell marshals to the main thread
//! (SPEC §3.1). That keeps this loop driveable headlessly by the Phase 7
//! `[AUTO]` integration test.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use kanatabar_core::backoff::BackoffConfig;
use kanatabar_core::ipc::{DoctorCheck, PresetInfo, RequestPayload, ResponsePayload, Status};
use kanatactl::Client;
use tokio::sync::mpsc::UnboundedSender;

use crate::model::MenuModel;
use crate::notify::notification_for;
use crate::reconnect::Reconnector;
use crate::session::Session;

/// What the connection loop hands to the UI thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    /// The menu should be rebuilt from this model.
    Model(MenuModel),
    /// A desktop notification should be shown (SPEC §8).
    Notify {
        /// Notification title.
        title: String,
        /// Notification body.
        body: String,
    },
}

/// Run the persistent event-stream connection forever: on each connect, seed
/// the [`Session`] with `GetStatus` + `ListPresets`, `Subscribe`, then fold
/// pushed events into the model. On any disconnect, push a disconnected model
/// and reconnect with exponential backoff (SPEC §8).
///
/// Never returns under normal operation; it only stops if `updates` is dropped
/// (the UI is gone).
pub async fn run_event_stream(
    socket: PathBuf,
    updates: UnboundedSender<Update>,
    backoff: BackoffConfig,
) {
    let mut reconnector = Reconnector::new(backoff);
    loop {
        match Client::connect(&socket).await {
            Ok(client) => {
                // A successful connect resets the backoff so the next drop
                // reconnects promptly (SPEC §8).
                reconnector.reset();
                if let Err(err) = stream_connection(client, &updates).await {
                    tracing::debug!(%err, "control connection ended; will reconnect");
                }
            }
            Err(err) => {
                tracing::debug!(%err, "control connect failed; will retry");
            }
        }

        // The daemon is (now) unreachable — reflect that, then wait.
        if updates
            .send(Update::Model(MenuModel::disconnected()))
            .is_err()
        {
            return; // UI gone.
        }
        tokio::time::sleep(reconnector.next_delay()).await;
    }
}

/// Drive one live connection: seed, subscribe, stream. Returns `Ok(())` on a
/// clean close and `Err` on a protocol/IO fault — both lead to a reconnect.
async fn stream_connection(mut client: Client, updates: &UnboundedSender<Update>) -> Result<()> {
    // Seed the current snapshot before subscribing, so the very first render is
    // the real state and later events are deltas on top of it.
    let mut session = Session::new();
    session.set_status(fetch_status(&mut client).await?);
    session.set_presets(fetch_presets(&mut client).await?);
    send_model(&session, updates)?;

    // Begin the event stream.
    match client.request(RequestPayload::Subscribe).await?.payload {
        ResponsePayload::Ack => {}
        ResponsePayload::Error { message, .. } => bail!("subscribe rejected: {message}"),
        other => bail!("unexpected reply to Subscribe: {other:?}"),
    }

    loop {
        let event = client.next_event().await?;
        if let Some((title, body)) = notification_for(&event) {
            // A closed UI channel means we're shutting down.
            updates.send(Update::Notify { title, body })?;
        }
        session.apply_event(&event);
        send_model(&session, updates)?;
    }
}

fn send_model(session: &Session, updates: &UnboundedSender<Update>) -> Result<()> {
    updates.send(Update::Model(session.menu_model()))?;
    Ok(())
}

async fn fetch_status(client: &mut Client) -> Result<Status> {
    match client.request(RequestPayload::GetStatus).await?.payload {
        ResponsePayload::Status(status) => Ok(status),
        ResponsePayload::Error { message, .. } => bail!("GetStatus failed: {message}"),
        other => bail!("unexpected reply to GetStatus: {other:?}"),
    }
}

async fn fetch_presets(client: &mut Client) -> Result<Vec<PresetInfo>> {
    match client.request(RequestPayload::ListPresets).await?.payload {
        ResponsePayload::Presets { presets } => Ok(presets),
        ResponsePayload::Error { message, .. } => bail!("ListPresets failed: {message}"),
        other => bail!("unexpected reply to ListPresets: {other:?}"),
    }
}

/// Issue a single control command on a fresh, short-lived connection (SPEC §8:
/// menu clicks). Menu clicks are infrequent, so a dedicated connection avoids
/// interleaving request/response with the event stream. Returns an actionable
/// message on rejection.
pub async fn send_command(socket: &Path, payload: RequestPayload) -> Result<()> {
    let mut client = Client::connect(socket).await?;
    match client.request(payload).await?.payload {
        ResponsePayload::Ack => Ok(()),
        ResponsePayload::Error { message, .. } => bail!("{message}"),
        other => bail!("unexpected reply: {other:?}"),
    }
}

/// Fetch a one-shot `Status` over a fresh connection. The Setup Wizard pairs
/// this with `fetch_doctor`: the supervisor's structured `degraded_reason`
/// catches runtime-only failures (a TCC denial) that the static checks
/// cannot see (SPEC §11; HW Run 9 finding).
pub async fn fetch_status_once(socket: &Path) -> Result<Status> {
    let mut client = Client::connect(socket).await?;
    fetch_status(&mut client).await
}

/// Fetch the configured presets over a fresh connection, for the wizard's
/// completion step (offer to import an existing config when none are set).
pub async fn fetch_presets_once(socket: &Path) -> Result<Vec<PresetInfo>> {
    let mut client = Client::connect(socket).await?;
    fetch_presets(&mut client).await
}

/// Run `doctor` over a fresh connection and return its checks (SPEC §9), for
/// the tray's "Run Doctor" / Setup Wizard actions (SPEC §11).
pub async fn fetch_doctor(socket: &Path) -> Result<Vec<DoctorCheck>> {
    let mut client = Client::connect(socket).await?;
    match client.request(RequestPayload::Doctor).await?.payload {
        ResponsePayload::DoctorReport { checks } => Ok(checks),
        ResponsePayload::Error { message, .. } => bail!("{message}"),
        other => bail!("unexpected reply: {other:?}"),
    }
}

/// Fetch the daemon's device list over a fresh connection (SPEC §8 "Devices").
pub async fn fetch_devices(socket: &Path) -> Result<Vec<kanatabar_core::ipc::DeviceInfo>> {
    let mut client = Client::connect(socket).await?;
    match client.request(RequestPayload::GetDevices).await?.payload {
        ResponsePayload::Devices { devices } => Ok(devices),
        ResponsePayload::Error { message, .. } => bail!("{message}"),
        other => bail!("unexpected reply: {other:?}"),
    }
}
