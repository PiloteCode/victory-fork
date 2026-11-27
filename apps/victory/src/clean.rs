//! `pilotebox clean`: what in the cache folder is old, with sizes, removed
//! only with `--yes`. Old is: a file of the cache's parts not read for 14
//! days, a folder set aside beside a part (`pipelines-vague10`), the
//! layer's crash dumps, and data copies `bench` left behind in the
//! temporary folder. `--all` takes the cache's parts whole (every run after
//! makes them again: shaders in minutes, pipelines while it plays). The
//! data folder (saves, settings) is never touched.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Result, bail};

use crate::play::line;
use crate::sys::gb;

/// The parts of the cache folder the layer keeps.
pub(crate) const PARTS: [&str; 3] = ["shaders", "pipelines", "images"];

/// Days a cache file may go unread before it is old.
const DAYS: u64 = 14;

/// Age from which a data copy of `bench` is taken as left behind.
const COPY_AGE: Duration = Duration::from_secs(24 * 3600);

#[derive(clap::Args, Debug)]
pub(crate) struct Clean {
    /// The cache's parts whole, not only their old files.
    #[arg(long)]
    pub all: bool,
    /// Removes what is listed (without it, clean only lists).
    #[arg(long)]
    pub yes: bool,
}

/// Something to remove: a folder whole, or files.
#[derive(Debug, PartialEq)]
pub(crate) enum Target {
    Folder(PathBuf),
    Files(Vec<PathBuf>),
}

#[derive(Debug, PartialEq)]
pub(crate) struct Item {
    pub what: String,
    pub bytes: u64,
    pub target: Target,
}

/// Bytes and files under `path` (0 if it is not there).
pub(crate) fn size(path: &Path) -> (u64, u64) {
    let mut total = (0, 0);
    for entry in walkdir::WalkDir::new(path).into_iter().filter_map(Result::ok) {
        if entry.file_type().is_file() {
            total.0 += entry.metadata().map_or(0, |m| m.len());
            total.1 += 1;
        }
    }
    total
}

/// The folders of `cache` set aside beside one of its parts: named after a
/// part and something more (`pipelines-vague10`, `shaders.old`).
pub(crate) fn set_aside(cache: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(cache) else { return Vec::new() };
    let mut out: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_ascii_lowercase();
            PARTS.iter().any(|part| {
                name.strip_prefix(part)
                    .and_then(|rest| rest.chars().next())
                    .is_some_and(|c| !c.is_ascii_alphanumeric())
            })
        })
        .map(|e| e.path())
        .collect();
    out.sort();
    out
}

/// When a file was last read or written.
fn last_used(meta: &std::fs::Metadata) -> Option<SystemTime> {
    [meta.accessed().ok(), meta.modified().ok()].into_iter().flatten().max()
}

/// What is old in `cache` at `now` (and, in `temp`, data copies `bench`
/// left), never anything inside `data`.
pub(crate) fn old(cache: &Path, temp: &Path, data: &Path, all: bool, now: SystemTime) -> Vec<Item> {
    let unread = Duration::from_secs(DAYS * 24 * 3600);
    let is_old = |meta: &std::fs::Metadata, age: Duration| {
        last_used(meta).is_some_and(|t| now.duration_since(t).is_ok_and(|d| d >= age))
    };
    let mut items = Vec::new();
    for part in PARTS {
        let dir = cache.join(part);
        if !dir.is_dir() {
            continue;
        }
        if all {
            let (bytes, files) = size(&dir);
            items.push(Item {
                what: format!("{part}: all of it ({files} files, --all)"),
                bytes,
                target: Target::Folder(dir),
            });
            continue;
        }
        let mut files = Vec::new();
        let mut bytes = 0;
        for entry in walkdir::WalkDir::new(&dir).into_iter().filter_map(Result::ok) {
            if let Ok(meta) = entry.metadata()
                && meta.is_file()
                && is_old(&meta, unread)
            {
                bytes += meta.len();
                files.push(entry.into_path());
            }
        }
        if !files.is_empty() {
            items.push(Item {
                what: format!("{part}: {} files not read for {DAYS} days", files.len()),
                bytes,
                target: Target::Files(files),
            });
        }
    }
    for dir in set_aside(cache) {
        let name = dir.file_name().unwrap_or_default().to_string_lossy().into_owned();
        items.push(Item {
            what: format!("{name}: set aside"),
            bytes: size(&dir).0,
            target: Target::Folder(dir),
        });
    }
    // Crash dumps of diagnostics builds (`VICTORY_CRASH_SAVE`).
    let dumps: Vec<(PathBuf, u64)> = std::fs::read_dir(cache)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.starts_with("crash-") && name.ends_with(".bin")
        })
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            (meta.is_file() && (all || is_old(&meta, unread))).then(|| (e.path(), meta.len()))
        })
        .collect();
    if !dumps.is_empty() {
        items.push(Item {
            what: format!("{} crash dumps", dumps.len()),
            bytes: dumps.iter().map(|(_, n)| n).sum(),
            target: Target::Files(dumps.into_iter().map(|(p, _)| p).collect()),
        });
    }
    for entry in std::fs::read_dir(temp).into_iter().flatten().filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(crate::bench::COPY_PREFIX)
            && let Ok(meta) = entry.metadata()
            && meta.is_dir()
            && meta.modified().is_ok_and(|t| now.duration_since(t).is_ok_and(|d| d >= COPY_AGE))
        {
            items.push(Item {
                what: format!("{name}: a data copy a bench left behind"),
                bytes: size(&entry.path()).0,
                target: Target::Folder(entry.path()),
            });
        }
    }
    // Never the data folder, whatever the paths say.
    let data = std::fs::canonicalize(data).unwrap_or_else(|_| data.to_owned());
    let inside = |p: &Path| std::fs::canonicalize(p).is_ok_and(|p| p.starts_with(&data) || data.starts_with(&p));
    items.retain(|item| match &item.target {
        Target::Folder(dir) => !inside(dir),
        Target::Files(files) => !files.iter().any(|f| inside(f)),
    });
    items
}

/// Removes `item`: bytes removed.
fn remove(item: &Item) -> u64 {
    match &item.target {
        Target::Folder(dir) => {
            if std::fs::remove_dir_all(dir).is_ok() {
                item.bytes
            } else {
                0
            }
        }
        Target::Files(files) => files
            .iter()
            .filter_map(|f| {
                let len = std::fs::metadata(f).ok()?.len();
                std::fs::remove_file(f).ok().map(|_| len)
            })
            .sum(),
    }
}

pub(crate) fn run(c: Clean) -> Result<()> {
    let cfg = victory_core::Config::load()?;
    let data = std::fs::canonicalize(&cfg.data_dir).unwrap_or_else(|_| cfg.data_dir.clone());
    let cache = std::fs::canonicalize(&cfg.cache_dir).unwrap_or_else(|_| cfg.cache_dir.clone());
    if cache.starts_with(&data) || data.starts_with(&cache) {
        bail!(
            "the cache folder ({}) and the data folder ({}) hold one another: clean does nothing, so that no save is lost",
            cfg.cache_dir.display(),
            cfg.data_dir.display()
        );
    }
    println!("{} clean", victory_core::product::banner());
    line("cache", format!("{} (the data folder, {}, is never touched)", cfg.cache_dir.display(), cfg.data_dir.display()));
    let items = old(&cfg.cache_dir, &std::env::temp_dir(), &cfg.data_dir, c.all, SystemTime::now());
    if items.is_empty() {
        line("old", "nothing");
        return Ok(());
    }
    for item in &items {
        line("old", format!("{:>9}  {}", gb(item.bytes), item.what));
    }
    let total: u64 = items.iter().map(|i| i.bytes).sum();
    if !c.yes {
        line(
            "total",
            format!("{} in {} places; nothing removed: run it again with --yes to remove them", gb(total), items.len()),
        );
        return Ok(());
    }
    let removed: u64 = items.iter().map(remove).sum();
    line("removed", gb(removed));
    if removed < total {
        line("left", format!("{}: in use, or not ours to remove", gb(total - removed)));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &Path, bytes: usize, age_days: u64) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![7u8; bytes]).unwrap();
        let then = SystemTime::now() - Duration::from_secs(age_days * 24 * 3600);
        let f = std::fs::File::options().write(true).open(path).unwrap();
        f.set_times(std::fs::FileTimes::new().set_accessed(then).set_modified(then)).unwrap();
    }

    #[test]
    fn old_files_and_set_aside_folders_are_listed() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("cache");
        let data = root.path().join("data");
        let temp = root.path().join("temp");
        file(&cache.join("shaders/ab/old"), 100, 30);
        file(&cache.join("shaders/ab/new"), 10, 1);
        file(&cache.join("pipelines/kept"), 10, 1);
        file(&cache.join("pipelines-vague10/x"), 50, 0);
        file(&cache.join("notes/mine"), 5, 90);
        file(&cache.join("crash-1234.bin"), 3, 20);
        file(&data.join("12AB34CD/saves/slot"), 9, 400);
        std::fs::create_dir_all(temp.join("pilotebox-bench-1-2")).unwrap();
        let now = SystemTime::now();
        let items = old(&cache, &temp, &data, false, now);
        let what: Vec<&str> = items.iter().map(|i| i.what.as_str()).collect();
        assert_eq!(
            what,
            ["shaders: 1 files not read for 14 days", "pipelines-vague10: set aside", "1 crash dumps"]
        );
        assert_eq!(items[0].bytes, 100);
        assert_eq!(items[0].target, Target::Files(vec![cache.join("shaders/ab/old")]));
        // A day later the bench's copy is old too.
        let later = old(&cache, &temp, &data, false, now + Duration::from_secs(2 * 24 * 3600));
        assert!(later.iter().any(|i| i.what.starts_with("pilotebox-bench-1-2")));
        // --all: the parts whole.
        let all = old(&cache, &temp, &data, true, now);
        assert!(all.iter().any(|i| i.target == Target::Folder(cache.join("pipelines"))));
        // Removing them leaves the rest, and the data folder, as they were.
        for item in &items {
            remove(item);
        }
        assert!(!cache.join("shaders/ab/old").exists() && !cache.join("pipelines-vague10").exists());
        assert!(cache.join("shaders/ab/new").exists() && cache.join("notes/mine").exists());
        assert!(data.join("12AB34CD/saves/slot").exists());
    }

    #[test]
    fn nothing_inside_the_data_folder_is_ever_listed() {
        let root = tempfile::tempdir().unwrap();
        // A cache inside the data folder (a wrong config): nothing listed.
        let data = root.path().join("data");
        let cache = data.join("cache");
        file(&cache.join("shaders/old"), 10, 30);
        file(&cache.join("shaders-x/y"), 10, 30);
        assert!(old(&cache, root.path(), &data, true, SystemTime::now()).is_empty());
    }

    #[test]
    fn set_aside_folders_are_named_after_a_part() {
        let root = tempfile::tempdir().unwrap();
        for dir in ["shaders", "shaders.old", "pipelines-vague10", "imagesx", "bench", "Images_2"] {
            std::fs::create_dir_all(root.path().join(dir)).unwrap();
        }
        let names: Vec<String> =
            set_aside(root.path()).iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["Images_2", "pipelines-vague10", "shaders.old"]);
    }
}
