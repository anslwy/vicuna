// Vicuna headless bridge: loopback HTTP (+ poll-based events) exposing the
// backend commands so a plain browser (live-browser/Chrome) can drive the
// REAL backend without the Tauri shell. Dev/test tool only: binds
// 127.0.0.1, no auth — never expose beyond loopback.
//
//   cargo run --bin bridge [--port 1421]      (src-tauri/)
//   POST /invoke {"cmd": "...", "args": {...}} -> {"ok": true, "data": ...}
//   GET  /poll/<channel-id>                    -> {"events": [ ... ]}
//   GET  /health                                -> {"ok": true}
//
// Argument keys are camelCase (same as Tauri's IPC convention) so frontend
// code is identical between runtimes. Channel args arrive as
// {"__bridge_channel": "<uuid>"} markers; the bridge creates the queue up
// front and hands run_loop a real Channel wired into it.
//
// VICUNA_TEST_RESOURCE_DIR (test-only): when set, browser_* commands
// resolve the bundled `lb` from that dir as if it were the app resource
// root — this is how the bundled (no-PATH) path is verified headlessly
// (see /tmp/vicuna-browser-bundled.sh).

// The server itself lives in vicuna_lib::server (shared with the desktop
// app's embedded server); this binary is the CLI around it.

mod cli;
mod client;
#[cfg(unix)]
mod server;
#[cfg(unix)]
mod upgrade;
mod serve;

use vicuna_lib::server::{server_dir, server_token, Config};

/// The token-less dev bridge: bind, serve forever.
fn run_bridge(port: u16) {
    let cfg = Config { bind: "127.0.0.1".into(), port, token: None, db: None, tailscale: false, https_port: None, resource_dir: vicuna_lib::browser::headless_resource_dir(), web: None };
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("bridge runtime");
    let bound = rt.block_on(vicuna_lib::server::bind(&cfg)).unwrap_or_else(|e| panic!("bridge {e}"));
    println!("vicuna bridge on http://{}", bound.addr);
    rt.block_on(bound.serve_until(shutdown_signal()));
}

pub(crate) async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = signal(SignalKind::terminate()).ok();
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = async {
                match sigterm.as_mut() {
                    Some(s) => { s.recv().await; }
                    None => std::future::pending::<()>().await,
                }
            } => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

const USAGE: &str = "vicuna — Vicuna's command line

usage:
  vicuna serve [-p|--port N] [--bind ADDR] [--db PATH] [--web-dir PATH] [--tailscale [--https-port N]]
        Run the Vicuna server: the backend for the desktop app AND the web UI
        for your browser, on one port (default 127.0.0.1:7421, chats in
        ~/vicuna/server/vicuna.db). Always token-protected; the banner prints
        the Server URL + Token for the app and a Browser URL carrying the
        token as #token=… (`vicuna token` prints it). --tailscale also exposes
        it over `tailscale serve` (HTTPS, tailnet only) and prints those URLs.
        --web-dir PATH serves another `build/` instead of the bundled UI.
  vicuna web [same options as serve] [--no-open]
        Open the Vicuna web UI in your browser. With no options it reuses a
        `vicuna serve` already running on 127.0.0.1:7421; otherwise it starts
        the server with these options (like `vicuna serve`) and opens it.
        --no-open just prints the URL (e.g. over SSH).
  vicuna install-service [serve options]   run `vicuna serve` in the background
        on login/boot (systemd user unit on Linux, launchd agent on macOS)
  vicuna uninstall-service
  vicuna token                             print the server token
  vicuna start [serve options]              start `vicuna serve` in the background
        Uses the installed service (systemd user unit / launchd agent) when there
        is one, else launches a detached `vicuna serve` (default `--tailscale`)
        logging to ~/vicuna/server/serve.log, and writes a pidfile.
  vicuna stop                               stop it (service manager, or SIGTERM)
  vicuna restart [serve options]            restart it detached, so it also
        completes from a session the server itself is hosting
  vicuna status                             running / how it runs / pid / version / log
  vicuna upgrade [--dev | --channel stable|dev] [--check] [--force] [--no-restart] [--dry-run]
        Install the latest release (checksum-verified, atomic replace), back up
        the server db to ~/vicuna/server/backups/<time>/ and restart a running
        `vicuna serve` (systemd / launchd / plain process) on the new version.
        --dev installs the rolling dev build from main (the `latest` prerelease)
        instead of the newest stable release; `vicuna upgrade` goes back to stable
        at the next stable release.
  vicuna open [path]                       open a folder in the Vicuna desktop app
  vicuna [path]                            same as `vicuna open [path]`
  vicuna bridge [port]                     dev bridge for browser testing (127.0.0.1, no auth, 1421)
  vicuna --version

sessions (client of a running `vicuna serve`, local or --server URL|NAME):
  vicuna session list|new|send|chat|view|attach|stop|set|archive|unarchive|delete
        start / view / continue / stop Work sessions on any harness, model,
        workspace and MCP set (`vicuna session help`; alias `vicuna s`)
  vicuna schedule list|add|rm             scheduled sends (`vicuna schedule help`)
  vicuna engines                           installed harnesses
  vicuna models [-e HARNESS]               models a harness offers
  vicuna mcp list|add|enable|disable|rm|tools   MCP servers (`vicuna mcp help`)
  vicuna server list|add|rm                saved servers for --server
  vicuna api list | ACTION [json|k=v …]    call any server action directly (`vicuna api help`)";

fn die(msg: &str) -> ! {
    eprintln!("{msg}\n\n{USAGE}");
    std::process::exit(2)
}

/// `Ok(())` prints nothing extra; a failure is the command's error + exit 1.
#[cfg(unix)]
fn report(r: Result<(), String>) {
    if let Err(e) = r {
        eprintln!("vicuna: {e}");
        std::process::exit(1)
    }
}

/// Parse `serve` options (shared by `serve`, `install-service`, `start`, `restart`).
fn parse_serve(args: &[String]) -> serve::ServeOpts {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        std::process::exit(0)
    }
    serve::parse(args, false).unwrap_or_else(|e| die(&e))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = args.get(1..).unwrap_or(&[]).to_vec();
    match args.first().map(String::as_str) {
        Some("serve") => serve::run(parse_serve(&rest), false),
        Some("web") => {
            if rest.iter().any(|a| a == "-h" || a == "--help") {
                println!("{USAGE}");
                return;
            }
            serve::web(serve::parse(&rest, true).unwrap_or_else(|e| die(&e)));
        }
        Some("install-service") => {
            let _ = parse_serve(&rest); // validate before writing anything
            match cli::install_service(&rest) {
                Ok(m) => {
                    println!("{m}");
                    println!("Token: {}", server_token());
                }
                Err(e) => {
                    eprintln!("install-service failed: {e}");
                    std::process::exit(1)
                }
            }
        }
        Some("uninstall-service") => match cli::uninstall_service() {
            Ok(m) => println!("{m}"),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1)
            }
        },
        Some("token") => println!("{}", server_token()),
        #[cfg(unix)]
        Some("start") => {
            let _ = parse_serve(&rest); // validate the serve options before spawning anything
            report(server::start(&rest));
        }
        #[cfg(unix)]
        Some("stop") => {
            if !rest.is_empty() {
                die("vicuna stop takes no options");
            }
            report(server::stop());
        }
        #[cfg(unix)]
        Some("restart") => {
            let _ = parse_serve(&rest);
            report(server::restart(&rest));
        }
        #[cfg(unix)]
        Some("status") => {
            if !rest.is_empty() {
                die("vicuna status takes no options");
            }
            match server::status() {
                Ok(true) => {}
                Ok(false) => std::process::exit(1), // scriptable, like `systemctl is-active`
                Err(e) => {
                    eprintln!("vicuna status: {e}");
                    std::process::exit(2)
                }
            }
        }
        #[cfg(unix)]
        Some("upgrade") => {
            let opts = upgrade::parse_opts(&rest).unwrap_or_else(|e| die(&e));
            if let Err(e) = upgrade::run(opts, &server_dir()) {
                eprintln!("vicuna upgrade: {e}");
                std::process::exit(1)
            }
        }
        Some("session") | Some("sessions") | Some("s") => client::run_async(client::session_main(&rest)),
        Some("schedule") => client::run_async(client::schedule_main(&rest)),
        Some("engines") | Some("harnesses") => client::run_async(client::engines_main(&rest)),
        Some("models") => client::run_async(client::models_main(&rest)),
        Some("mcp") => client::run_async(client::mcp_main(&rest)),
        Some("server") | Some("servers") => client::run_async(client::server_main(&rest)),
        Some("api") => client::run_async(client::api_main(&rest)),
        Some("open") => match cli::open_in_app(rest.first().map(String::as_str)) {
            Ok(p) => println!("Opened {p} in Vicuna."),
            Err(e) => {
                eprintln!("vicuna open: {e}");
                std::process::exit(1)
            }
        },
        Some("bridge") => {
            let port = rest.first().map(|p| p.parse().unwrap_or_else(|_| die("bad port"))).unwrap_or(1421);
            run_bridge(port);
        }
        Some("-h") | Some("--help") | Some("help") => println!("{USAGE}"),
        Some("-V") | Some("--version") | Some("version") => println!("vicuna {}", env!("CARGO_PKG_VERSION")),
        // `cargo run -- <port>` (the old bridge invocation) keeps working
        Some(p) if p.parse::<u16>().is_ok() => {
            run_bridge(p.parse().unwrap())
        }
        // `vicuna [path]` — the old desktop launcher's behavior
        Some(p) if !p.starts_with('-') => match cli::open_in_app(Some(p)) {
            Ok(p) => println!("Opened {p} in Vicuna."),
            Err(e) => {
                eprintln!("vicuna: {e}");
                std::process::exit(1)
            }
        },
        Some(other) => die(&format!("unknown option {other}")),
        // no args: the dev bridge, as before (docs/scripts run `cargo run` here)
        None => run_bridge(1421),
    }
}

