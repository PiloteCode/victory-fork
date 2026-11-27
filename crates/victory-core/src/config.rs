//! Where the user's files live.
//!
//! Resolution order for each value: environment variable, then the config
//! file (`VICTORY_CONFIG`, else `./pilotebox.toml`, `./victory.toml`, then
//! `pilotebox.toml` in the product's own folder), then the default.
//! See `victory.example.toml` at the repository root.
//!
//! The data and cache folders are `%LOCALAPPDATA%\PiloteBox\data` and
//! `\cache` by default. Upstream's `%LOCALAPPDATA%\victory` is used in their
//! place while it is there and ours holds neither (`pilotebox migrate`
//! copies the data over and moves the cache).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::policy::ensure_outside_repo;

#[derive(Debug, Default, Clone)]
pub struct Config {
    /// The user's dump of the title.
    pub game_dir: Option<PathBuf>,
    /// Folder containing the PC FidelityFX upscaler for the optional FSR profile.
    pub fidelityfx_dir: Option<PathBuf>,
    /// Root for all derived data. Always outside the repository.
    pub cache_dir: PathBuf,
    /// What titles write and must keep (persistent local storage). Never
    /// inside the cache, which may be deleted at any time.
    pub data_dir: PathBuf,
    /// Other titles used to exercise platform-level code.
    pub test_titles: Vec<PathBuf>,
    /// The config file that was read, if any.
    pub source: Option<PathBuf>,
    /// Whether a default folder in use is upstream's old one
    /// ([`old_app_dir`]) because ours holds no data or cache yet.
    pub legacy: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    #[serde(default)]
    paths: FilePaths,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FilePaths {
    game_dir: Option<PathBuf>,
    fidelityfx_dir: Option<PathBuf>,
    cache_dir: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    #[serde(default)]
    test_titles: Vec<PathBuf>,
}

impl Config {
    /// Loads configuration from the environment and the config file.
    pub fn load() -> Result<Self> {
        let explicit = explicit_config_path(|k| std::env::var_os(k));
        let path = explicit.clone().unwrap_or_else(|| {
            let user = app_dir().ok().map(|d| d.join(CONFIG_FILE));
            config_candidates(user).into_iter().find(|p| p.is_file()).unwrap_or_else(|| PathBuf::from(CONFIG_FILE))
        });
        let file = if path.is_file() {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            Some((path, text))
        } else if let Some(p) = explicit {
            anyhow::bail!(
                "VICTORY_CONFIG points at {}, which does not exist",
                p.display()
            );
        } else {
            None
        };
        let (source, parsed) = match file {
            Some((path, text)) => {
                let parsed: FileConfig =
                    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
                (Some(path), parsed)
            }
            None => (None, FileConfig::default()),
        };
        Self::resolve(parsed.paths, source, |k| std::env::var_os(k))
    }

    fn resolve(
        file: FilePaths,
        source: Option<PathBuf>,
        env: impl Fn(&str) -> Option<std::ffi::OsString>,
    ) -> Result<Self> {
        let env_path = |k: &str| env(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let test_titles = match env("VICTORY_TEST_TITLES") {
            Some(v) if !v.is_empty() => std::env::split_paths(&v)
                .filter(|p| !p.as_os_str().is_empty())
                .collect(),
            _ => file.test_titles,
        };
        let cache_dir = env_path("VICTORY_CACHE_DIR").or(file.cache_dir);
        let data_dir = env_path("VICTORY_DATA_DIR").or(file.data_dir);
        let mut legacy = false;
        let mut default = |name: &str| -> Result<PathBuf> {
            let (root, old) = default_root(&app_dir()?, &old_app_dir()?);
            legacy |= old;
            Ok(root.join(name))
        };
        let cache_dir = cache_dir.map_or_else(|| default("cache"), Ok)?;
        let data_dir = data_dir.map_or_else(|| default("data"), Ok)?;
        Ok(Config {
            legacy,
            game_dir: env_path("VICTORY_GAME_DIR").or(file.game_dir),
            fidelityfx_dir: env_path("VICTORY_FIDELITYFX_DIR").or(file.fidelityfx_dir),
            cache_dir,
            data_dir,
            test_titles,
            source,
        })
    }

    /// The game directory, or a helpful error if it is not configured.
    pub fn require_game_dir(&self) -> Result<&Path> {
        self.game_dir.as_deref().context(
            "no game directory configured: set VICTORY_GAME_DIR or [paths].game_dir in \
             victory.toml (see victory.example.toml)",
        )
    }

    /// Returns `<cache>/<sub>`, creating it, after checking the asset policy.
    pub fn cache_subdir(&self, sub: &str) -> Result<PathBuf> {
        subdir(&self.cache_dir, sub)
    }

    /// Returns `<data>/<sub>`, creating it, after checking the asset policy.
    pub fn data_subdir(&self, sub: &str) -> Result<PathBuf> {
        subdir(&self.data_dir, sub)
    }
}

/// The config file explicitly named by the environment, if any. An empty
/// `VICTORY_CONFIG` means unset, like the other path variables.
fn explicit_config_path(env: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    env("VICTORY_CONFIG")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn subdir(root: &Path, sub: &str) -> Result<PathBuf> {
    let dir = root.join(sub);
    ensure_outside_repo(&dir)?;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

/// The config file looked for first, in the working folder.
pub const CONFIG_FILE: &str = "pilotebox.toml";

/// Upstream's config file, looked for next.
const OLD_CONFIG_FILE: &str = "victory.toml";

/// The config files looked for when `VICTORY_CONFIG` names none, in order:
/// ours, then upstream's, in the working folder, then ours in the user's
/// folder (`user`).
fn config_candidates(user: Option<PathBuf>) -> Vec<PathBuf> {
    [PathBuf::from(CONFIG_FILE), PathBuf::from(OLD_CONFIG_FILE)].into_iter().chain(user).collect()
}

/// `%LOCALAPPDATA%` (elsewhere the XDG or home cache).
fn user_base() -> Result<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_CACHE_HOME"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .context("cannot determine a user directory; set VICTORY_CACHE_DIR and VICTORY_DATA_DIR")
}

/// The product's own folder: `%LOCALAPPDATA%\PiloteBox`.
pub fn app_dir() -> Result<PathBuf> {
    Ok(user_base()?.join("PiloteBox"))
}

/// Upstream's folder: `%LOCALAPPDATA%\victory`.
pub fn old_app_dir() -> Result<PathBuf> {
    Ok(user_base()?.join("victory"))
}

/// The folder the default data and cache folders are in, and whether it is
/// the old one: ours, unless it holds neither and the old one is there.
pub fn default_root(ours: &Path, old: &Path) -> (PathBuf, bool) {
    let in_use = |root: &Path| root.join("data").exists() || root.join("cache").exists();
    if !in_use(ours) && old.is_dir() { (old.to_owned(), true) } else { (ours.to_owned(), false) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn environment_overrides_file() {
        let file = FilePaths {
            game_dir: Some("from-file".into()),
            cache_dir: Some("cache-from-file".into()),
            ..Default::default()
        };
        let env = |k: &str| (k == "VICTORY_GAME_DIR").then(|| OsString::from("from-env"));
        let cfg = Config::resolve(file, None, env).unwrap();
        assert_eq!(cfg.game_dir.unwrap(), PathBuf::from("from-env"));
        assert_eq!(cfg.cache_dir, PathBuf::from("cache-from-file"));
    }

    #[test]
    fn empty_env_values_are_ignored() {
        let file = FilePaths {
            game_dir: Some("from-file".into()),
            ..Default::default()
        };
        let env = |k: &str| (k == "VICTORY_GAME_DIR").then(OsString::new);
        let cfg = Config::resolve(file, None, env).unwrap();
        assert_eq!(cfg.game_dir.unwrap(), PathBuf::from("from-file"));
    }

    #[test]
    fn empty_test_titles_env_is_ignored() {
        let file = FilePaths {
            test_titles: vec!["from-file".into()],
            ..Default::default()
        };
        let env = |k: &str| (k == "VICTORY_TEST_TITLES").then(OsString::new);
        let cfg = Config::resolve(file, None, env).unwrap();
        assert_eq!(cfg.test_titles, vec![PathBuf::from("from-file")]);
    }

    #[test]
    fn test_titles_env_splits_paths() {
        let value = std::env::join_paths(["first", "second"]).unwrap();
        let file = FilePaths::default();
        let env = |k: &str| (k == "VICTORY_TEST_TITLES").then(|| value.clone());
        let cfg = Config::resolve(file, None, env).unwrap();
        assert_eq!(
            cfg.test_titles,
            vec![PathBuf::from("first"), PathBuf::from("second")]
        );
    }

    #[test]
    fn empty_config_env_is_ignored() {
        let empty = |k: &str| (k == "VICTORY_CONFIG").then(OsString::new);
        assert_eq!(explicit_config_path(empty), None);
        let set = |k: &str| (k == "VICTORY_CONFIG").then(|| OsString::from("custom.toml"));
        assert_eq!(
            explicit_config_path(set),
            Some(PathBuf::from("custom.toml"))
        );
    }

    #[test]
    fn our_config_file_comes_before_upstreams() {
        let user = PathBuf::from("user").join(CONFIG_FILE);
        assert_eq!(
            config_candidates(Some(user.clone())),
            [PathBuf::from("pilotebox.toml"), PathBuf::from("victory.toml"), user]
        );
    }

    #[test]
    fn the_old_folder_is_used_until_ours_holds_something() {
        let base = tempfile::tempdir().unwrap();
        let (ours, old) = (base.path().join("PiloteBox"), base.path().join("victory"));
        // Neither: ours.
        assert_eq!(default_root(&ours, &old), (ours.clone(), false));
        // Only the old one: the old one, even with our config file there.
        std::fs::create_dir_all(old.join("data")).unwrap();
        std::fs::create_dir_all(&ours).unwrap();
        std::fs::write(ours.join(CONFIG_FILE), "").unwrap();
        assert_eq!(default_root(&ours, &old), (old.clone(), true));
        // Ours holds a data or cache folder: ours.
        std::fs::create_dir_all(ours.join("cache")).unwrap();
        assert_eq!(default_root(&ours, &old), (ours, false));
    }

    #[test]
    fn example_config_parses() {
        let text = include_str!("../../../victory.example.toml");
        let parsed: FileConfig = toml::from_str(text).unwrap();
        assert!(parsed.paths.game_dir.is_some());
    }

    #[test]
    fn cache_inside_repo_is_refused() {
        let inside = Path::new(env!("CARGO_MANIFEST_DIR")).join("cache");
        let cfg = Config {
            cache_dir: inside,
            ..Default::default()
        };
        assert!(cfg.cache_subdir("x").is_err());
    }

    #[test]
    fn data_inside_repo_is_refused() {
        let inside = Path::new(env!("CARGO_MANIFEST_DIR")).join("data");
        let cfg = Config {
            data_dir: inside,
            ..Default::default()
        };
        assert!(cfg.data_subdir("x").is_err());
    }
}
