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

use vicuna_lib::server::{server_dir, server_token, Config};

/// Bind, print the banner (+ tailscale), serve forever.
fn run(cfg: Config) {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("bridge runtime");
    let bound = rt.block_on(vicuna_lib::server::bind(&cfg)).unwrap_or_else(|e| panic!("bridge {e}"));
    let addr = bound.addr;
    match (&cfg.token, &cfg.db) {
        (Some(t), Some(db)) => {
            println!("vicuna server {} on http://{addr}", env!("CARGO_PKG_VERSION"));
            println!("  chats: {}", db.display());
            println!("  token: {t}");
            if cfg.tailscale {
                match cli::tailscale_expose(cfg.port, cfg.https_port) {
                    Ok(url) => {
                        println!("  tailnet URL: {url}");
                        println!("In Vicuna: sidebar server menu → Connect to new server… → {url} + the token above.");
                    }
                    Err(e) => println!("  tailscale: {e}"),
                }
            } else {
                println!("Expose it to your tailnet (HTTPS) with `vicuna serve --tailscale`, or by hand:");
                println!("  tailscale serve --bg --https=8443 http://127.0.0.1:{}", cfg.port);
            }
            if cfg.bind != "127.0.0.1" && cfg.bind != "localhost" {
                println!("WARNING: listening on {} over plain HTTP; prefer --bind 127.0.0.1 behind tailscale serve.", cfg.bind);
            }
        }
        _ => println!("vicuna bridge on http://{addr}"),
    }
    rt.block_on(bound.serve());
}

const USAGE: &str = "vicuna — Vicuna's command line

usage:
  vicuna serve [--port N] [--bind ADDR] [--db PATH] [--tailscale [--https-port N]]
        Run a Vicuna server for the desktop app (token auth; chats in ~/vicuna/server).
        --tailscale also exposes it over `tailscale serve` (HTTPS, tailnet only)
        and prints the URL to paste into the app. Default 127.0.0.1:7421.
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
  vicuna upgrade [--check] [--force] [--no-restart] [--dry-run]
        Install the latest release (checksum-verified, atomic replace), back up
        the server db to ~/vicuna/server/backups/<time>/ and restart a running
        `vicuna serve` (systemd / launchd / plain process) on the new version.
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

/// Parse `serve` options (shared by `serve` and `install-service`).
fn parse_serve(args: &[String]) -> Config {
    let mut cfg = Config { bind: "127.0.0.1".into(), port: 7421, token: None, db: None, tailscale: false, https_port: None, resource_dir: None };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().unwrap_or_else(|| die(&format!("{a} needs a value")));
        match a.as_str() {
            "--port" => cfg.port = val().parse().unwrap_or_else(|_| die("bad --port")),
            "--bind" => cfg.bind = val(),
            "--db" => cfg.db = Some(std::path::PathBuf::from(val())),
            "--tailscale" => cfg.tailscale = true,
            "--https-port" => cfg.https_port = Some(val().parse().unwrap_or_else(|_| die("bad --https-port"))),
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0)
            }
            other => die(&format!("unknown option {other}")),
        }
    }
    cfg
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = args.get(1..).unwrap_or(&[]).to_vec();
    match args.first().map(String::as_str) {
        Some("serve") => {
            let mut cfg = parse_serve(&rest);
            cfg.token = Some(server_token());
            if cfg.db.is_none() {
                cfg.db = Some(server_dir().join("vicuna.db"));
            }
            run(cfg);
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
            run(Config { bind: "127.0.0.1".into(), port, token: None, db: None, tailscale: false, https_port: None, resource_dir: None });
        }
        Some("-h") | Some("--help") | Some("help") => println!("{USAGE}"),
        Some("-V") | Some("--version") | Some("version") => println!("vicuna {}", env!("CARGO_PKG_VERSION")),
        // `cargo run -- <port>` (the old bridge invocation) keeps working
        Some(p) if p.parse::<u16>().is_ok() => {
            run(Config { bind: "127.0.0.1".into(), port: p.parse().unwrap(), token: None, db: None, tailscale: false, https_port: None, resource_dir: None })
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
        None => run(Config { bind: "127.0.0.1".into(), port: 1421, token: None, db: None, tailscale: false, https_port: None, resource_dir: None }),
    }
}

