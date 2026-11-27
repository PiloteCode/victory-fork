//! `pilotebox report`: one text file for a bug report. It holds doctor's
//! checks, the last play and the last benchmark, and the last 200 lines of
//! the last play's log and of the crash log. User names in paths become
//! `<user>`, and the title's folder `<title folder>`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::game::{self, Places};

/// Lines kept of each log.
const TAIL: usize = 200;

#[derive(clap::Args, Debug)]
pub(crate) struct Report {
    /// The title's executable, or its folder (default: found as `play`
    /// finds it).
    #[arg(long, value_name = "PATH")]
    pub game: Option<PathBuf>,
}

/// The last `n` lines of a file, or why there are none.
fn tail(path: &Path, n: usize) -> String {
    match std::fs::read(path) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            let lines: Vec<&str> = text.lines().collect();
            lines[lines.len().saturating_sub(n)..].join("\n")
        }
        Err(_) => "(none)".to_owned(),
    }
}

/// `text` with the user's name in paths replaced by `<user>`, and the
/// folders in `hidden` by their labels.
pub(crate) fn redact(text: &str, user: Option<&str>, hidden: &[(String, &str)]) -> String {
    let mut out = text.to_owned();
    for (path, label) in hidden {
        if !path.is_empty() {
            out = replace_ignoring_case(&out, path, label);
        }
    }
    // `\Users\<name>\` and `/Users/<name>/`, whoever the name is.
    for sep in ['\\', '/'] {
        let marker = format!("{sep}Users{sep}");
        let mut done = String::with_capacity(out.len());
        let mut rest = out.as_str();
        while let Some(at) = find_ignoring_case(rest, &marker) {
            let after = at + marker.len();
            done.push_str(&rest[..after]);
            let name_len = rest[after..].find(|c: char| c == sep || c.is_whitespace() || c == '"').unwrap_or(rest.len() - after);
            done.push_str(if name_len > 0 { "<user>" } else { "" });
            rest = &rest[after + name_len..];
        }
        done.push_str(rest);
        out = done;
    }
    if let Some(user) = user.filter(|u| u.len() >= 3) {
        out = replace_ignoring_case(&out, user, "<user>");
    }
    out
}

fn find_ignoring_case(text: &str, what: &str) -> Option<usize> {
    text.to_ascii_lowercase().find(&what.to_ascii_lowercase())
}

fn replace_ignoring_case(text: &str, what: &str, with: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = find_ignoring_case(rest, what) {
        out.push_str(&rest[..at]);
        out.push_str(with);
        rest = &rest[at + what.len()..];
    }
    out.push_str(rest);
    out
}

pub(crate) fn run(r: Report) -> Result<()> {
    let cfg = victory_core::Config::load()?;
    let found = game::find(&Places::new(r.game.as_deref(), &cfg)).ok();
    let mut text = format!("{}, {}\n\n", victory_core::product::heading("bug report"), crate::state::local_text());
    text.push_str("== doctor\n");
    for c in crate::doctor::checks(r.game.as_deref()) {
        text.push_str(&format!("{:<8} {:<11}{}\n", c.level.word(), c.name, c.text));
    }
    text.push_str("\n== the last play\n");
    text.push_str(&crate::state::last_run(&cfg).map_or_else(|| "(none recorded)".to_owned(), |run| run.line()));
    text.push_str("\n\n== the last benchmark\n");
    let bench = crate::state::newest_bench(&crate::bench::reports_dir(&cfg));
    match bench.as_ref().and_then(|b| std::fs::read_to_string(b.with_extension("md")).ok()) {
        Some(md) => text.push_str(&md),
        None => text.push_str("(none)\n"),
    }
    let (log, _) = crate::state::play_logs(&cfg);
    text.push_str(&format!("\n== the last play's log, its last {TAIL} lines ({})\n", log.display()));
    text.push_str(&tail(&log, TAIL));
    let crash_log = found.as_ref().map(|f| game::crash_log(&cfg.data_dir, &f.exe));
    let crash_name = crash_log.as_ref().map_or_else(|| "not known: the title was not found".to_owned(), |p| p.display().to_string());
    text.push_str(&format!("\n\n== the crash log, its last {TAIL} lines ({crash_name})\n"));
    text.push_str(&crash_log.map_or_else(|| "(none)".to_owned(), |p| tail(&p, TAIL)));
    text.push('\n');

    let title_folder = found
        .as_ref()
        .and_then(|f| f.exe.parent())
        .map(|d| d.display().to_string())
        .unwrap_or_default();
    let exe_name = found
        .as_ref()
        .and_then(|f| f.exe.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let hidden = [(title_folder, "<title folder>"), (exe_name, "<title>.exe")];
    let user = std::env::var("USERNAME").ok();
    let text = redact(&text, user.as_deref(), &hidden);

    let dir = crate::state::beside_cache(&cfg, "reports");
    victory_core::ensure_outside_repo(&dir)?;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let [y, mo, d, h, mi, s] = crate::sys::local_time();
    let path = dir.join(format!("report-{y:04}-{mo:02}-{d:02}_{h:02}{mi:02}{s:02}.txt"));
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    println!("{}", victory_core::product::heading("report"));
    crate::play::line("written", path.display());
    crate::play::line(
        "next",
        "read it, then attach it to an issue: user names and the title's folder are taken out, nothing else is",
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_names_and_the_title_folder_are_taken_out() {
        let hidden = [(r"D:\Games\Title Folder".to_owned(), "<title folder>"), ("title_main.exe".to_owned(), "<title>.exe")];
        let text = r"log C:\Users\Some.One\AppData\Local\PiloteBox\cache and c:/users/Some.One/x
game d:\games\title folder\title_main.exe; someone said Some.One";
        assert_eq!(
            redact(text, Some("Some.One"), &hidden),
            r"log C:\Users\<user>\AppData\Local\PiloteBox\cache and c:/users/<user>/x
game <title folder>\<title>.exe; someone said <user>"
        );
        assert_eq!(redact(r"C:\Users\", None, &[]), r"C:\Users\");
    }

    #[test]
    fn a_tail_keeps_the_last_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log");
        std::fs::write(&path, "1\n2\n3\n4\n").unwrap();
        assert_eq!(tail(&path, 2), "3\n4");
        assert_eq!(tail(&path, 10), "1\n2\n3\n4");
        assert_eq!(tail(&dir.path().join("none"), 2), "(none)");
    }
}
