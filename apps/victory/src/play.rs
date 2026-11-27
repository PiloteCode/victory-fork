//! `pilotebox play`: the game started with no path to type. It is `run
//! <the game> --execute` with the game found (`game`), the console profile
//! by name and the play defaults (`defaults`), after one short block saying
//! what it plays and where its files are.

use std::path::PathBuf;

use anyhow::Result;

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

/// The `VICTORY_*` switches set in the environment now, as `NAME=value`.
pub(crate) fn environment_switches() -> Vec<String> {
    let mut set: Vec<String> = std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v)))
        .filter(|(k, _)| k.starts_with("VICTORY_"))
        .map(|(k, v)| format!("{k}={}", v.to_string_lossy()))
        .collect();
    set.sort();
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
    let mut parts: Vec<String> = ours.iter().map(|(n, v, why)| format!("{n}={v} ({why})")).collect();
    parts.push(match defaults {
        0 => "no play defaults (VICTORY_PLAY_DEFAULTS=0, or all set already)".to_owned(),
        n => format!("{n} play defaults (VICTORY_PLAY_DEFAULTS=0: none)"),
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

pub(crate) fn run(play: Play) -> Result<()> {
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
    println!("{}", victory_core::product::banner());
    line("mode", play.profile.mode.name());
    line("game", format!("{} ({})", found.exe.display(), found.from));
    for note in &found.notes {
        line("note", note);
    }
    line("data", cfg.data_dir.display());
    line("cache", cfg.cache_dir.display());
    line("crash log", crash_log.display());
    line("switches", switches_text(&ours, defaults.len()));
    let theirs: Vec<&String> = before
        .iter()
        .filter(|s| !ours.iter().any(|(n, _, _)| s.starts_with(&format!("{n}="))))
        .collect();
    if !theirs.is_empty() {
        let theirs: Vec<&str> = theirs.iter().map(|s| s.as_str()).collect();
        line("also set", format!("{} (in the environment)", theirs.join(" ")));
    }
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
            "VICTORY_BVH_BUILDER=1 (--rt); 17 play defaults (VICTORY_PLAY_DEFAULTS=0: none)"
        );
        assert!(switches_text(&[], 0).starts_with("no play defaults"));
    }
}
