//! kanata child process management: preflight, spawn, exit classification,
//! graceful termination (SPEC §6.1).

use std::io;
use std::os::unix::process::ExitStatusExt;
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use kanatabar_core::kanata::{BackendEvent, StderrFault};
use kanatabar_core::state::ExitClass;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::config::SpawnTarget;

/// Result of the pre-spawn `kanata --cfg <path> --check` (SPEC §6.1c, §6.4).
#[derive(Debug)]
pub enum Preflight {
    /// Config accepted; safe to spawn.
    Ok,
    /// Config rejected; `detail` carries kanata's parse errors (never `.kbd`
    /// contents — kanata reports locations, not the file body).
    ConfigBroken {
        /// Trimmed stderr from the failed check.
        detail: String,
    },
    /// The kanata binary itself is missing.
    BinMissing,
}

/// Run the config check that must pass before every spawn [HARD].
pub async fn preflight_config_check(target: &SpawnTarget, timeout: Duration) -> Preflight {
    let mut cmd = Command::new(&target.kanata_bin);
    cmd.arg("--cfg")
        .arg(&target.kanata_cfg)
        .arg("--check")
        .args(&target.extra_args)
        .stdin(Stdio::null());

    let output = match tokio::time::timeout(timeout, cmd.output()).await {
        Err(_elapsed) => {
            return Preflight::ConfigBroken {
                detail: format!("config check did not finish within {timeout:?}"),
            }
        }
        Ok(Err(err)) if err.kind() == io::ErrorKind::NotFound => return Preflight::BinMissing,
        Ok(Err(err)) => {
            return Preflight::ConfigBroken {
                detail: format!("failed to run config check: {err}"),
            }
        }
        Ok(Ok(output)) => output,
    };

    if output.status.success() {
        Preflight::Ok
    } else {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Preflight::ConfigBroken { detail }
    }
}

/// Why a spawn attempt failed.
#[derive(Debug)]
pub enum SpawnError {
    /// Binary not found — immediate `Degraded`, no backoff spin (SPEC §16).
    BinMissing,
    /// Any other I/O failure — counts against the retry budget.
    Io(io::Error),
}

/// Fault-flag encoding for the lock-free channel between the output-forwarding
/// tasks and the supervisor (0 = none; first classified fault wins).
const FAULT_NONE: u8 = 0;
const FAULT_PERMISSION: u8 = 1;
const FAULT_DEVICE_IN_USE: u8 = 2;
const FAULT_PORT_IN_USE: u8 = 3;

/// Something the supervisor must react to on a live child: its exit, or a
/// live output-backend transition in its log stream (SPEC §6.5; HW
/// 2026-07-11: a driver version mismatch leaves kanata alive but unremapping).
#[derive(Debug)]
pub enum ChildWake {
    /// The child exited.
    Exited(io::Result<ExitStatus>),
    /// The child reported its output backend down/up while still running.
    Backend(BackendEvent),
}

/// A supervised kanata child.
#[derive(Debug)]
pub struct KanataChild {
    inner: Child,
    /// First give-up-worthy fault seen in the child's output (SPEC §2, §6.5),
    /// written by the forwarding tasks, read after exit.
    fault: Arc<AtomicU8>,
    /// Live backend transitions from the forwarding tasks (senders drop at
    /// pipe EOF, closing the channel).
    backend_rx: mpsc::UnboundedReceiver<BackendEvent>,
    /// The output-forwarding tasks; awaited after exit so a fault printed
    /// just before death is not missed (pipe EOF ends them).
    output_tasks: Vec<JoinHandle<()>>,
}

impl KanataChild {
    /// Spawn `kanata --cfg <cfg>` + extra args, piping its output into our
    /// structured log with `source=kanata` (SPEC §6.1).
    pub fn spawn(target: &SpawnTarget) -> Result<Self, SpawnError> {
        let mut cmd = Command::new(&target.kanata_bin);
        cmd.arg("--cfg").arg(&target.kanata_cfg);
        if let Some(port) = target.tcp_port {
            // Enables kanata's NDJSON server for the layer relay (SPEC §6.5).
            cmd.arg("--port").arg(port.to_string());
        }
        cmd.args(&target.extra_args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Safety net: if the Child handle is ever dropped while the
            // process lives (a panicking test tearing down its runtime — the
            // source of orphaned mock-kanata processes squatting on the TCP
            // port, seen on HW 2026-07-11), SIGKILL it. Production paths
            // always terminate() gracefully first, so this changes nothing
            // there.
            .kill_on_drop(true);

        let mut child = cmd.spawn().map_err(|err| {
            if err.kind() == io::ErrorKind::NotFound {
                SpawnError::BinMissing
            } else {
                SpawnError::Io(err)
            }
        })?;

        let fault = Arc::new(AtomicU8::new(FAULT_NONE));
        let (backend_tx, backend_rx) = mpsc::unbounded_channel();
        let mut output_tasks = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            output_tasks.push(forward_output(
                stdout,
                "stdout",
                Arc::clone(&fault),
                backend_tx.clone(),
            ));
        }
        if let Some(stderr) = child.stderr.take() {
            output_tasks.push(forward_output(
                stderr,
                "stderr",
                Arc::clone(&fault),
                backend_tx,
            ));
        }

        Ok(Self {
            inner: child,
            fault,
            backend_rx,
            output_tasks,
        })
    }

    /// OS pid, while the child has not been reaped.
    pub fn pid(&self) -> Option<u32> {
        self.inner.id()
    }

    /// Wait for the child to exit. Cancel-safe (tokio guarantee), so the
    /// supervisor can select over it.
    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        self.inner.wait().await
    }

    /// Wait for the next thing the supervisor must react to: exit, or a live
    /// backend transition (SPEC §6.5). Cancel-safe: both `Child::wait` and
    /// `mpsc::Receiver::recv` are, so the supervisor can select over this.
    /// A closed backend channel (pipe EOF) just leaves the exit arm.
    pub async fn next_wake(&mut self) -> ChildWake {
        tokio::select! {
            status = self.inner.wait() => ChildWake::Exited(status),
            Some(event) = self.backend_rx.recv() => ChildWake::Backend(event),
        }
    }

    /// The first give-up-worthy fault seen in the child's output, if any.
    /// Call [`Self::drain_output`] first after an exit, or a fault printed in
    /// the child's final moments may not have been read yet.
    pub fn fault(&self) -> Option<StderrFault> {
        match self.fault.load(Ordering::Relaxed) {
            FAULT_PERMISSION => Some(StderrFault::PermissionDenied),
            FAULT_DEVICE_IN_USE => Some(StderrFault::DeviceInUse),
            FAULT_PORT_IN_USE => Some(StderrFault::PortInUse),
            _ => None,
        }
    }

    /// Await the output-forwarding tasks (they end at pipe EOF, i.e. child
    /// exit), bounded by `timeout` so a wedged pipe can never stall the
    /// supervisor loop.
    pub async fn drain_output(&mut self, timeout: Duration) {
        for task in self.output_tasks.drain(..) {
            if tokio::time::timeout(timeout, task).await.is_err() {
                debug!("output forwarder still running after exit; not waiting");
            }
        }
    }

    /// Requested termination [HARD]: SIGTERM, wait ≤ grace, then SIGKILL
    /// (SPEC §6.1). Consumes the child; the exit is `Requested` by definition.
    pub async fn terminate(mut self, grace: Duration) {
        if let Some(pid) = self.inner.id() {
            let pid = nix::unistd::Pid::from_raw(pid as i32);
            if let Err(err) = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM) {
                debug!(%err, "SIGTERM failed (child likely already gone)");
            }
        }
        match tokio::time::timeout(grace, self.inner.wait()).await {
            Ok(Ok(status)) => info!(?status, "child exited (requested)"),
            Ok(Err(err)) => warn!(%err, "waiting for terminated child failed"),
            Err(_elapsed) => {
                warn!(
                    grace_ms = grace.as_millis() as u64,
                    "child ignored SIGTERM; killing"
                );
                if let Err(err) = self.inner.start_kill() {
                    warn!(%err, "SIGKILL failed");
                }
                if let Ok(status) = self.inner.wait().await {
                    info!(?status, "child exited (killed)");
                }
            }
        }
    }
}

/// Classify an exit we did **not** request (SPEC §6.1, §2 panic escape).
///
/// Heuristic pending [VERIFY] against the installed kanata: a clean exit
/// (code 0) that we didn't ask for is treated as the user's panic escape —
/// an intentional stop, never a crash-loop trigger. Everything else is a crash.
pub fn classify_unrequested_exit(result: io::Result<ExitStatus>) -> ExitClass {
    match result {
        Ok(status) if status.success() => ExitClass::PanicEscape,
        Ok(status) => ExitClass::Crash {
            code: status.code(),
            signal: status.signal(),
        },
        Err(_) => ExitClass::Crash {
            code: None,
            signal: None,
        },
    }
}

/// Run `<bin> --version` and return kanata's reported version string, or `None`
/// if it could not be determined (SPEC §6.5).
pub async fn query_version(bin: &std::path::Path) -> Option<String> {
    let output = Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .await
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    kanatabar_core::kanata::parse_version(&text).map(|v| v.to_string())
}

/// Forward one child output stream into the structured log, line by line,
/// recording the first give-up-worthy fault (SPEC §2: TCC denial, device held
/// elsewhere) and relaying live backend transitions (SPEC §6.5). State and
/// errors only — kanata does not print keystrokes, and we never log `.kbd`
/// contents (SPEC §6.6 [HARD]).
fn forward_output(
    stream: impl AsyncRead + Unpin + Send + 'static,
    which: &'static str,
    fault: Arc<AtomicU8>,
    backend_tx: mpsc::UnboundedSender<BackendEvent>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stream).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            relay_line(&line, which, &fault, &backend_tx);
        }
    })
}

/// Classify and log one child output line. Classification (faults, backend
/// health) runs on **every** line; only the log level varies: kanata's raw
/// VHID-status heartbeat (`virtual_hid_keyboard_ready …`, ~1/s forever — HW
/// 2026-07-13: 91% of `kanatad.err.log`) relays at DEBUG so it can't drown
/// the INFO log or the `GetLogs` ring (SPEC §6.6), everything else at INFO.
fn relay_line(
    line: &str,
    which: &'static str,
    fault: &AtomicU8,
    backend_tx: &mpsc::UnboundedSender<BackendEvent>,
) {
    if let Some(found) = kanatabar_core::kanata::classify_fault_line(line) {
        let code = match found {
            StderrFault::PermissionDenied => FAULT_PERMISSION,
            StderrFault::DeviceInUse => FAULT_DEVICE_IN_USE,
            StderrFault::PortInUse => FAULT_PORT_IN_USE,
        };
        // First fault wins; later lines don't overwrite it.
        let _ = fault.compare_exchange(FAULT_NONE, code, Ordering::Relaxed, Ordering::Relaxed);
    }
    if let Some(event) = kanatabar_core::kanata::classify_backend_line(line) {
        // Receiver gone (child handle dropped) is fine — best effort.
        let _ = backend_tx.send(event);
    }
    if kanatabar_core::kanata::is_vhid_status_noise(line) {
        debug!(source = "kanata", stream = which, "{line}");
    } else {
        info!(source = "kanata", stream = which, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logbuf::LogBuffer;
    use tracing_subscriber::prelude::*;

    /// Relay a batch of lines under the production default filter (`info`,
    /// main.rs) into a fresh ring, returning the ring contents plus the
    /// fault/backend side-channels for inspection.
    fn relay_at_info(
        lines: &[&str],
    ) -> (
        Vec<String>,
        Arc<AtomicU8>,
        mpsc::UnboundedReceiver<BackendEvent>,
    ) {
        let buffer = LogBuffer::new(64);
        let fault = Arc::new(AtomicU8::new(FAULT_NONE));
        let (backend_tx, backend_rx) = mpsc::unbounded_channel();
        let subscriber = tracing_subscriber::registry()
            .with(tracing_subscriber::EnvFilter::new("info"))
            .with(buffer.layer());
        tracing::subscriber::with_default(subscriber, || {
            for line in lines {
                relay_line(line, "stdout", &fault, &backend_tx);
            }
        });
        (buffer.last(64), fault, backend_rx)
    }

    #[test]
    fn vhid_heartbeat_is_not_relayed_at_info() {
        let (lines, _, _) = relay_at_info(&[
            "virtual_hid_keyboard_ready true",
            "connected",
            "driver activated: true",
        ]);
        assert!(
            lines.is_empty(),
            "heartbeat leaked into INFO ring: {lines:?}"
        );
    }

    #[test]
    fn real_lines_still_relay_at_info() {
        let (lines, _, mut backend_rx) = relay_at_info(&[
            "virtual_hid_keyboard_ready true",
            "17:05:04.2338 [INFO] Starting kanata proper",
            "output backend unavailable — releasing input devices",
            "output backend and console session ready — re-grabbing input devices",
        ]);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[0].contains("Starting kanata proper"), "{lines:?}");
        assert!(lines[1].contains("output backend unavailable"), "{lines:?}");
        assert!(lines[2].contains("console session ready"), "{lines:?}");
        // Backend classification fired despite the heartbeat interleaved.
        assert_eq!(backend_rx.try_recv(), Ok(BackendEvent::Down));
        assert_eq!(backend_rx.try_recv(), Ok(BackendEvent::Up));
    }

    #[test]
    fn classification_runs_on_every_line_regardless_of_level() {
        // A fault line must set the flag even when surrounded by heartbeat.
        let (_, fault, _) = relay_at_info(&[
            "virtual_hid_keyboard_ready true",
            "IOHIDDeviceOpen error: (iokit/common) privilege violation",
            "virtual_hid_keyboard_ready true",
        ]);
        assert_eq!(fault.load(Ordering::Relaxed), FAULT_PERMISSION);
    }
}
