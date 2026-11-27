//! `pilotebox` (the `victory` package): the command-line front end.
//!
//! Every command reads the user's own files and writes only to stdout or to
//! the user's cache directory (outside the repository).
//!
//! The commands for playing (`play`, `bench`, `check`, `doctor`, `clean`)
//! come first; the developers' (`run`, `shaders`, `warm`, `translate`,
//! `replay` and the others) stay as upstream has them.

mod bench;
mod census;
mod check;
mod clean;
mod defaults;
mod doctor;
mod frames;
mod game;
mod inspect;
mod play;
#[cfg(all(windows, feature = "diagnostics"))]
mod replay;
mod run;
mod shader;
mod sys;
mod translate;
mod translate_diff;
#[cfg(windows)]
mod warm;

use std::num::NonZeroUsize;
use std::path::PathBuf;

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};

use victory_core::product;

/// Under the commands in `--help`.
const AFTER_HELP: &str = "\
First time: `pilotebox doctor`, then `pilotebox play`. A benchmark: `pilotebox bench`. \
Every VICTORY_* switch of the layer still works as it is.";

#[derive(Parser)]
#[command(
    name = product::BINARY,
    version = product::VERSION,
    about = product::banner(),
    after_help = AFTER_HELP
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
enum Command {
    /// Plays the game, found with no path to type (Series X unless --mode s).
    Play(play::Play),
    /// A timed run that ends by itself, then its figures, printed and written
    /// beside the cache folder.
    #[command(after_help = bench::AFTER_HELP)]
    Bench(bench::Bench),
    /// Checks this PC: card, driver, PCIe link, processor, memory, disk,
    /// cache, the game's files, other runs.
    Doctor(doctor::Doctor),
    /// doctor, then a short bench on a copy of the data folder, ending with
    /// PASS or FAIL.
    Check(check::Check),
    /// Lists what in the cache folder is old and removes it with --yes. Never
    /// the data folder.
    Clean(clean::Clean),
    /// Summarise one PE image: headers, sections, imports, TLS, build id.
    Inspect {
        file: PathBuf,
        /// List every imported function.
        #[arg(long)]
        imports: bool,
        /// List every exported function.
        #[arg(long)]
        exports: bool,
    },
    /// Aggregate the imports of every PE image under the given paths and
    /// show which ones our runtime covers. Defaults to the configured
    /// game and test-title directories.
    Census {
        paths: Vec<PathBuf>,
        /// Do not count host pass-through as covered.
        #[arg(long)]
        strict: bool,
        /// List every missing `dll!function`, most used first.
        #[arg(long)]
        missing: bool,
    },
    /// List the `dll!export` pairs an import library describes.
    LibExports {
        lib: PathBuf,
        /// List every export, not just per-DLL counts.
        #[arg(long)]
        list: bool,
    },
    /// Find shader containers in files or directories (also inside engine
    /// archives, if uncompressed) and report their layout. Shader packs
    /// among the paths are listed last, their stages named from the
    /// containers found.
    ShaderInfo {
        paths: Vec<PathBuf>,
        /// Print one line per container found (and per pack pipeline), and
        /// show why paths were skipped.
        #[arg(short, long)]
        verbose: bool,
        /// Also list the decoded native instructions (implies --verbose).
        #[arg(long)]
        disasm: bool,
        /// Instead list the effects of shader databases: technique, pass,
        /// shaders (name, container hash, engine hash) and pass state.
        #[arg(long)]
        effects: bool,
    },
    /// Run every shader found (containers, SGD5 files) through the
    /// translator and report what it cannot handle yet.
    ///
    /// VICTORY_HOST_WAVE (or VICTORY_HOST_MAX_WAVE) translates as for a host
    /// of waves that wide, including with --warm.
    Translate {
        paths: Vec<PathBuf>,
        /// Check emitted DXIL with dxil.dll's validator (`VICTORY_DXIL`,
        /// else the newest Windows SDK's).
        #[arg(long)]
        validate: bool,
        /// Also create a host D3D12 pipeline for each emitted shader.
        #[arg(long)]
        pipelines: bool,
        /// Print one line per program and outcome.
        #[arg(long)]
        list: bool,
        /// With the list (implied): each emitted program's line names a
        /// hash of its DXIL (`dxil=` and 16 hex digits of its BLAKE3,
        /// before validation), for `translate-diff`.
        #[arg(long)]
        hashes: bool,
        /// Worker threads, at least 1 (default: every logical core).
        #[arg(long)]
        jobs: Option<NonZeroUsize>,
        /// Warm the run's shader cache: a file of shader hashes (the 16
        /// hex digits a run's log names a shader by, anywhere in a line),
        /// or `all`; the containers with those hashes are translated into
        /// the cache for the detected adapter and nothing else is done. Set
        /// `VICTORY_HOST_WAVE` and `VICTORY_VIEW_LIMIT` to warm for a
        /// different host.
        #[arg(long)]
        warm: Option<PathBuf>,
    },
    /// Compare two `translate --list` outputs (two builds over the same
    /// shaders): the programs one validates and the other does not, with
    /// the other's reasons, and with `--hashes` lists, the programs both
    /// validate whose DXIL differs.
    TranslateDiff {
        old: PathBuf,
        new: PathBuf,
        /// Also name each program (container hash and stage).
        #[arg(long)]
        keys: bool,
    },
    /// Translate every shader found (as `translate` finds them) into the
    /// shader cache, so that no run has to: a few minutes, once per version
    /// of the translator. Translations an earlier build of the same version
    /// cached are made again, and those that came out different counted.
    /// In diagnostics builds, VICTORY_LOG=debug shows skipped paths.
    Shaders {
        paths: Vec<PathBuf>,
        /// Worker threads (default: every logical core).
        #[arg(long)]
        jobs: Option<usize>,
        /// Also write every shader that does not translate to this file,
        /// one line each: hash, where it was found, why.
        #[arg(long)]
        list: Option<PathBuf>,
    },
    /// Make the host pipelines of every pipeline the title ever created
    /// (runs with `VICTORY_PIPELINE_BOOK=1` write them down), so that the
    /// driver's cache on disk has them before a run needs one: once per change
    /// of the translator or the driver. Goes on where it was stopped.
    #[cfg(windows)]
    Warm {
        /// Processes at once (default: two thirds of the logical cores).
        #[arg(long)]
        jobs: Option<usize>,
        /// At most this many pipelines.
        #[arg(long)]
        limit: Option<usize>,
        /// Those made before too.
        #[arg(long)]
        again: bool,
        /// Only the pipelines earlier runs drew with.
        #[arg(long)]
        used: bool,
        /// (What the processes it starts are given.)
        #[arg(long, hide = true)]
        these: Option<String>,
    },
    /// Prepare, map and bind a title executable; with --execute, run it.
    Run {
        exe: PathBuf,
        /// What unimplemented imports do when called.
        #[arg(long, value_enum, default_value_t = Unimplemented::Trap)]
        unimplemented: Unimplemented,
        /// Disable host pass-through (everything must be implemented by us).
        #[arg(long)]
        strict: bool,
        /// Actually start the title. Without it, stop after binding.
        #[arg(long)]
        execute: bool,
        /// The console model the title sees (device type, memory).
        #[arg(long, value_enum, default_value_t = Console::SeriesX)]
        console: Console,
        /// Arguments for the title, after `--` (it reads the whole command
        /// line; a title that relaunches itself passes some).
        #[arg(last = true)]
        title_args: Vec<String>,
    },
    /// Run a captured frame's GPU work again (diagnostics builds; see
    /// docs/replay.md).
    #[cfg(all(windows, feature = "diagnostics"))]
    Replay(replay::Replay),
    /// Show the resolved configuration and check it.
    Config,
}

#[derive(Clone, Copy, ValueEnum)]
enum Unimplemented {
    Trap,
    Zero,
}

#[derive(Clone, Copy, ValueEnum)]
enum Console {
    SeriesX,
    SeriesS,
}

fn main() -> Result<()> {
    let first = std::env::args().nth(1);
    // The switches the title is played with, before anything reads them
    // (`defaults`).
    if first.as_deref() == Some("run") {
        println!("{}", product::banner());
        defaults::apply();
    }
    // The checks print their own lines: the layer's, opening the card, are
    // left out unless they warn.
    let quiet = matches!(first.as_deref(), Some("doctor" | "check" | "clean"));
    victory_core::logging::init(if quiet { "warn" } else { "info" });
    // The audio recorder `run` starts for `VICTORY_AUDIO_CAPTURE`.
    #[cfg(all(windows, feature = "diagnostics"))]
    if victory_hle::modules::audio::capture::child() {
        return Ok(());
    }
    let cli = Cli::parse();
    let Some(command) = cli.command else {
        Cli::command().print_help()?;
        return Ok(());
    };
    match command {
        Command::Play(options) => play::run(options),
        Command::Bench(options) => bench::run(options),
        Command::Doctor(options) => doctor::run(options),
        Command::Check(options) => check::run(options),
        Command::Clean(options) => clean::run(options),
        Command::Inspect {
            file,
            imports,
            exports,
        } => inspect::run(&file, imports, exports),
        Command::Census {
            paths,
            strict,
            missing,
        } => census::run(paths, strict, missing),
        Command::LibExports { lib, list } => inspect::lib_exports(&lib, list),
        Command::ShaderInfo {
            paths,
            verbose,
            disasm,
            effects,
        } => shader::run(paths, verbose || disasm, disasm, effects),
        Command::Translate {
            paths,
            validate,
            pipelines,
            list,
            hashes,
            jobs,
            warm,
        } => {
            let view_limit = std::env::var("VICTORY_VIEW_LIMIT").ok().and_then(|v| v.parse().ok());
            let host_wave = std::env::var("VICTORY_HOST_WAVE").ok().and_then(|v| v.parse().ok());
            // The warm-up keys translations on what the adapter tells the
            // translator, as a run does: the host is opened for what is not set.
            if warm.is_some() && (view_limit.is_none() || host_wave.is_none()) {
                victory_hle::modules::d3d12x::prepare_shader_warm().map_err(anyhow::Error::msg)?;
            }
            // `VICTORY_VIEW_LIMIT=<null slot>`: translate as on a host whose view heap is capped
            if let Some(n) = view_limit {
                victory_shader::emit::set_view_heap_limit(n);
            }
            // `VICTORY_HOST_WAVE=32`: translate as for a GPU whose waves are at most 32 lanes
            // (`translate` itself also reads `VICTORY_HOST_MAX_WAVE`)
            if let Some(n) = host_wave {
                victory_shader::emit::set_host_max_wave(n);
            }
            let jobs = jobs.map(NonZeroUsize::get);
            if let Some(file) = warm {
                return translate::warm(paths, &file, jobs);
            }
            translate::run(paths, validate, pipelines, list, hashes, jobs)
        }
        Command::TranslateDiff { old, new, keys } => translate_diff::run(&old, &new, keys),
        Command::Shaders { paths, jobs, list } => translate::precompile(paths, jobs, list),
        #[cfg(windows)]
        Command::Warm { jobs, limit, again, used, these } => warm::run(jobs, limit, again, used, these),
        Command::Run {
            exe,
            unimplemented,
            strict,
            execute,
            console,
            title_args,
        } => {
            victory_hle::runtime::set_title_args(title_args);
            run::run(&exe, unimplemented, strict, execute, console)
        }
        #[cfg(all(windows, feature = "diagnostics"))]
        Command::Replay(options) => replay::run(options),
        Command::Config => show_config(),
    }
}

fn show_config() -> Result<()> {
    let cfg = victory_core::Config::load()?;
    let status = |p: &Option<PathBuf>| match p {
        Some(p) if p.exists() => format!("{}", p.display()),
        Some(p) => format!("{}  (does not exist)", p.display()),
        None => "(not set)".to_owned(),
    };
    println!(
        "config file : {}",
        cfg.source
            .as_ref()
            .map_or("(none)".into(), |p| p.display().to_string())
    );
    println!("game_dir    : {}", status(&cfg.game_dir));
    println!("cache_dir   : {}", cfg.cache_dir.display());
    println!("data_dir    : {}", cfg.data_dir.display());
    for t in &cfg.test_titles {
        println!("test_title  : {}", status(&Some(t.clone())));
    }
    for p in cfg
        .game_dir
        .iter()
        .chain(&cfg.test_titles)
        .chain([&cfg.cache_dir, &cfg.data_dir])
    {
        if let Err(e) = victory_core::ensure_outside_repo(p) {
            println!("POLICY VIOLATION: {e}");
        }
    }
    Ok(())
}

/// Paths given on the command line, or the configured game + test titles.
pub(crate) fn default_paths(paths: Vec<PathBuf>) -> Result<Vec<PathBuf>> {
    if !paths.is_empty() {
        return Ok(paths);
    }
    let cfg = victory_core::Config::load()?;
    let all: Vec<PathBuf> = cfg.game_dir.into_iter().chain(cfg.test_titles).collect();
    anyhow::ensure!(
        !all.is_empty(),
        "no paths given and none configured (see victory.example.toml)"
    );
    Ok(all)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jobs(args: &[&str]) -> Result<Option<usize>, clap::Error> {
        let all = [&["pilotebox", "translate", "shaders"][..], args].concat();
        match Cli::try_parse_from(all)?.command {
            Some(Command::Translate { jobs, .. }) => Ok(jobs.map(NonZeroUsize::get)),
            _ => unreachable!(),
        }
    }

    fn parse(args: &[&str]) -> Result<Command, clap::Error> {
        let all = [&["pilotebox"][..], args].concat();
        Ok(Cli::try_parse_from(all)?.command.expect("a command"))
    }

    #[test]
    fn the_cli_is_valid_and_named() {
        Cli::command().debug_assert();
        assert_eq!(Cli::command().get_name(), "pilotebox");
        assert!(Cli::try_parse_from(["pilotebox"]).unwrap().command.is_none());
    }

    #[test]
    fn play_takes_a_mode_by_name() {
        let Command::Play(p) = parse(&["play"]).unwrap() else { panic!() };
        assert_eq!((p.profile.mode, p.profile.rt, p.profile.safe, p.profile.game), (game::Mode::X, false, false, None));
        let Command::Play(p) = parse(&["play", "--mode", "s", "--rt", "--safe", "--game", r"D:\g", "--", "-x=1"]).unwrap()
        else {
            panic!()
        };
        assert_eq!(p.profile.mode, game::Mode::S);
        assert!(p.profile.rt && p.profile.safe && !p.crash_log);
        assert_eq!(p.profile.game, Some(PathBuf::from(r"D:\g")));
        assert_eq!(p.title_args, ["-x=1"]);
        // The long names too; nothing else.
        assert!(matches!(parse(&["play", "--mode", "series-s"]), Ok(Command::Play(p)) if p.profile.mode == game::Mode::S));
        assert!(parse(&["play", "--mode", "one"]).is_err());
    }

    #[test]
    fn bench_check_and_clean_take_their_options() {
        let Command::Bench(b) = parse(&["bench"]).unwrap() else { panic!() };
        assert_eq!((b.seconds, b.keep_data, b.name.as_deref(), b.keys.as_deref()), (300, false, None, None));
        let Command::Bench(b) =
            parse(&["bench", "--seconds", "120", "--mode", "s", "--rt", "--name", "A b", "--keep-data", "--keys", "none"])
                .unwrap()
        else {
            panic!()
        };
        assert_eq!((b.seconds, b.keep_data, b.keys.as_deref()), (120, true, Some("none")));
        let options = bench::Options::from(b);
        assert_eq!(options.label, "A-b");
        let Command::Bench(b) = parse(&["bench", "--rt"]).unwrap() else { panic!() };
        assert_eq!(bench::Options::from(b).label, "series-x-rt");
        assert!(parse(&["bench", "--seconds", "soon"]).is_err());
        let Command::Check(c) = parse(&["check"]).unwrap() else { panic!() };
        assert_eq!(c.seconds, 90);
        let Command::Check(c) = parse(&["check", "--seconds", "30"]).unwrap() else { panic!() };
        assert_eq!(c.seconds, 30);
        let Command::Clean(c) = parse(&["clean"]).unwrap() else { panic!() };
        assert!(!c.all && !c.yes);
        let Command::Clean(c) = parse(&["clean", "--all", "--yes"]).unwrap() else { panic!() };
        assert!(c.all && c.yes);
        assert!(matches!(parse(&["doctor"]), Ok(Command::Doctor(d)) if d.game.is_none()));
    }

    #[test]
    fn run_keeps_its_interface() {
        let Command::Run { exe, execute, console, .. } =
            parse(&["run", r"D:\g\t.exe", "--execute", "--console", "series-s"]).unwrap()
        else {
            panic!()
        };
        assert_eq!(exe, PathBuf::from(r"D:\g\t.exe"));
        assert!(execute && matches!(console, Console::SeriesS));
    }

    #[test]
    fn translate_jobs_must_be_at_least_one() {
        let zero = jobs(&["--jobs", "0"]).expect_err("--jobs 0 is refused");
        assert_eq!(zero.kind(), clap::error::ErrorKind::ValueValidation);
        assert_eq!(jobs(&["--jobs", "1"]).unwrap(), Some(1));
        assert_eq!(jobs(&[]).unwrap(), None);
    }
}
