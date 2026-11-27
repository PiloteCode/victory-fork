//! `pilotebox setup`: the first run in one command. It finds the title (or
//! takes `--game`), writes `pilotebox.toml` so that every later command
//! finds it from anywhere, runs `doctor`, translates the title's shaders
//! into the cache ahead (the `shaders` command, with its count), and says
//! what to do next.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::doctor::{self, Level};
use crate::game::{self, Places};
use crate::play::line;

#[derive(clap::Args, Debug)]
pub(crate) struct Setup {
    /// The title's executable, or its folder (default: found as `play`
    /// finds it).
    #[arg(long, value_name = "PATH")]
    pub game: Option<PathBuf>,
    /// Leaves the shaders to the first run (which then stutters longer).
    #[arg(long)]
    pub no_shaders: bool,
    /// Threads translating the shaders (default: every logical core).
    #[arg(long, value_name = "N")]
    pub jobs: Option<usize>,
}

/// A config file naming `game_dir`.
pub(crate) fn config_text(game_dir: &Path) -> String {
    let path = game_dir.display().to_string().replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        "# PiloteBox's settings, written by `pilotebox setup`. The environment\n\
         # (PILOTEBOX_GAME_DIR, PILOTEBOX_DATA_DIR, PILOTEBOX_CACHE_DIR) takes\n\
         # precedence; victory.example.toml in the repository names every key.\n\
         [paths]\n\
         game_dir = \"{path}\"\n"
    )
}

/// Where the config goes when none is in use: the product's own folder,
/// read from any working folder.
fn user_config() -> Result<PathBuf> {
    Ok(victory_core::config::app_dir()?.join(victory_core::config::CONFIG_FILE))
}

pub(crate) fn run(s: Setup) -> Result<()> {
    println!("{}", victory_core::product::heading("setup"));
    let cfg = victory_core::Config::load()?;
    let found = game::find(&Places::new(s.game.as_deref(), &cfg))
        .context("pilotebox setup --game <the title's executable or its folder> tells it where the title is")?;
    let dir = found.exe.parent().context("the title's executable has no folder")?.to_owned();
    line("game", format!("{} ({})", found.exe.display(), found.from));

    match &cfg.source {
        Some(file)
            if cfg
                .game_dir
                .as_deref()
                .and_then(game::exe_in)
                .and_then(|e| std::path::absolute(e).ok())
                .is_some_and(|e| e == found.exe) =>
        {
            line("config", format!("{} names it already", file.display()));
        }
        Some(file) => line(
            "config",
            format!(
                "{} is yours and stays as it is: put game_dir = \"{}\" under [paths] there",
                file.display(),
                dir.display()
            ),
        ),
        None => {
            let file = user_config()?;
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
            }
            std::fs::write(&file, config_text(&dir)).with_context(|| format!("writing {}", file.display()))?;
            line("config", format!("{} written", file.display()));
        }
    }
    if let Some(note) = crate::play::legacy_note(&cfg) {
        line("note", note);
    }

    println!();
    let checks = doctor::checks(Some(&found.exe));
    doctor::print(&checks);
    if checks.iter().any(|c| c.level == Level::Problem) {
        println!("Not ready: fix the problems above, then run pilotebox setup again.");
        std::process::exit(1);
    }

    if s.no_shaders {
        line("shaders", "left to the first run (--no-shaders)");
    } else {
        println!();
        line("shaders", "the title's shaders go into the cache now: a few minutes, once per translator");
        crate::translate::precompile(vec![dir], s.jobs)?;
    }
    println!();
    line("next", "pilotebox play, or a double click on PiloteBox.cmd");
    line("", "pilotebox check runs it for 90 s and says PASS or FAIL");
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_config_names_the_folder_in_toml() {
        let text = super::config_text(std::path::Path::new(r#"D:\Games\A "B""#));
        assert!(text.ends_with("[paths]\ngame_dir = \"D:\\\\Games\\\\A \\\"B\\\"\"\n"), "{text}");
        assert!(text.lines().filter(|l| !l.starts_with('#')).count() == 2);
    }
}
