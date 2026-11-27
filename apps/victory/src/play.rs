//! `pilotebox play`: the game started with no path to type. It is `run
//! <the title> --execute` with the title found (`game`), the console profile
//! by name and the play defaults (`defaults`), after one short block saying
//! what it plays and where its files are.

use std::path::PathBuf;

use anyhow::Result;
use victory_core::product;

use crate::game::{self, Places, Profile};

#[derive(clap::Args, Debug)]
pub(crate) struct Play {
    #[command(flatten)]
    pub profile: Profile,
    /// (For PiloteBox.cmd: prints where this play's crash log is, and stops.)
    #[arg(long, hide = true)]
    pub crash_log: bool,
    /// Arguments for the title, after `--` (as `run` passes them).
    #[arg(last = true)]
    pub title_args: Vec<String>,
}

/// One line of a block: a label, then its value.
pub(crate) fn line(label: &str, value: impl std::fmt::Display) {
    println!("  {label:<11}{value}");
}

/// The line saying the old folders are in use, if they are.
pub(crate) fn legacy_note(cfg: &victory_core::Config) -> Option<String> {
    cfg.legacy.then(|| {
        let old = victory_core::config::old_app_dir().map_or_else(|_| "the old folder".to_owned(), |d| d.display().to_string());
        format!("{old} is used, the old folder (pilotebox migrate moves it to %LOCALAPPDATA%\\PiloteBox)")
    })
}

/// The switches set in the environment now, by the names the user knows
/// (`PILOTEBOX_X` for `VICTORY_X`), as `NAME=value`.
pub(crate) fn environment_switches() -> Vec<String> {
    let mut set: Vec<String> = std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v)))
        .filter(|(k, _)| k.starts_with(product::UPSTREAM_PREFIX) || k.starts_with(product::PREFIX))
        .map(|(k, v)| format!("{}={}", product::shown(&k), v.to_string_lossy()))
        .collect();
    set.sort();
    set.dedup();
    set
}

/// Sets `switches` in the environment. Before any thread starts.
pub(crate) fn set_switches(switches: &[(&str, &str, &str)]) {
    for (name, value, _) in switches {
        // SAFETY: `main` has started no thread yet.
        unsafe { std::env::set_var(name, value) };
    }
}

/// What the switches line says: ours, then the play defaults.
pub(crate) fn switches_text(ours: &[(&str, &str, &str)], defaults: usize) -> String {
    let mut parts: Vec<String> = ours.iter().map(|(n, v, why)| format!("{}={v} ({why})", product::shown(n))).collect();
    parts.push(match defaults {
        0 => "no play defaults (PILOTEBOX_PLAY_DEFAULTS=0, or all set already)".to_owned(),
        n => format!("{n} play defaults (PILOTEBOX_PLAY_DEFAULTS=0: none)"),
    });
    parts.join("; ")
}

/// What `--crash-log` says (PiloteBox.cmd prints it after a failed run).
fn crash_log_line(crash_log: &std::path::Path) -> String {
    if crash_log.is_file() {
        format!("The crash log is {}", crash_log.display())
    } else {
        format!("No crash log was written ({}): see the lines above.", crash_log.display())
    }
}

pub(crate) fn run(play: Play, log: Option<PathBuf>) -> Result<()> {
    let cfg = victory_core::Config::load()?;
    let found = game::find(&Places::new(play.profile.game.as_deref(), &cfg))?;
    let crash_log: PathBuf = game::crash_log(&cfg.data_dir, &found.exe);
    if play.crash_log {
        println!("{}", crash_log_line(&crash_log));
        return Ok(());
    }
    let before = environment_switches();
    let ours = play.profile.switches();
    set_switches(&ours);
    let defaults = crate::defaults::set();
    println!("{}", product::banner());
    line("mode", play.profile.mode.name());
    line("game", format!("{} ({})", found.exe.display(), found.from));
    for note in found.notes.iter().cloned().chain(legacy_note(&cfg)) {
        line("note", note);
    }
    line("data", cfg.data_dir.display());
    line("cache", cfg.cache_dir.display());
    line("crash log", crash_log.display());
    if let Some(log) = &log {
        line("log", log.display());
    }
    line("switches", switches_text(&ours, defaults.len()));
    let theirs: Vec<&str> = before
        .iter()
        .filter(|s| !ours.iter().any(|(n, _, _)| s.starts_with(&format!("{}=", product::shown(n)))))
        .map(String::as_str)
        .collect();
    if !theirs.is_empty() {
        line("also set", format!("{} (in the environment; the VICTORY_ names work too)", theirs.join(" ")));
    }
    crate::state::note_run(
        &cfg,
        &crate::state::LastRun {
            started: crate::state::unix_now(),
            when: crate::state::local_text(),
            mode: play.profile.label(),
            crash_log: crash_log.clone(),
        },
    );
    victory_hle::runtime::set_title_args(play.title_args);
    crate::run::run(&found.exe, crate::Unimplemented::Trap, false, true, play.profile.mode.console())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_switches_line_names_who_set_what() {
        let ours = [("VICTORY_BVH_BUILDER", "1", "--rt")];
        assert_eq!(
            switches_text(&ours, 17),
            "PILOTEBOX_BVH_BUILDER=1 (--rt); 17 play defaults (PILOTEBOX_PLAY_DEFAULTS=0: none)"
        );
        assert!(switches_text(&[], 0).starts_with("no play defaults"));
    }

    #[test]
    fn the_old_folders_are_named_when_used() {
        let cfg = victory_core::Config::default();
        assert_eq!(legacy_note(&cfg), None);
        let old = victory_core::Config { legacy: true, ..Default::default() };
        assert!(legacy_note(&old).unwrap().contains("pilotebox migrate"));
    }
}
