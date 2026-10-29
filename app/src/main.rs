//! Connect: peer-to-peer remote desktop.
//!
//! With no arguments this opens the app. The headless commands exist for testing without a display:
//!
//! ```text
//! connect host [--test-pattern] [--print-input]
//!                                     share this PC (Windows); Allow/Deny is asked on the terminal
//! connect join <USER-ID> [--seconds N]
//! connect id                          print this PC's User-ID
//! ```
//!
//! Only Windows can host; other platforms can only connect. `--test-pattern` hosts a generated
//! image anywhere, and `--print-input` prints the remote input instead of acting on it; both are
//! for development.
//!
//! Set `CONNECT_CONFIG_DIR` to use a separate identity, e.g. to run two copies on one PC, and
//! `CONNECT_RENDERER=glow` if the window stays blank (falls back from wgpu to plain OpenGL).

mod cli;
mod gui;

use std::path::PathBuf;

use anyhow::{Result, bail};
use connect_client::Stats;
use connect_net as net;
use tracing_subscriber::EnvFilter;

const USAGE: &str = "\
usage: connect                        open the app
       connect host [--test-pattern] [--print-input]
                                      share this PC from the terminal (Windows)
       connect join <USER-ID> [--seconds N]
       connect id                     print this PC's User-ID";

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,iroh=warn,noq=warn".into()))
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    if matches!(args.first().map(String::as_str), Some("-h" | "--help" | "help")) {
        println!("{USAGE}");
        return Ok(());
    }

    let config_dir = match std::env::var_os("CONNECT_CONFIG_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => net::default_config_dir()?,
    };
    let runtime = tokio::runtime::Runtime::new()?;
    // The app asks before creating an account; the terminal commands just make one.
    let key = || net::load_or_create_identity(&config_dir);

    match args.first().map(String::as_str) {
        None => gui::run(runtime, config_dir),
        Some("host") => runtime.block_on(cli::host(key()?, &args[1..])),
        Some("join") => runtime.block_on(cli::join(key()?, &args[1..])),
        Some("id") => {
            println!("{}", net::format_user_id(&key()?.public()));
            Ok(())
        }
        Some(other) => bail!("unknown command {other:?}\n\n{USAGE}"),
    }
}

/// This PC's name as shown to the other side, until the user picks one.
fn device_name() -> String {
    let name = gethostname::gethostname().to_string_lossy().trim().to_owned();
    if name.is_empty() { "My PC".into() } else { name }
}

/// One line of session figures, for the terminal and the in-session overlay.
pub(crate) fn stats_line(s: &Stats) -> String {
    let rtt = s.rtt.map_or("–".into(), |r| format!("{:.0} ms", r.as_secs_f32() * 1000.0));
    let latency = s.latency_ms.map_or("–".into(), |l| format!("~{l:.0} ms"));
    format!(
        "{} · RTT {rtt} · {:.0} fps · {:.1} Mb/s · latency {latency} (encode {:.1} / decode {:.1} ms) · lost {}",
        s.path.label(),
        s.fps,
        s.mbps,
        s.encode_ms,
        s.decode_ms,
        s.frames_lost,
    )
}
