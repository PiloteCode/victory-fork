//! This fork's product name, in one place.
//!
//! What the user sees is called by it: the window's caption, the command
//! line's name, version and help, the first lines of a run, the crash log's
//! entries. Crates, modules, the `VICTORY_*` switches, the data and cache
//! folders (`%LOCALAPPDATA%\victory`) and the config file's keys keep
//! upstream's names, so that merging upstream's trees stays easy.

/// The product's name.
pub const NAME: &str = "PiloteBox";

/// The program's file name, without `.exe`.
pub const BINARY: &str = "pilotebox";

/// What it is, in one line.
pub const DESCRIPTION: &str = "plays Xbox Series titles natively on Windows";

/// The workspace's version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The first line a run prints: name, version and description.
pub fn banner() -> String {
    format!("{NAME} {VERSION} | {DESCRIPTION}")
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_banner_names_the_product() {
        let banner = super::banner();
        assert!(banner.starts_with("PiloteBox "), "{banner}");
        assert!(banner.contains(super::VERSION));
        assert!(super::BINARY.chars().all(|c| c.is_ascii_lowercase()));
    }
}
