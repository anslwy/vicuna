// Vicuna headless bridge binary: serves the backend over loopback HTTP so a
// plain browser (live-browser/Chrome) can drive the real backend without
// the Tauri shell. Dev/test tool only.
//
//   cargo run --bin bridge [--port 1421]     (run from src-tauri/)
fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(1421);
    vicuna_lib::bridge::run(port);
}
