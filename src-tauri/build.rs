fn main() {
    tauri_build::build();
    // Bake a UTC build timestamp for Settings → App updates, so the app can
    // show the current build's age next to a release's published date (the
    // `latest` rolling tag never changes, so dates are the version signal).
    // Seconds since epoch — the frontend renders it with Date; no chrono needed.
    if let Ok(d) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        println!("cargo:rustc-env=VICUNA_BUILD_UNIX={}", d.as_secs());
    }
}
