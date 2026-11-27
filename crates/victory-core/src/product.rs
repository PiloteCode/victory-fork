//! This fork's product name, in one place.
//!
//! What the user sees is called by it: the window's caption, the command
//! line's name, version and help, the first lines of a run, the crash log's
//! entries, the switches by their `PILOTEBOX_` names. Crates, modules,
//! source paths and cache file names keep upstream's names, and the layer
//! reads its switches by their `VICTORY_` names (each `PILOTEBOX_X` is
//! copied to `VICTORY_X` at start-up, [`alias_switches`]), so that merging
//! upstream's trees stays easy.

use std::ffi::OsString;

/// The product's name.
pub const NAME: &str = "PiloteBox";

/// The program's file name, without `.exe`.
pub const BINARY: &str = "pilotebox";

/// What it is, in one line.
pub const DESCRIPTION: &str = "plays Xbox Series titles natively on Windows";

/// The fork's version: its wave, then the steps since (0.12: the new layer;
/// 0.12.1: mouths and shaders; 0.12.2: the upscaler's containers and the
/// commands for playing; 0.13.0: avatarmanz's newer layer, TaynanGT's tree,
/// title buffers placed on the card's side and the settle's cause). The
/// crates keep the workspace's version.
pub const VERSION: &str = "0.13.0";

/// The prefix of the switches as the user sets them.
pub const PREFIX: &str = "PILOTEBOX_";

/// The prefix the layer reads them by.
pub const UPSTREAM_PREFIX: &str = "VICTORY_";

/// The first line a run prints: name, version and description.
pub fn banner() -> String {
    format!("{NAME} {VERSION} | {DESCRIPTION}")
}

/// The first line of a command's output: name, version and what it does.
pub fn heading(what: &str) -> String {
    format!("{NAME} {VERSION} | {what}")
}

/// A switch's name as the user sees it: `VICTORY_X` is `PILOTEBOX_X`.
pub fn shown(name: &str) -> String {
    match name.strip_prefix(UPSTREAM_PREFIX) {
        Some(rest) => format!("{PREFIX}{rest}"),
        None => name.to_owned(),
    }
}

/// The `VICTORY_` switches to set for the `PILOTEBOX_` ones among `vars`:
/// each `PILOTEBOX_X` whose `VICTORY_X` is not set, with its value.
pub fn aliases(vars: impl IntoIterator<Item = (OsString, OsString)>) -> Vec<(String, OsString)> {
    let vars: Vec<(OsString, OsString)> = vars.into_iter().collect();
    let set = |name: &str| vars.iter().any(|(k, _)| k.to_str() == Some(name));
    vars.iter()
        .filter_map(|(k, v)| {
            let rest = k.to_str()?.strip_prefix(PREFIX)?;
            let upstream = format!("{UPSTREAM_PREFIX}{rest}");
            (!rest.is_empty() && !set(&upstream)).then(|| (upstream, v.clone()))
        })
        .collect()
}

/// Copies each `PILOTEBOX_X` of the environment to `VICTORY_X` when that is
/// not set, so that the layer, which reads the `VICTORY_` names, sees it.
///
/// # Safety
/// No other thread may read or write the environment meanwhile: call it
/// first thing in `main`.
pub unsafe fn alias_switches() {
    for (name, value) in aliases(std::env::vars_os()) {
        // SAFETY: the caller has no other thread yet.
        unsafe { std::env::set_var(name, value) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_banner_names_the_product() {
        let banner = banner();
        assert!(banner.starts_with("PiloteBox 0.13.0 | "), "{banner}");
        assert_eq!(heading("doctor"), "PiloteBox 0.13.0 | doctor");
        assert!(BINARY.chars().all(|c| c.is_ascii_lowercase()));
    }

    #[test]
    fn our_switch_names_stand_for_upstreams() {
        let vars = [
            ("PILOTEBOX_BVH_BUILDER", "1"),
            ("PILOTEBOX_DATA_DIR", r"D:\data"),
            // Set under both names: the upstream one is kept.
            ("PILOTEBOX_RELAY", "1"),
            ("VICTORY_RELAY", "2"),
            ("PILOTEBOX_", "x"),
            ("PATH", "x"),
        ]
        .map(|(k, v)| (OsString::from(k), OsString::from(v)));
        let mut set = aliases(vars);
        set.sort();
        assert_eq!(
            set,
            [
                ("VICTORY_BVH_BUILDER".to_owned(), OsString::from("1")),
                ("VICTORY_DATA_DIR".to_owned(), OsString::from(r"D:\data")),
            ]
        );
        assert_eq!(shown("VICTORY_BVH_BUILDER"), "PILOTEBOX_BVH_BUILDER");
        assert_eq!(shown("RUST_LOG"), "RUST_LOG");
    }
}
