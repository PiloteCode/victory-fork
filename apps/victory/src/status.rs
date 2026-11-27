//! A bare `pilotebox`: where things stand (the title, the folders, the
//! cache, the last play and the last benchmark), then the commands in two
//! groups: for playing, and upstream's for developers.

use crate::play::line;
use crate::sys::gb;

/// The commands for playing, in the order they are listed.
pub(crate) const PLAYING: [&str; 8] = ["setup", "play", "bench", "check", "doctor", "clean", "migrate", "report"];

/// The status lines: label and text.
fn lines() -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    let cfg = match victory_core::Config::load() {
        Ok(cfg) => cfg,
        Err(e) => {
            out.push(("config", format!("{e:#}")));
            return out;
        }
    };
    out.push(match crate::game::find(&crate::game::Places::new(None, &cfg)) {
        Ok(found) => ("game", format!("{} ({})", found.exe.display(), found.from)),
        Err(_) => ("game", "not found: pilotebox setup --game <the title's executable or its folder>".to_owned()),
    });
    out.push(("mode", "Series X (play --mode s: Series S)".to_owned()));
    out.push(("data", cfg.data_dir.display().to_string()));
    if let Some(note) = crate::play::legacy_note(&cfg) {
        out.push(("note", note));
    }
    let parts: Vec<(String, u64)> = crate::clean::PARTS
        .iter()
        .map(|p| (p.to_string(), crate::clean::size(&cfg.cache_dir.join(p)).0))
        .collect();
    out.push((
        "cache",
        if parts.iter().all(|(_, n)| *n == 0) {
            "empty: pilotebox setup translates the shaders ahead".to_owned()
        } else {
            parts.iter().map(|(p, n)| format!("{p} {}", gb(*n))).collect::<Vec<_>>().join(", ")
        },
    ));
    out.push((
        "last play",
        crate::state::last_run(&cfg).map_or_else(|| "none yet".to_owned(), |run| run.line()),
    ));
    let bench = crate::state::newest_bench(&crate::bench::reports_dir(&cfg))
        .and_then(|b| std::fs::read_to_string(b).ok())
        .and_then(|json| crate::state::json_field(&json, "line"));
    out.push(("last bench", bench.unwrap_or_else(|| "none yet (pilotebox bench)".to_owned())));
    out
}

pub(crate) fn run(cli: &clap::Command) {
    println!("{}", victory_core::product::banner());
    for (label, text) in lines() {
        line(label, text);
    }
    let about = |name: &str| {
        cli.find_subcommand(name)
            .and_then(|c| c.get_about())
            .map(|a| a.to_string())
            .unwrap_or_default()
    };
    println!("\nFor playing:");
    for name in PLAYING {
        println!("  {name:<15}{}", about(name));
    }
    println!("\nFor developers (upstream's commands, unchanged):");
    let mut developers: Vec<&str> = cli
        .get_subcommands()
        .map(|c| c.get_name())
        .filter(|n| !PLAYING.contains(n) && *n != "help")
        .collect();
    developers.sort_unstable();
    println!("  {}", developers.join(", "));
    println!(
        "\n`pilotebox <command> --help` says more. The switches are PILOTEBOX_* (the VICTORY_* names work too)."
    );
}
