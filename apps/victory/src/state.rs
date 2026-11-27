//! What the commands for playing keep beside the cache folder, so that a
//! bare `pilotebox`, `report` and the next run can say what happened: the
//! benchmarks (`bench`), the last plays' logs (`logs`), the bug reports
//! (`reports`) and the last play's start (`last-run.txt`).

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// `name` beside the cache folder (`%LOCALAPPDATA%\PiloteBox\<name>` by
/// default; never inside the data folder).
pub(crate) fn beside_cache(cfg: &victory_core::Config, name: &str) -> PathBuf {
    cfg.cache_dir.with_file_name(name)
}

/// The log of the last `play`, and of the one before.
pub(crate) fn play_logs(cfg: &victory_core::Config) -> (PathBuf, PathBuf) {
    let logs = beside_cache(cfg, "logs");
    (logs.join("play-last.log"), logs.join("play-previous.log"))
}

/// Keeps the last play's log as the previous one; returns the path the new
/// one goes to.
pub(crate) fn rotate_play_log(cfg: &victory_core::Config) -> Option<PathBuf> {
    let (last, previous) = play_logs(cfg);
    victory_core::ensure_outside_repo(&last).ok()?;
    if last.is_file() {
        let _ = std::fs::rename(&last, &previous);
    }
    Some(last)
}

/// Seconds since 1970.
pub(crate) fn unix_now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

/// The local time as text: `2026-11-27 15:30:12`.
pub(crate) fn local_text() -> String {
    let [y, mo, d, h, mi, s] = crate::sys::local_time();
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}

/// The last play's start, as `play` wrote it.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct LastRun {
    pub started: f64,
    pub when: String,
    pub mode: String,
    pub crash_log: PathBuf,
}

impl LastRun {
    fn text(&self) -> String {
        format!(
            "started={}\nwhen={}\nmode={}\ncrash_log={}\n",
            self.started,
            self.when,
            self.mode,
            self.crash_log.display()
        )
    }

    fn parse(text: &str) -> Option<LastRun> {
        let get = |key: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
                .map(str::trim)
                .map(str::to_owned)
        };
        Some(LastRun {
            started: get("started")?.parse().ok()?,
            when: get("when").unwrap_or_default(),
            mode: get("mode").unwrap_or_default(),
            crash_log: PathBuf::from(get("crash_log").unwrap_or_default()),
        })
    }

    /// Whether the crash log was written after this run started.
    pub(crate) fn crashed(&self) -> bool {
        std::fs::metadata(&self.crash_log)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .is_some_and(|t| t.as_secs_f64() >= self.started)
    }

    /// One line on it.
    pub(crate) fn line(&self) -> String {
        let how = if self.crashed() {
            format!("a crash was written to {}", self.crash_log.display())
        } else {
            "no crash written".to_owned()
        };
        format!("{}, {}: {how}", self.when, self.mode)
    }
}

pub(crate) fn last_run_file(cfg: &victory_core::Config) -> PathBuf {
    beside_cache(cfg, "last-run.txt")
}

/// Notes a play's start (best effort).
pub(crate) fn note_run(cfg: &victory_core::Config, run: &LastRun) {
    let file = last_run_file(cfg);
    if victory_core::ensure_outside_repo(&file).is_ok() {
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(file, run.text());
    }
}

pub(crate) fn last_run(cfg: &victory_core::Config) -> Option<LastRun> {
    LastRun::parse(&std::fs::read_to_string(last_run_file(cfg)).ok()?)
}

/// The newest benchmark's JSON report in `dir`.
pub(crate) fn newest_bench(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .max_by_key(|p| p.file_name().map(|n| n.to_owned()))
}

/// A string field of a flat JSON object written by `bench`.
pub(crate) fn json_field(json: &str, key: &str) -> Option<String> {
    let at = json.find(&format!("\"{key}\": \""))? + key.len() + 5;
    let mut out = String::new();
    let mut chars = json[at..].chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                }
                c => out.push(c),
            },
            c => out.push(c),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_run_is_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = victory_core::Config {
            cache_dir: dir.path().join("cache"),
            ..Default::default()
        };
        assert_eq!(last_run(&cfg), None);
        let run = LastRun {
            started: unix_now() + 3600.0,
            when: "2026-11-27 15:30:12".into(),
            mode: "Series X".into(),
            crash_log: dir.path().join("crashes.log"),
        };
        note_run(&cfg, &run);
        assert_eq!(last_run_file(&cfg), dir.path().join("last-run.txt"));
        let read = last_run(&cfg).unwrap();
        assert_eq!(read, run);
        // A crash log older than the start is not this run's.
        std::fs::write(&run.crash_log, "x").unwrap();
        assert!(!read.crashed());
        let earlier = LastRun { started: 0.0, ..read };
        assert!(earlier.crashed());
        assert!(earlier.line().starts_with("2026-11-27 15:30:12, Series X: a crash was written to "));
    }

    #[test]
    fn the_newest_bench_and_its_line_are_found() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(newest_bench(dir.path()), None);
        for (name, line) in [("2026-11-26_101010-a.json", "old"), ("2026-11-27_090000-b.json", "new \\\"one\\\"")] {
            std::fs::write(dir.path().join(name), format!("{{\n  \"line\": \"{line}\",\n  \"x\": 1\n}}\n")).unwrap();
        }
        std::fs::write(dir.path().join("2026-11-28_000000-c.md"), "").unwrap();
        let newest = newest_bench(dir.path()).unwrap();
        assert!(newest.ends_with("2026-11-27_090000-b.json"));
        let json = std::fs::read_to_string(newest).unwrap();
        assert_eq!(json_field(&json, "line").as_deref(), Some("new \"one\""));
        assert_eq!(json_field(&json, "absent"), None);
    }
}
