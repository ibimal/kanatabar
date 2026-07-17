//! kanatactl — KanataBar control CLI (SPEC §9).
//!
//! A thin client of the control protocol (SPEC §7). Exit codes (SPEC §9):
//! 0 ok · 1 operational error · 2 usage · 3 cannot connect · 4 degraded.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use kanatabar_core::ipc::{DoctorCheck, Event, RequestPayload, ResponsePayload, Status};
use kanatabar_core::state::SupervisorState;
use kanatactl::install::{self, Component, InstallConfig};
use kanatactl::{Client, DEFAULT_SOCKET};

// Exit codes (SPEC §9).
const EXIT_OK: u8 = 0;
const EXIT_OPERATIONAL: u8 = 1;
const EXIT_USAGE: u8 = 2;
const EXIT_CANNOT_CONNECT: u8 = 3;
const EXIT_DEGRADED: u8 = 4;

#[derive(Parser)]
#[command(name = "kanatactl", version, about = "Control the KanataBar daemon")]
struct Cli {
    /// Control socket path.
    #[arg(long, env = "KANATABAR_SOCK", default_value = DEFAULT_SOCKET)]
    socket: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show daemon status.
    Status {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Start kanata.
    Start,
    /// Stop kanata (the daemon keeps running).
    Stop,
    /// Restart kanata.
    Restart,
    /// Pause remapping.
    Pause,
    /// Resume from pause.
    Resume,
    /// Stream state-change events until interrupted.
    Watch,
    /// Manage presets (SPEC §9).
    Preset {
        #[command(subcommand)]
        cmd: PresetCmd,
    },
    /// Validate or apply a `.kbd` config (SPEC §9).
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    /// Show or follow the daemon's buffered log (SPEC §6.6, §9).
    Logs {
        /// Number of buffered lines to fetch.
        #[arg(short = 'n', long, default_value_t = 50)]
        lines: u32,
        /// Keep streaming new lines until interrupted.
        #[arg(long)]
        follow: bool,
    },
    /// List input devices the daemon can see (SPEC §9).
    Devices,
    /// Enable/disable autostart of the active preset.
    Autostart {
        /// `on` or `off`.
        state: OnOff,
    },
    /// Run the full preflight checklist (SPEC §9). `--json` doubles as a
    /// bug-report bundle.
    Doctor {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Install the daemon (LaunchDaemon) and/or agent (LaunchAgent) (SPEC §9,
    /// §10). Requires root (`sudo`).
    Install(InstallArgs),
    /// Undo `install`: bootout the launchd job(s) and remove every path they
    /// touched (SPEC §9, §10). Requires root (`sudo`).
    Uninstall(InstallArgs),
}

#[derive(Subcommand)]
enum PresetCmd {
    /// List configured presets (active one marked).
    List,
    /// Validate and switch to a named preset.
    Switch {
        /// Preset name from config.toml.
        name: String,
    },
    /// Add or update a preset in config.toml (no need to hand-edit it).
    Add {
        /// Preset name.
        name: String,
        /// Path to the preset's `.kbd` file.
        config: PathBuf,
        /// Start this preset automatically when the daemon boots.
        #[arg(long)]
        autostart: bool,
    },
    /// Remove a preset from config.toml.
    Remove {
        /// Preset name.
        name: String,
    },
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Run `kanata --check` on a `.kbd` without applying it.
    Validate {
        /// Path to the `.kbd` file.
        path: PathBuf,
    },
    /// Validate then apply a `.kbd` (backed up as last-known-good).
    Apply {
        /// Path to the `.kbd` file.
        path: PathBuf,
    },
    /// Re-read config.toml so hand edits (presets) take effect without
    /// restarting the daemon. `[defaults]` changes still need a restart.
    Reload,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum OnOff {
    On,
    Off,
}

#[derive(Args)]
struct InstallArgs {
    /// Only the root daemon (LaunchDaemon).
    #[arg(long)]
    daemon_only: bool,
    /// Only the per-user tray agent (LaunchAgent).
    #[arg(long)]
    agent_only: bool,
    /// Root prefix for every installed path; for tests only.
    #[arg(long, hide = true)]
    prefix: Option<PathBuf>,
    /// Skip `launchctl bootstrap`/`bootout`; for tests only.
    #[arg(long, hide = true)]
    skip_launchctl: bool,
}

impl InstallArgs {
    fn into_config(self) -> Result<InstallConfig, &'static str> {
        let component = match (self.daemon_only, self.agent_only) {
            (true, true) => return Err("--daemon-only and --agent-only are mutually exclusive"),
            (true, false) => Component::Daemon,
            (false, true) => Component::Agent,
            (false, false) => Component::Both,
        };
        let mut config = InstallConfig {
            component,
            skip_launchctl: self.skip_launchctl,
            ..InstallConfig::default()
        };
        if let Some(prefix) = self.prefix {
            config.prefix = prefix;
        }
        Ok(config)
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    // install/uninstall never talk to the daemon (a fresh install runs before
    // it exists at all; uninstall is about to remove it) — dispatch before
    // connecting.
    let command = match cli.command {
        Command::Install(args) => return ExitCode::from(run_install(args)),
        Command::Uninstall(args) => return ExitCode::from(run_uninstall(args)),
        other => other,
    };

    let mut client = match Client::connect(&cli.socket).await {
        Ok(client) => client,
        Err(err) => {
            // `doctor` still reports — an unreachable daemon is itself the
            // first failed check (SPEC §9), so it works before/after install.
            if let Command::Doctor { json } = &command {
                render_doctor(&doctor_offline_report(&cli.socket), *json);
                return ExitCode::from(EXIT_CANNOT_CONNECT);
            }
            eprintln!(
                "cannot connect to daemon at {}: {err:#}",
                cli.socket.display()
            );
            return ExitCode::from(EXIT_CANNOT_CONNECT);
        }
    };

    let code = match command {
        Command::Status { json } => status(&mut client, json).await,
        Command::Start => simple(&mut client, RequestPayload::Start, "started").await,
        Command::Stop => simple(&mut client, RequestPayload::Stop, "stopped").await,
        Command::Restart => simple(&mut client, RequestPayload::Restart, "restarting").await,
        Command::Pause => simple(&mut client, RequestPayload::Pause, "paused").await,
        Command::Resume => simple(&mut client, RequestPayload::Resume, "resuming").await,
        Command::Watch => watch(&mut client).await,
        Command::Preset { cmd } => preset(&mut client, cmd).await,
        Command::Config { cmd } => config(&mut client, cmd).await,
        Command::Logs { lines, follow } => logs(&mut client, lines, follow).await,
        Command::Devices => devices(&mut client).await,
        Command::Autostart { state } => {
            simple(
                &mut client,
                RequestPayload::SetAutostart {
                    enabled: matches!(state, OnOff::On),
                },
                "autostart updated",
            )
            .await
        }
        Command::Doctor { json } => doctor(&mut client, json).await,
        Command::Install(_) | Command::Uninstall(_) => unreachable!("handled above"),
    };
    ExitCode::from(code)
}

/// `kanatactl preset list|switch` (SPEC §9).
async fn preset(client: &mut Client, cmd: PresetCmd) -> u8 {
    match cmd {
        PresetCmd::List => match client.request(RequestPayload::ListPresets).await {
            Ok(response) => match response.payload {
                ResponsePayload::Presets { presets } => {
                    if presets.is_empty() {
                        println!("No presets configured — KanataBar is passing keys through.");
                        suggest_existing_configs();
                        return EXIT_OK;
                    }
                    for preset in presets {
                        println!(
                            "{} {:<16} {}{}",
                            if preset.active { "*" } else { " " },
                            preset.name,
                            preset.config,
                            if preset.autostart {
                                "  [autostart]"
                            } else {
                                ""
                            },
                        );
                    }
                    EXIT_OK
                }
                ResponsePayload::Error { message, .. } => operational_msg(&message),
                other => operational_msg(&format!("unexpected reply: {other:?}")),
            },
            Err(err) => operational(err),
        },
        PresetCmd::Switch { name } => {
            simple(client, RequestPayload::SwitchPreset { name }, "switched").await
        }
        PresetCmd::Add {
            name,
            config,
            autostart,
        } => {
            // Absolutize client-side so config.toml stores a stable path (the
            // daemon's cwd differs); the daemon re-checks the file exists.
            let config = std::fs::canonicalize(&config)
                .unwrap_or(config)
                .display()
                .to_string();
            simple(
                client,
                RequestPayload::AddPreset {
                    name,
                    config,
                    autostart,
                },
                "preset added",
            )
            .await
        }
        PresetCmd::Remove { name } => {
            simple(
                client,
                RequestPayload::RemovePreset { name },
                "preset removed",
            )
            .await
        }
    }
}

/// When no presets are configured, scan the user's `~/.config/kanata` for
/// existing `.kbd` files and name them, so a kanata user who already has a
/// config learns how to turn it into a preset instead of starting blank
/// (v0.1.1 onboarding). Runs in the CLI, as the user — no daemon or root.
fn suggest_existing_configs() {
    let kbds = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| {
            let dir = kanatabar_core::kanata::kanata_config_dir(&home);
            let mut kbds: Vec<PathBuf> = std::fs::read_dir(&dir)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|ext| ext == "kbd"))
                .collect();
            kbds.sort();
            kbds
        })
        .unwrap_or_default();

    if kbds.is_empty() {
        println!("Add a preset to start remapping:");
        println!("  kanatactl preset add <name> <path/to.kbd> [--autostart]");
        return;
    }

    println!("Found a kanata config — add it as a preset to start remapping:");
    for kbd in &kbds {
        // A file literally named `config.kbd` makes a poor preset name; fall
        // back to `main` so the suggested command reads naturally.
        let stem = kbd.file_stem().and_then(|s| s.to_str()).unwrap_or("main");
        let name = if stem == "config" { "main" } else { stem };
        println!("  kanatactl preset add {name} {}", kbd.display());
    }
}

/// `kanatactl config validate|apply` (SPEC §9). Paths are absolutized
/// client-side: the daemon resolves them in *its* working directory.
async fn config(client: &mut Client, cmd: ConfigCmd) -> u8 {
    let absolutize = |path: PathBuf| {
        std::fs::canonicalize(&path).unwrap_or(path) // daemon re-checks anyway
    };
    match cmd {
        ConfigCmd::Validate { path } => {
            let path = absolutize(path).display().to_string();
            simple(client, RequestPayload::ValidateConfig { path }, "config OK").await
        }
        ConfigCmd::Apply { path } => {
            let path = absolutize(path).display().to_string();
            simple(client, RequestPayload::ApplyConfig { path }, "applied").await
        }
        ConfigCmd::Reload => {
            simple(client, RequestPayload::ReloadConfig, "config.toml reloaded").await
        }
    }
}

/// `kanatactl logs [-n N] [--follow]` (SPEC §6.6, §9): `GetLogs` answers with
/// N `LogLine` frames then an `Ack`; `--follow` then streams pushed lines.
async fn logs(client: &mut Client, lines: u32, follow: bool) -> u8 {
    if let Err(err) = client.send_request(RequestPayload::GetLogs { lines }).await {
        return operational(err);
    }
    loop {
        match client.next_response().await {
            Ok(response) => match response.payload {
                ResponsePayload::LogLine { line } => println!("{line}"),
                ResponsePayload::Ack => break,
                ResponsePayload::Error { message, .. } => return operational_msg(&message),
                ResponsePayload::Event(_) => {} // not subscribed, but harmless
                other => return operational_msg(&format!("unexpected reply: {other:?}")),
            },
            Err(err) => return operational(err),
        }
    }
    if !follow {
        return EXIT_OK;
    }
    if let Err(err) = client.send_request(RequestPayload::FollowLogs).await {
        return operational(err);
    }
    loop {
        match client.next_response().await {
            Ok(response) => match response.payload {
                ResponsePayload::LogLine { line } => println!("{line}"),
                ResponsePayload::Ack => {} // the FollowLogs acknowledgement
                ResponsePayload::Error { message, .. } => return operational_msg(&message),
                _ => {}
            },
            Err(err) => return operational(err),
        }
    }
}

/// `kanatactl devices` (SPEC §9): the daemon's live device list; `matched`
/// mirrors the re-sync relevance rule (keyboard, not Karabiner-virtual).
async fn devices(client: &mut Client) -> u8 {
    match client.request(RequestPayload::GetDevices).await {
        Ok(response) => match response.payload {
            ResponsePayload::Devices { devices } => {
                if devices.is_empty() {
                    println!(
                        "(no devices recorded — the monitor reports devices \
                         present at daemon start and hotplugs since)"
                    );
                    return EXIT_OK;
                }
                for device in devices {
                    // display_name: nameless IOHID devices exist (HW Run 10);
                    // a blank line here would look like broken output.
                    println!(
                        "{} {}",
                        if device.matched { "✔" } else { " " },
                        device.display_name(),
                    );
                }
                EXIT_OK
            }
            ResponsePayload::Error { message, .. } => operational_msg(&message),
            other => operational_msg(&format!("unexpected reply: {other:?}")),
        },
        Err(err) => operational(err),
    }
}

/// `kanatactl install` (SPEC §9, §10): copy binaries, write plists, bootstrap
/// the launchd job(s).
fn run_install(args: InstallArgs) -> u8 {
    let config = match args.into_config() {
        Ok(config) => config,
        Err(msg) => return usage(msg),
    };
    match install::install(&config) {
        Ok(report) => {
            for path in &report.created {
                println!("installed {}", path.display());
            }
            EXIT_OK
        }
        Err(err) => operational(err),
    }
}

/// `kanatactl uninstall` (SPEC §9, §10): bootout the launchd job(s) and
/// remove every path `install` could have created.
fn run_uninstall(args: InstallArgs) -> u8 {
    let config = match args.into_config() {
        Ok(config) => config,
        Err(msg) => return usage(msg),
    };
    match install::uninstall(&config) {
        Ok(report) => {
            for path in &report.removed {
                println!("removed {}", path.display());
            }
            EXIT_OK
        }
        Err(err) => operational(err),
    }
}

fn usage(message: &str) -> u8 {
    eprintln!("error: {message}");
    EXIT_USAGE
}

/// Fetch and print status; exit 4 when the daemon is `Degraded` (SPEC §9).
async fn status(client: &mut Client, json: bool) -> u8 {
    let response = match client.request(RequestPayload::GetStatus).await {
        Ok(response) => response,
        Err(err) => return operational(err),
    };
    match response.payload {
        ResponsePayload::Status(status) => {
            if json {
                match serde_json::to_string_pretty(&status) {
                    Ok(text) => println!("{text}"),
                    Err(err) => return operational(err),
                }
            } else {
                print_status(&status);
            }
            if status.state == SupervisorState::Degraded {
                EXIT_DEGRADED
            } else {
                EXIT_OK
            }
        }
        ResponsePayload::Error { message, .. } => operational_msg(&message),
        other => operational_msg(&format!("unexpected reply: {other:?}")),
    }
}

/// Send a lifecycle command and report the acknowledgement.
async fn simple(client: &mut Client, payload: RequestPayload, done: &str) -> u8 {
    match client.request(payload).await {
        Ok(response) => match response.payload {
            ResponsePayload::Ack => {
                println!("{done}");
                EXIT_OK
            }
            ResponsePayload::Error { message, .. } => operational_msg(&message),
            other => operational_msg(&format!("unexpected reply: {other:?}")),
        },
        Err(err) => operational(err),
    }
}

/// Subscribe and print events until the connection closes or Ctrl-C.
async fn watch(client: &mut Client) -> u8 {
    match client.request(RequestPayload::Subscribe).await {
        Ok(response) if matches!(response.payload, ResponsePayload::Ack) => {}
        Ok(response) => {
            return operational_msg(&format!("subscribe failed: {:?}", response.payload))
        }
        Err(err) => return operational(err),
    }
    loop {
        match client.next_event().await {
            Ok(event) => print_event(&event),
            Err(err) => return operational(err),
        }
    }
}

/// Fetch and render the doctor report; exit 0 when every check passes, else 1
/// (SPEC §9). The report is the manual-QA oracle.
async fn doctor(client: &mut Client, json: bool) -> u8 {
    let response = match client.request(RequestPayload::Doctor).await {
        Ok(response) => response,
        Err(err) => return operational(err),
    };
    match response.payload {
        ResponsePayload::DoctorReport { checks } => {
            render_doctor(&checks, json);
            if kanatabar_core::doctor::all_ok(&checks) {
                EXIT_OK
            } else {
                EXIT_OPERATIONAL
            }
        }
        ResponsePayload::Error { message, .. } => operational_msg(&message),
        other => operational_msg(&format!("unexpected reply: {other:?}")),
    }
}

/// The report shown when the daemon can't be reached: the "daemon reachable"
/// check itself fails (SPEC §9), so `doctor` is useful before install too.
fn doctor_offline_report(socket: &std::path::Path) -> Vec<DoctorCheck> {
    vec![DoctorCheck {
        name: kanatabar_core::doctor::checks::DAEMON.to_string(),
        ok: false,
        detail: format!("cannot connect to {}", socket.display()),
        fix_hint: Some(
            "is kanatad installed and running? try `sudo kanatactl install`".to_string(),
        ),
    }]
}

fn render_doctor(checks: &[DoctorCheck], json: bool) {
    if json {
        // Doubles as a bug-report bundle (SPEC §9); `{"checks":[…]}` mirrors
        // the `DoctorReport` payload shape.
        match serde_json::to_string_pretty(&serde_json::json!({ "checks": checks })) {
            Ok(text) => println!("{text}"),
            Err(err) => eprintln!("error: {err}"),
        }
        return;
    }
    // Shared with the tray's "Run Doctor" so both show the identical report.
    print!("{}", kanatabar_core::doctor::format_report(checks));
}

fn print_status(status: &Status) {
    println!("state:          {:?}", status.state);
    if let Some(preset) = &status.active_preset {
        println!("preset:         {preset}");
    } else if status.passthrough {
        println!("preset:         (none — passthrough, remapping nothing)");
    }
    if let Some(layer) = &status.active_layer {
        println!("layer:          {layer}");
    }
    match status.kanata_pid {
        Some(pid) => println!("kanata pid:     {pid}"),
        None => println!("kanata pid:     (not running)"),
    }
    if let Some(error) = &status.last_error {
        println!("last error:     {error}");
    }
    println!("uptime:         {}s", status.uptime_s);
    println!("daemon version: {}", status.daemon_version);
}

fn print_event(event: &Event) {
    match event {
        Event::StateChanged { from, to, reason } => {
            let suffix = reason
                .map(|r| format!(" ({})", r.describe()))
                .unwrap_or_default();
            println!("{from:?} -> {to:?}{suffix}");
        }
        other => println!("{other:?}"),
    }
}

fn operational(err: impl std::fmt::Display) -> u8 {
    eprintln!("error: {err}");
    EXIT_OPERATIONAL
}

fn operational_msg(message: &str) -> u8 {
    eprintln!("error: {message}");
    EXIT_OPERATIONAL
}
