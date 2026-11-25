//! The translator's fingerprint, for the key of translated shaders kept on
//! disk (`src/shader_cache.rs`): what decides a translation's result, read
//! at build time. That is the translator's sources (`victory-shader`), the
//! encoding of a cache entry (`src/shader_cache.rs`), the cache itself
//! (`victory-pipeline`), the locked dependency versions and the toolchain.
//! What is only known when the translator runs (its settings, whether its
//! debugging switches are built in) is added to the key there.
//!
//! The key was the BLAKE3 of our executable: every rebuild, whatever it
//! changed, threw away the thousands of translations a world had cached and
//! the next start translated them all again.

use std::path::{Path, PathBuf};

fn main() {
    let gpu = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("set by cargo"));
    let root = gpu.join("..").join("..");
    let mut files = Vec::new();
    for dir in ["crates/victory-shader/src", "crates/victory-pipeline/src"] {
        let dir = root.join(dir);
        println!("cargo:rerun-if-changed={}", dir.display());
        sources(&dir, &mut files);
    }
    for file in [
        "crates/victory-shader/Cargo.toml",
        "crates/victory-pipeline/Cargo.toml",
        "crates/victory-gpu/src/shader_cache.rs",
        "Cargo.lock",
        "rust-toolchain.toml",
    ] {
        files.push(root.join(file));
    }
    files.sort();
    // FNV-1a over each file's path (from the root) and bytes.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for &b in bytes {
            hash = (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
    };
    for file in &files {
        println!("cargo:rerun-if-changed={}", file.display());
        let name = file.strip_prefix(&root).unwrap_or(file).to_string_lossy().replace('\\', "/");
        eat(name.as_bytes());
        eat(&[0]);
        eat(&std::fs::read(file).unwrap_or_default());
        eat(&[0]);
    }
    println!("cargo:rustc-env=VICTORY_TRANSLATOR_FINGERPRINT={hash:016x}");
}

/// The `.rs` files under `dir`, at any depth.
fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}
