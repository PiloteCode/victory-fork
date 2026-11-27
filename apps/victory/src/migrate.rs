//! `pilotebox migrate`: from upstream's folder (`%LOCALAPPDATA%\victory`)
//! to ours (`%LOCALAPPDATA%\PiloteBox`). The data folder (saves, settings)
//! is copied, each file's size checked, and the old one is never removed;
//! the cache, the benchmarks, the logs and the reports are moved. Without
//! `--yes` it only says what it would do. Nothing is moved by itself: until
//! this runs, the old folder stays in use.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::play::line;
use crate::sys::gb;

#[derive(clap::Args, Debug)]
pub(crate) struct Migrate {
    /// Copies and moves (without it, migrate only says what it would do).
    #[arg(long)]
    pub yes: bool,
}

/// What is moved whole: folders and files beside the data folder.
const MOVED: [&str; 5] = ["cache", "bench", "logs", "reports", "last-run.txt"];

/// What migrate would do.
#[derive(Debug, PartialEq)]
pub(crate) struct Plan {
    /// The data folder to copy, its files and bytes.
    pub data: Option<(PathBuf, PathBuf, u64, u64)>,
    /// What to move, from where to where, and its bytes.
    pub moves: Vec<(PathBuf, PathBuf, u64)>,
}

/// What migrating `old` to `new` would do; an error if it cannot be done
/// safely.
pub(crate) fn plan(old: &Path, new: &Path) -> Result<Plan> {
    if !old.is_dir() {
        bail!("nothing to migrate: {} is not there", old.display());
    }
    let mut plan = Plan { data: None, moves: Vec::new() };
    let data = old.join("data");
    if data.is_dir() {
        let to = new.join("data");
        if std::fs::read_dir(&to).is_ok_and(|mut d| d.next().is_some()) {
            bail!(
                "{} already holds files: migrate copies the data only into an empty folder, so that no save is \
                 overwritten (look at both, keep one, then run it again)",
                to.display()
            );
        }
        let (bytes, files) = crate::clean::size(&data);
        plan.data = Some((data, to, files, bytes));
    }
    for name in MOVED {
        let from = old.join(name);
        if !from.exists() {
            continue;
        }
        let to = new.join(name);
        if to.exists() {
            bail!("{} is there already: move or remove one of the two, then run it again", to.display());
        }
        plan.moves.push((from.clone(), to, crate::clean::size(&from).0));
    }
    Ok(plan)
}

/// Copies `from` into `to` (which must not hold files), checking each
/// file's size: files and bytes copied.
fn copy_checked(from: &Path, to: &Path) -> Result<(u64, u64)> {
    let (mut files, mut bytes) = (0, 0);
    std::fs::create_dir_all(to).with_context(|| format!("creating {}", to.display()))?;
    for entry in walkdir::WalkDir::new(from).min_depth(1) {
        let entry = entry?;
        let target = to.join(entry.path().strip_prefix(from)?);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&target)?;
            continue;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        let len = entry.metadata()?.len();
        std::fs::copy(entry.path(), &target).with_context(|| format!("copying {}", entry.path().display()))?;
        let copied = std::fs::metadata(&target)?.len();
        if copied != len {
            bail!(
                "the copy of {} has {copied} bytes, the original {len}: the old data folder is as it was; remove {} and \
                 run it again",
                entry.path().display(),
                to.display()
            );
        }
        files += 1;
        bytes += len;
    }
    Ok((files, bytes))
}

/// Does `plan`: the data copied and checked, the rest moved.
pub(crate) fn apply(plan: &Plan) -> Result<()> {
    if let Some((from, to, files, bytes)) = &plan.data {
        let copied = copy_checked(from, to)?;
        if copied != (*files, *bytes) {
            bail!(
                "{} files ({} bytes) copied of {files} ({bytes} bytes): the old data folder is as it was; remove {} \
                 and run it again",
                copied.0,
                copied.1,
                to.display()
            );
        }
    }
    for (from, to, _) in &plan.moves {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(from, to).with_context(|| {
            format!("moving {} to {} (close every run, then try again)", from.display(), to.display())
        })?;
    }
    Ok(())
}

pub(crate) fn run(m: Migrate) -> Result<()> {
    let old = victory_core::config::old_app_dir()?;
    let new = victory_core::config::app_dir()?;
    let cfg = victory_core::Config::load()?;
    println!("{}", victory_core::product::heading("migrate"));
    line("from", old.display());
    line("to", new.display());
    if !cfg.legacy {
        if cfg.data_dir.starts_with(&new) || cfg.cache_dir.starts_with(&new) {
            line("nothing", "the new folder is in use already");
            if old.is_dir() {
                line("old", format!("{} is still there: remove it yourself when you no longer need it", old.display()));
            }
        } else {
            line(
                "nothing",
                "your data and cache folders are set by the environment or the config file: migrate moves the \
                 default folders only",
            );
        }
        return Ok(());
    }
    let plan = plan(&old, &new)?;
    if let Some((_, to, files, bytes)) = &plan.data {
        line("copy", format!("data: {files} files, {} to {} (the old data folder stays as it is)", gb(*bytes), to.display()));
    }
    for (from, to, bytes) in &plan.moves {
        let name = from.file_name().unwrap_or_default().to_string_lossy();
        line("move", format!("{name}: {} to {}", gb(*bytes), to.display()));
    }
    if !m.yes {
        line("nothing", "changed yet: run it again with --yes");
        return Ok(());
    }
    let running = crate::sys::processes_named(&[&format!("{}.exe", victory_core::product::BINARY), "victory.exe"]);
    if !running.is_empty() {
        bail!("a run is going ({} processes): close it first, then migrate", running.len());
    }
    apply(&plan)?;
    line("done", "the data copied and checked, the rest moved");
    if let Some((from, ..)) = &plan.data {
        line(
            "old data",
            format!("{} stays: remove it yourself once you have played with the new one", from.display()),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn the_data_is_copied_and_kept_and_the_cache_moved() {
        let base = tempfile::tempdir().unwrap();
        let (old, new) = (base.path().join("victory"), base.path().join("PiloteBox"));
        file(&old.join("data/12AB34CD/saves/slot/blob"), "a save");
        file(&old.join("data/12AB34CD/crashes.log"), "old crash");
        file(&old.join("cache/shaders/ab/cd"), "translation");
        file(&old.join("bench/2026-11-27.md"), "figures");
        // Our config file may be there first: it does not stop the move.
        file(&new.join("pilotebox.toml"), "[paths]");
        let plan = plan(&old, &new).unwrap();
        assert_eq!(plan.data, Some((old.join("data"), new.join("data"), 2, 15)));
        assert_eq!(plan.moves.len(), 2);
        apply(&plan).unwrap();
        // Copied: both there, equal.
        assert_eq!(std::fs::read_to_string(new.join("data/12AB34CD/saves/slot/blob")).unwrap(), "a save");
        assert_eq!(std::fs::read_to_string(old.join("data/12AB34CD/saves/slot/blob")).unwrap(), "a save");
        // Moved.
        assert_eq!(std::fs::read_to_string(new.join("cache/shaders/ab/cd")).unwrap(), "translation");
        assert!(!old.join("cache").exists() && !old.join("bench").exists());
        // Ours is now the one in use.
        assert_eq!(victory_core::config::default_root(&new, &old), (new.clone(), false));
        // Again: the data is there now, so nothing is overwritten.
        file(&old.join("cache/x"), "again");
        let error = super::plan(&old, &new).unwrap_err().to_string();
        assert!(error.contains("already holds files"), "{error}");
    }

    #[test]
    fn nothing_to_migrate_is_said() {
        let base = tempfile::tempdir().unwrap();
        let error = plan(&base.path().join("victory"), &base.path().join("PiloteBox")).unwrap_err().to_string();
        assert!(error.starts_with("nothing to migrate"), "{error}");
    }
}
