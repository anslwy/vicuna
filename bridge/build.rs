// Embed the built web UI (`../build`, from `npm run build`) into the
// `vicuna` binary for `vicuna web`. A checkout that hasn't built the
// frontend still compiles: the list is just empty and `vicuna web` says so.
// `VICUNA_WEB_BUILD_DIR` points at another build folder.

use std::path::{Path, PathBuf};

fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        if p.is_dir() {
            walk(root, &p, out);
        } else if p.is_file() {
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            out.push((rel, p.canonicalize().unwrap_or(p)));
        }
    }
}

fn main() {
    println!("cargo:rerun-if-env-changed=VICUNA_WEB_BUILD_DIR");
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = std::env::var("VICUNA_WEB_BUILD_DIR").map(PathBuf::from).unwrap_or_else(|_| manifest.join("../build"));
    println!("cargo:rerun-if-changed={}", root.display());
    let mut files = Vec::new();
    if root.join("index.html").is_file() {
        walk(&root, &root, &mut files);
    } else {
        println!("cargo:warning=no web UI at {} (run `npm run build` first); `vicuna web` will need --web-dir", root.display());
    }
    let mut src = String::from("pub static WEB_FILES: &[(&str, &[u8])] = &[\n");
    for (rel, abs) in &files {
        src.push_str(&format!("    ({rel:?}, include_bytes!({:?})),\n", abs.to_string_lossy()));
    }
    src.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("web_assets.rs");
    std::fs::write(out, src).unwrap();
}
