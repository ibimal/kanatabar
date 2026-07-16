//! kanatactl client library (SPEC §7, §9).
//!
//! A thin async client over the control socket: connect, negotiate `Hello`,
//! send requests, and correlate responses by id. Shared by the `kanatactl`
//! binary and the integration tests so both exercise the same protocol code.
//! `install` (SPEC §9, §10) is a separate concern: it never talks to the
//! socket, since a fresh `install` runs before the daemon exists at all.

pub mod install;

use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use kanatabar_core::ipc::{
    Event, Request, RequestPayload, Response, ResponsePayload, MAX_LINE_BYTES,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;

/// Default control-socket path (SPEC §3.2).
pub const DEFAULT_SOCKET: &str = "/var/run/kanatabar.sock";

/// A connected, greeted control-socket client.
pub struct Client {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    next_id: u64,
}

impl Client {
    /// Connect to the socket and complete the `Hello`/`HelloAck` handshake.
    pub async fn connect(path: &Path) -> Result<Self> {
        let stream = UnixStream::connect(path)
            .await
            .with_context(|| format!("connecting to {}", path.display()))?;
        let (read_half, write_half) = stream.into_split();
        let mut client = Self {
            reader: BufReader::new(read_half),
            writer: write_half,
            next_id: 1,
        };
        client.hello().await?;
        Ok(client)
    }

    async fn hello(&mut self) -> Result<()> {
        let response = self
            .request(RequestPayload::Hello { min_v: 1, max_v: 1 })
            .await?;
        match response.payload {
            ResponsePayload::HelloAck => Ok(()),
            ResponsePayload::Error { message, .. } => {
                bail!("daemon rejected handshake: {message}")
            }
            other => bail!("unexpected handshake reply: {other:?}"),
        }
    }

    /// Send a request and return the matching (non-event) response.
    pub async fn request(&mut self, payload: RequestPayload) -> Result<Response> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&Request::new(Some(id), payload)).await?;

        loop {
            let response = self.read_response().await?;
            // Skip unsolicited events that may arrive after Subscribe.
            if matches!(response.payload, ResponsePayload::Event(_)) {
                continue;
            }
            return Ok(response);
        }
    }

    /// Block until the next pushed event arrives (after `Subscribe`).
    pub async fn next_event(&mut self) -> Result<Event> {
        loop {
            let response = self.read_response().await?;
            if let ResponsePayload::Event(event) = response.payload {
                return Ok(event);
            }
        }
    }

    /// Send a request without waiting for a reply, returning its correlation
    /// id — for the multi-frame exchanges (`GetLogs` answers with N `LogLine`
    /// frames then an `Ack`; `FollowLogs` streams until the connection
    /// closes). Read the frames with [`Self::next_response`].
    pub async fn send_request(&mut self, payload: RequestPayload) -> Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&Request::new(Some(id), payload)).await?;
        Ok(id)
    }

    /// The next raw response frame, whatever its payload.
    pub async fn next_response(&mut self) -> Result<Response> {
        self.read_response().await
    }

    async fn send(&mut self, request: &Request) -> Result<()> {
        let mut buf = serde_json::to_vec(request)?;
        buf.push(b'\n');
        self.writer.write_all(&buf).await?;
        self.writer.flush().await?;
        Ok(())
    }

    async fn read_response(&mut self) -> Result<Response> {
        let mut line = Vec::new();
        let n = self.reader.read_until(b'\n', &mut line).await?;
        if n == 0 {
            return Err(anyhow!("daemon closed the connection"));
        }
        // The daemon frames at the same cap; a longer line is a protocol fault.
        if line.len() > MAX_LINE_BYTES + 1 {
            bail!("daemon sent an oversize line");
        }
        serde_json::from_slice(&line).context("decoding daemon response")
    }
}
