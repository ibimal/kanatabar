//! Control-IPC protocol types (SPEC §7).
//!
//! The wire format is NDJSON: one JSON object per line. Requests carry an
//! optional correlation `id`; every response and event carries the protocol
//! version `v` (SPEC §7.1) and echoes the request `id` (`null` for unsolicited
//! events). The `type` tag and its variant fields are flattened next to
//! `v`/`id`, matching the §7.2 example:
//!
//! ```json
//! {"v":1,"id":null,"type":"Status","state":"Running","kanata_pid":4123, ...}
//! ```
//!
//! Every type here round-trips through serde (exhaustively tested); the daemon
//! owns the NDJSON framing and treats all decoded payloads as untrusted input.

use serde::{Deserialize, Serialize};

use crate::config::PresetList;
use crate::state::{DegradedReason, SupervisorState};
use crate::PROTOCOL_VERSION;

/// Maximum length of one NDJSON line; longer lines close the connection
/// (SPEC §7.1).
pub const MAX_LINE_BYTES: usize = 64 * 1024;

/// A client → daemon request frame: optional correlation id plus payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// Correlation id echoed on the matching response; `None` is allowed.
    pub id: Option<u64>,
    /// The request itself.
    #[serde(flatten)]
    pub payload: RequestPayload,
}

impl Request {
    /// Build a request with the given id and payload.
    pub fn new(id: Option<u64>, payload: RequestPayload) -> Self {
        Self { id, payload }
    }
}

/// The set of requests (SPEC §7.2). `Hello` must be sent first on a connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RequestPayload {
    /// Version negotiation; must be the first frame.
    Hello {
        /// Lowest protocol version the client speaks.
        min_v: u32,
        /// Highest protocol version the client speaks.
        max_v: u32,
    },
    /// Request a one-shot [`Status`].
    GetStatus,
    /// Stream [`Event`]s on this connection until it closes.
    Subscribe,
    /// Start kanata.
    Start,
    /// Stop kanata; the daemon keeps running.
    Stop,
    /// Restart kanata.
    Restart,
    /// Pause remapping.
    Pause,
    /// Resume from pause.
    Resume,
    /// Switch to a named preset.
    SwitchPreset {
        /// Preset name.
        name: String,
    },
    /// List configured presets.
    ListPresets,
    /// Validate a `.kbd` file without applying it.
    ValidateConfig {
        /// Path to the `.kbd` file.
        path: String,
    },
    /// Validate then apply a `.kbd` file.
    ApplyConfig {
        /// Path to the `.kbd` file.
        path: String,
    },
    /// Replace the supervisor's preset list.
    SetPresetList {
        /// The new preset set.
        presets: PresetList,
    },
    /// Add or update a single preset (`kanatactl preset add`), preserving the
    /// other presets and their advanced fields. Upsert semantics.
    AddPreset {
        /// Preset name.
        name: String,
        /// Path to the preset's `.kbd` file.
        config: String,
        /// Start this preset automatically at daemon boot.
        autostart: bool,
    },
    /// Remove a single preset by name (`kanatactl preset remove`).
    RemovePreset {
        /// Preset name.
        name: String,
    },
    /// Re-read `config.toml` from disk (`kanatactl config reload`) so hand
    /// edits take effect without restarting the daemon.
    ReloadConfig,
    /// Fetch the last `lines` buffered log lines.
    GetLogs {
        /// Number of lines to return.
        lines: u32,
    },
    /// Stream log lines as they are emitted.
    FollowLogs,
    /// List input devices the daemon can see.
    GetDevices,
    /// Enable or disable autostart of the active preset.
    SetAutostart {
        /// Desired autostart state.
        enabled: bool,
    },
    /// Run the full preflight checklist.
    Doctor,
}

/// A daemon → client response frame. Carries the protocol version and echoes
/// the request id (SPEC §7.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    /// Protocol version (SPEC §7.1: every response/event carries `v`).
    pub v: u32,
    /// Echo of the request id; `None` for unsolicited events.
    pub id: Option<u64>,
    /// The response itself.
    #[serde(flatten)]
    pub payload: ResponsePayload,
}

impl Response {
    /// Successful version negotiation, tagged with the negotiated version.
    pub fn hello_ack(negotiated_v: u32, id: Option<u64>) -> Self {
        Self {
            v: negotiated_v,
            id,
            payload: ResponsePayload::HelloAck,
        }
    }

    /// Generic acknowledgement of a command (current protocol version).
    pub fn ack(id: Option<u64>) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id,
            payload: ResponsePayload::Ack,
        }
    }

    /// An error response with a stable [`ErrorKind`] and actionable message.
    pub fn error(id: Option<u64>, kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id,
            payload: ResponsePayload::Error {
                kind,
                message: message.into(),
            },
        }
    }

    /// A status response.
    pub fn status(id: Option<u64>, status: Status) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id,
            payload: ResponsePayload::Status(status),
        }
    }

    /// An unsolicited event (id is always `null`).
    pub fn event(event: Event) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id: None,
            payload: ResponsePayload::Event(event),
        }
    }
}

/// The set of responses and events (SPEC §7.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ResponsePayload {
    /// Version negotiation succeeded.
    HelloAck,
    /// A command was accepted.
    Ack,
    /// A request failed.
    Error {
        /// Stable, machine-usable category.
        kind: ErrorKind,
        /// Human-readable, actionable message.
        message: String,
    },
    /// Current daemon status.
    Status(Status),
    /// An asynchronous event (only after `Subscribe`).
    Event(Event),
    /// Result of `Doctor`.
    DoctorReport {
        /// One entry per preflight check.
        checks: Vec<DoctorCheck>,
    },
    /// One buffered or streamed log line.
    LogLine {
        /// The formatted log line (never keystrokes or `.kbd` contents).
        line: String,
    },
    /// Result of `ListPresets`.
    Presets {
        /// Known presets.
        presets: Vec<PresetInfo>,
    },
    /// Result of `GetDevices`.
    Devices {
        /// Visible input devices.
        devices: Vec<DeviceInfo>,
    },
}

/// Stable, machine-usable error categories (SPEC §7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorKind {
    /// Protocol version negotiation failed.
    Incompatible,
    /// Request failed validation (unknown type, bad fields, oversize, wrong order).
    InvalidRequest,
    /// A user-supplied path failed the §6.4 safety checks.
    PathRejected,
    /// `kanata --check` rejected the config.
    ConfigInvalid,
    /// Karabiner DriverKit extension is not `activated enabled`.
    DriverNotActivated,
    /// Karabiner VirtualHIDDevice daemon is not running.
    VhidDaemonDown,
    /// Operation requires a stopped child, but one is running.
    AlreadyRunning,
    /// Operation requires a running child, but none is.
    NotRunning,
    /// Unexpected daemon-side failure.
    Internal,
}

/// A point-in-time status snapshot (SPEC §7.2 `Status`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    /// Supervisor state.
    pub state: SupervisorState,
    /// Active preset name, when one is selected.
    pub active_preset: Option<String>,
    /// Active kanata layer, when known (via kanata TCP; Phase 5).
    pub active_layer: Option<String>,
    /// Live kanata child pid.
    pub kanata_pid: Option<u32>,
    /// Reported kanata version (Phase 5).
    pub kanata_version: Option<String>,
    /// Driver preflight result; `None` when not yet checked (Phase 5).
    pub driver_ok: Option<bool>,
    /// Last error message, e.g. why we are `Degraded`.
    pub last_error: Option<String>,
    /// Structured reason when `state` is `Degraded` (v1-additive, 2026-07:
    /// `last_error` is prose for humans; the wizard needs the enum to map a
    /// runtime degradation — e.g. a TCC denial the doctor checks cannot see
    /// statically — onto the setup step that fixes it). Absent on the wire
    /// unless degraded, and `default` on read, so frames from/for pre-field
    /// peers parse unchanged (SPEC §7.2 example stays exact).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<DegradedReason>,
    /// True when kanata is running the built-in passthrough config (no user
    /// preset selected — remapping nothing). Lets the UI say "passthrough"
    /// instead of surfacing the internal `safe.kbd` path. Omitted from the wire
    /// when false (v1-additive; the §7.2 example stays exact).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub passthrough: bool,
    /// Daemon uptime in seconds.
    pub uptime_s: u64,
    /// Daemon version.
    pub daemon_version: String,
}

/// One line of a [`ResponsePayload::DoctorReport`] (SPEC §7.2, §9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorCheck {
    /// Check name.
    pub name: String,
    /// Whether the check passed.
    pub ok: bool,
    /// Detail line.
    pub detail: String,
    /// Actionable fix hint when it failed.
    pub fix_hint: Option<String>,
}

/// A preset as reported by `ListPresets`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresetInfo {
    /// Preset name.
    pub name: String,
    /// Path to its `.kbd` file.
    pub config: String,
    /// Whether it autostarts.
    pub autostart: bool,
    /// Whether it is the active preset.
    pub active: bool,
}

/// An input device as reported by `GetDevices`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// Device name (as kanata sees it).
    pub name: String,
    /// Whether kanata is currently matching/grabbing it.
    pub matched: bool,
}

impl DeviceInfo {
    /// What every device-list surface shows when the name is blank. Some real
    /// IOHID devices carry no product string (HW Run 10 finding, 2026-07-17)
    /// — the wire `name` stays the raw truth; presentation goes through here
    /// so the CLI and the devices window can never drift apart.
    pub const UNNAMED: &'static str = "Unnamed device";

    /// True when the device reported no usable product string.
    pub fn is_unnamed(&self) -> bool {
        self.name.trim().is_empty()
    }

    /// The name to display: the device's own, or [`Self::UNNAMED`].
    pub fn display_name(&self) -> &str {
        if self.is_unnamed() {
            Self::UNNAMED
        } else {
            &self.name
        }
    }
}

/// Asynchronous events pushed to subscribers (SPEC §7.2 `Event{...}`).
///
/// Tagged with `event` so the discriminator does not collide with the
/// response envelope's `type` tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event")]
pub enum Event {
    /// Supervisor state transition.
    StateChanged {
        /// Previous state.
        from: SupervisorState,
        /// New state.
        to: SupervisorState,
        /// Degraded reason, when `to == Degraded`.
        reason: Option<DegradedReason>,
    },
    /// kanata reported a new active layer.
    LayerChanged {
        /// New layer name.
        layer: String,
    },
    /// A keyboard was added or removed.
    DeviceChanged {
        /// True if added, false if removed.
        added: bool,
        /// Device name.
        name: String,
    },
    /// kanata crashed.
    Crash {
        /// Exit code, if any.
        code: Option<i32>,
        /// Terminating signal, if any.
        signal: Option<i32>,
    },
    /// A driver/VHID health problem was detected.
    DriverIssue {
        /// Which problem.
        kind: ErrorKind,
        /// Actionable message.
        message: String,
    },
    /// A new config was applied.
    ConfigApplied {
        /// Preset name, when applied by name.
        preset: Option<String>,
        /// Path that was applied.
        path: String,
    },
    /// The configured preset list changed (add/remove/reload/set). A signal to
    /// re-fetch presets — no payload, so it stays cheap and the client always
    /// reads the authoritative list. Lets the tray's Presets menu update
    /// without a reconnect (SPEC §8).
    PresetsChanged,
}

/// Negotiate a protocol version: the daemon speaks exactly
/// [`PROTOCOL_VERSION`], so it accepts the connection iff that version is in
/// the client's `[min_v, max_v]` range (SPEC §7.1).
pub fn negotiate(min_v: u32, max_v: u32) -> Option<u32> {
    if min_v <= PROTOCOL_VERSION && PROTOCOL_VERSION <= max_v {
        Some(PROTOCOL_VERSION)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PresetDef;
    use std::collections::BTreeMap;

    /// Every request payload variant (SPEC §7.2), for exhaustive round-tripping.
    fn all_request_payloads() -> Vec<RequestPayload> {
        let mut presets = BTreeMap::new();
        presets.insert(
            "main".to_string(),
            PresetDef {
                config: "/main.kbd".into(),
                autostart: true,
                kanata_bin: None,
                extra_args: vec![],
            },
        );
        vec![
            RequestPayload::Hello { min_v: 1, max_v: 3 },
            RequestPayload::GetStatus,
            RequestPayload::Subscribe,
            RequestPayload::Start,
            RequestPayload::Stop,
            RequestPayload::Restart,
            RequestPayload::Pause,
            RequestPayload::Resume,
            RequestPayload::SwitchPreset {
                name: "gaming".into(),
            },
            RequestPayload::ListPresets,
            RequestPayload::ValidateConfig {
                path: "/x.kbd".into(),
            },
            RequestPayload::ApplyConfig {
                path: "/x.kbd".into(),
            },
            RequestPayload::SetPresetList {
                presets: PresetList { presets },
            },
            RequestPayload::AddPreset {
                name: "gaming".into(),
                config: "/x.kbd".into(),
                autostart: false,
            },
            RequestPayload::RemovePreset {
                name: "gaming".into(),
            },
            RequestPayload::ReloadConfig,
            RequestPayload::GetLogs { lines: 100 },
            RequestPayload::FollowLogs,
            RequestPayload::GetDevices,
            RequestPayload::SetAutostart { enabled: true },
            RequestPayload::Doctor,
        ]
    }

    /// Every event variant.
    fn all_events() -> Vec<Event> {
        vec![
            Event::StateChanged {
                from: SupervisorState::Running,
                to: SupervisorState::Degraded,
                reason: Some(DegradedReason::RetryBudgetExhausted),
            },
            Event::LayerChanged {
                layer: "nav".into(),
            },
            Event::DeviceChanged {
                added: true,
                name: "Keychron K2".into(),
            },
            Event::Crash {
                code: Some(1),
                signal: None,
            },
            Event::DriverIssue {
                kind: ErrorKind::VhidDaemonDown,
                message: "daemon down".into(),
            },
            Event::ConfigApplied {
                preset: Some("main".into()),
                path: "/main.kbd".into(),
            },
            Event::PresetsChanged,
        ]
    }

    fn sample_status() -> Status {
        Status {
            state: SupervisorState::Running,
            active_preset: Some("main".into()),
            active_layer: Some("base".into()),
            kanata_pid: Some(4123),
            kanata_version: Some("1.8.1".into()),
            driver_ok: Some(true),
            last_error: None,
            degraded_reason: None,
            passthrough: false,
            uptime_s: 8021,
            daemon_version: "0.3.0".into(),
        }
    }

    /// Every response payload variant.
    fn all_response_payloads() -> Vec<ResponsePayload> {
        let mut payloads = vec![
            ResponsePayload::HelloAck,
            ResponsePayload::Ack,
            ResponsePayload::Error {
                kind: ErrorKind::Incompatible,
                message: "client too old".into(),
            },
            ResponsePayload::Status(sample_status()),
            // The degraded variant: `degraded_reason` is on the wire only
            // here (skip_serializing_if), so round-trip both shapes.
            ResponsePayload::Status(Status {
                state: SupervisorState::Degraded,
                last_error: Some("denied".into()),
                degraded_reason: Some(DegradedReason::InputMonitoringDenied),
                ..sample_status()
            }),
            ResponsePayload::DoctorReport {
                checks: vec![DoctorCheck {
                    name: "driver".into(),
                    ok: false,
                    detail: "not activated".into(),
                    fix_hint: Some("run Setup Wizard".into()),
                }],
            },
            ResponsePayload::LogLine {
                line: "state transition".into(),
            },
            ResponsePayload::Presets {
                presets: vec![PresetInfo {
                    name: "main".into(),
                    config: "/main.kbd".into(),
                    autostart: true,
                    active: true,
                }],
            },
            ResponsePayload::Devices {
                devices: vec![DeviceInfo {
                    name: "Keychron K2".into(),
                    matched: true,
                }],
            },
        ];
        payloads.extend(all_events().into_iter().map(ResponsePayload::Event));
        payloads
    }

    #[test]
    fn every_request_round_trips() {
        for payload in all_request_payloads() {
            for id in [None, Some(42)] {
                let req = Request::new(id, payload.clone());
                let json = serde_json::to_string(&req).unwrap();
                let back: Request = serde_json::from_str(&json).unwrap();
                assert_eq!(req, back, "round-trip failed for {json}");
            }
        }
    }

    #[test]
    fn every_response_round_trips() {
        for payload in all_response_payloads() {
            let resp = Response {
                v: PROTOCOL_VERSION,
                id: Some(7),
                payload: payload.clone(),
            };
            let json = serde_json::to_string(&resp).unwrap();
            let back: Response = serde_json::from_str(&json).unwrap();
            assert_eq!(back, resp, "round-trip failed for {json}");
        }
    }

    #[test]
    fn status_wire_matches_spec_example() {
        // The §7.2 example, compared as JSON values so field order is irrelevant.
        let example = r#"{"v":1,"id":null,"type":"Status","state":"Running",
            "active_preset":"main","active_layer":"base","kanata_pid":4123,
            "kanata_version":"1.8.1","driver_ok":true,"last_error":null,
            "uptime_s":8021,"daemon_version":"0.3.0"}"#;
        let expected: serde_json::Value = serde_json::from_str(example).unwrap();

        let resp = Response {
            v: 1,
            id: None,
            payload: ResponsePayload::Status(sample_status()),
        };
        let actual = serde_json::to_value(&resp).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn request_id_is_always_present_on_the_wire() {
        // Correlation requires `id` to be explicit, even when null.
        let json = serde_json::to_string(&Request::new(None, RequestPayload::GetStatus)).unwrap();
        assert_eq!(json, r#"{"id":null,"type":"GetStatus"}"#);
    }

    #[test]
    fn event_uses_distinct_discriminator() {
        let resp = Response::event(Event::StateChanged {
            from: SupervisorState::Stopped,
            to: SupervisorState::Starting,
            reason: None,
        });
        let value = serde_json::to_value(&resp).unwrap();
        assert_eq!(value["type"], "Event");
        assert_eq!(value["event"], "StateChanged");
        assert_eq!(value["id"], serde_json::Value::Null);
    }

    #[test]
    fn unknown_request_type_is_rejected() {
        assert!(serde_json::from_str::<Request>(r#"{"id":1,"type":"Exec","cmd":"rm"}"#).is_err());
    }

    #[test]
    fn error_kind_all_variants_round_trip() {
        for kind in [
            ErrorKind::Incompatible,
            ErrorKind::InvalidRequest,
            ErrorKind::PathRejected,
            ErrorKind::ConfigInvalid,
            ErrorKind::DriverNotActivated,
            ErrorKind::VhidDaemonDown,
            ErrorKind::AlreadyRunning,
            ErrorKind::NotRunning,
            ErrorKind::Internal,
        ] {
            let json = serde_json::to_string(&kind).unwrap();
            let back: ErrorKind = serde_json::from_str(&json).unwrap();
            assert_eq!(kind, back);
        }
    }

    #[test]
    fn negotiate_accepts_only_supported_version() {
        assert_eq!(negotiate(1, 1), Some(PROTOCOL_VERSION));
        assert_eq!(negotiate(1, 5), Some(PROTOCOL_VERSION));
        assert_eq!(negotiate(2, 5), None);
        assert_eq!(negotiate(0, 0), None);
    }
}
