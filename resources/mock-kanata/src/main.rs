//! mock-kanata — flag-driven kanata test double (SPEC §17).
//!
//! Mirrors the real kanata flags the supervisor uses (`--cfg`, `--check`,
//! `--port`, `--nodelay`) and adds `mock-*` controls to script behaviors:
//! run forever, exit after a delay with a chosen code, fail `--check`.
//! Phase 1+ extend it as gates require (fake TCP layer events,
//! crash-on-signal).

use std::io::Write;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use clap::Parser;

#[derive(Parser)]
#[command(
    name = "mock-kanata",
    version,
    about = "kanata test double for KanataBar"
)]
struct Cli {
    /// Config file path (mirrors kanata; existence is not required).
    #[arg(long)]
    cfg: Option<PathBuf>,

    /// Validate the config and exit (mirrors kanata).
    #[arg(long)]
    check: bool,

    /// TCP server port (mirrors kanata; accepted, not yet served).
    #[arg(long)]
    port: Option<u16>,

    /// Mirrors kanata's --nodelay; accepted and ignored.
    #[arg(long)]
    nodelay: bool,

    /// Mock control: make --check fail with a parse-error message.
    #[arg(long)]
    mock_fail_check: bool,

    /// Mock control: exit after this many milliseconds instead of running forever.
    #[arg(long)]
    mock_exit_after_ms: Option<u64>,

    /// Mock control: exit code to use with --mock-exit-after-ms.
    #[arg(long, default_value_t = 0)]
    mock_exit_code: u8,

    /// Mock control: layer name(s) to emit over the TCP server on connect
    /// (repeatable). Defaults to a single "base" layer (SPEC §17 fake events).
    #[arg(long = "mock-layer")]
    mock_layers: Vec<String>,

    /// Mock control: a line to print to stderr at startup, e.g. a captured
    /// kanata fault ("IOHIDDeviceOpen error: …") for the give-up-worthy-fault
    /// classification tests (SPEC §2, §17).
    #[arg(long)]
    mock_stderr_line: Option<String>,

    /// Mock control: `<ms>:<line>` printed to stdout after the given delay
    /// (repeatable, each on its own timeline). Scripts live log narratives —
    /// e.g. the captured backend-unavailable → recovery sequence (SPEC §17).
    #[arg(long = "mock-stdout-script")]
    mock_stdout_script: Vec<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    if cli.check {
        if cli.mock_fail_check {
            eprintln!("mock-kanata: config error: simulated parse failure");
            return ExitCode::FAILURE;
        }
        // Content-based validation so tests can model a broken config by its
        // contents (a file containing "BROKEN"), exercising the real `--check`
        // code path in the supervisor and config manager.
        if let Some(cfg) = &cli.cfg {
            if let Ok(contents) = std::fs::read_to_string(cfg) {
                if contents.contains("BROKEN") {
                    eprintln!("mock-kanata: config error: line 1: unexpected token BROKEN");
                    return ExitCode::FAILURE;
                }
            }
        }
        println!("mock-kanata: config OK");
        return ExitCode::SUCCESS;
    }

    println!(
        "mock-kanata: started (cfg={:?}, port={:?})",
        cli.cfg, cli.port
    );

    if let Some(line) = &cli.mock_stderr_line {
        eprintln!("{line}");
    }

    // Timed stdout lines (`<ms>:<line>`), each on its own thread so delays
    // don't stack; stdout is line-buffered to a pipe, so flush explicitly.
    for entry in &cli.mock_stdout_script {
        let Some((ms, line)) = entry.split_once(':') else {
            eprintln!("mock-kanata: bad --mock-stdout-script entry (want <ms>:<line>): {entry}");
            return ExitCode::FAILURE;
        };
        let Ok(ms) = ms.parse::<u64>() else {
            eprintln!("mock-kanata: bad --mock-stdout-script delay: {entry}");
            return ExitCode::FAILURE;
        };
        let line = line.to_string();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(ms));
            println!("{line}");
            let _ = std::io::stdout().flush();
        });
    }

    // Serve fake TCP layer events (SPEC §17), like `kanata --port`.
    if let Some(port) = cli.port {
        let layers = if cli.mock_layers.is_empty() {
            vec!["base".to_string()]
        } else {
            cli.mock_layers.clone()
        };
        thread::spawn(move || serve_layers(port, layers));
    }

    match cli.mock_exit_after_ms {
        Some(ms) => {
            thread::sleep(Duration::from_millis(ms));
            eprintln!("mock-kanata: exiting with code {}", cli.mock_exit_code);
            ExitCode::from(cli.mock_exit_code)
        }
        None => loop {
            thread::sleep(Duration::from_secs(3600));
        },
    }
}

/// Accept TCP connections and emit `{"LayerChange":{"new":"<layer>"}}` NDJSON
/// events, then hold the connection open like real kanata.
fn serve_layers(port: u16, layers: Vec<String>) {
    let Ok(listener) = TcpListener::bind(("127.0.0.1", port)) else {
        return;
    };
    for stream in listener.incoming().flatten() {
        let layers = layers.clone();
        thread::spawn(move || {
            let mut stream = stream;
            for layer in &layers {
                let line = format!("{{\"LayerChange\":{{\"new\":\"{layer}\"}}}}\n");
                if stream.write_all(line.as_bytes()).is_err() {
                    return;
                }
                thread::sleep(Duration::from_millis(20));
            }
            // Hold the connection open (block) as real kanata does.
            loop {
                thread::sleep(Duration::from_secs(3600));
            }
        });
    }
}
