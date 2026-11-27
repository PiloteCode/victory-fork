//! Where the game is, and the profile it is played with: what `play`,
//! `bench`, `check` and `doctor` share.
//!
//! The executable is looked for in this order: `--game`, `PILOTEBOX_GAME_DIR`
//! (or `VICTORY_GAME_DIR`), the config file's `[paths] game_dir`
//! (`pilotebox.toml` or `victory.toml`; the environment
//! takes its place when set, as for every path of the config), then beside
//! this program and in each folder above it (the folder above the
//! repository, for a program built in it), then in the working folder and
//! the folders above it.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::Console;

/// The executable looked for in a folder whose manifest names none.
pub(crate) const DEFAULT_EXE: &str = "game_scarlett_master.exe";

/// The title's manifest, beside its executable.
const MANIFEST: &str = "MicrosoftGame.config";

/// The console profile the title sees.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum Mode {
    /// Series X (the default).
    #[value(alias = "series-x")]
    X,
    /// Series S (`run --console series-s`).
    #[value(alias = "series-s")]
    S,
}

impl Mode {
    pub(crate) fn console(self) -> Console {
        match self {
            Mode::X => Console::SeriesX,
            Mode::S => Console::SeriesS,
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Mode::X => "Series X",
            Mode::S => "Series S",
        }
    }

    /// For file names.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Mode::X => "series-x",
            Mode::S => "series-s",
        }
    }
}

/// The options `play`, `bench` and `check` share.
#[derive(clap::Args, Debug, Clone)]
pub(crate) struct Profile {
    /// The console profile: x is Series X (the default), s is Series S.
    #[arg(long, value_enum, default_value_t = Mode::X)]
    pub mode: Mode,
    /// Turns the ray-traced light's structure builder on
    /// (PILOTEBOX_BVH_BUILDER=1): for looking, not for playing yet.
    #[arg(long)]
    pub rt: bool,
    /// The play defaults without their newest ways
    /// (PILOTEBOX_PLAY_DEFAULTS=safe): the first thing to try when something
    /// is wrong.
    #[arg(long)]
    pub safe: bool,
    /// The title's executable, or its folder. Default: PILOTEBOX_GAME_DIR,
    /// then the config file's [paths] game_dir (pilotebox setup writes it),
    /// then the title's executable beside this program or in a folder above
    /// it (the one above the repository).
    #[arg(long, value_name = "PATH")]
    pub game: Option<PathBuf>,
}

impl Profile {
    /// The switches it sets: name, value and the option that asked.
    pub(crate) fn switches(&self) -> Vec<(&'static str, &'static str, &'static str)> {
        let mut out = Vec::new();
        if self.rt {
            out.push(("VICTORY_BVH_BUILDER", "1", "--rt"));
        }
        if self.safe {
            out.push(("VICTORY_PLAY_DEFAULTS", "safe", "--safe"));
        }
        out
    }

    /// A label for file names: the mode, then the switches.
    pub(crate) fn label(&self) -> String {
        let mut label = self.mode.label().to_owned();
        for (on, name) in [(self.rt, "-rt"), (self.safe, "-safe")] {
            if on {
                label.push_str(name);
            }
        }
        label
    }
}

/// Where to look for the game, in order.
#[derive(Debug, Default)]
pub(crate) struct Places {
    /// `--game`: an executable or a folder.
    pub arg: Option<PathBuf>,
    /// `PILOTEBOX_GAME_DIR`, which start-up copies to `VICTORY_GAME_DIR`.
    pub env: Option<PathBuf>,
    /// The config file's `[paths] game_dir` (when the environment sets none).
    pub config: Option<PathBuf>,
    /// Folders looked in last, and how each is told.
    pub around: Vec<(PathBuf, &'static str)>,
}

impl Places {
    /// From the command line, the environment, the config and where this
    /// program is.
    pub(crate) fn new(arg: Option<&Path>, cfg: &victory_core::Config) -> Places {
        let env = std::env::var_os("VICTORY_GAME_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        // `Config` takes the environment's first: its value is the file's
        // only when the environment sets none.
        let config = if env.is_some() { None } else { cfg.game_dir.clone() };
        let mut around: Vec<(PathBuf, &'static str)> = Vec::new();
        let mut add = |from: Option<PathBuf>, here: &'static str, above: &'static str| {
            let Some(from) = from else { return };
            for (i, dir) in from.ancestors().enumerate() {
                if !around.iter().any(|(d, _)| d == dir) {
                    around.push((dir.to_owned(), if i == 0 { here } else { above }));
                }
            }
        };
        let program = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_owned));
        add(program, "beside this program", "in a folder above this program");
        add(std::env::current_dir().ok(), "in the working folder", "in a folder above the working folder");
        Places {
            arg: arg.map(Path::to_owned),
            env,
            config,
            around,
        }
    }
}

/// The game found, how, and what was passed over on the way.
#[derive(Debug, PartialEq)]
pub(crate) struct Found {
    pub exe: PathBuf,
    pub from: String,
    pub notes: Vec<String>,
}

/// The game's executable, by [`Places`] in order. A `--game` that holds no
/// game is an error; a configured folder that holds none is passed over
/// with a note.
pub(crate) fn find(places: &Places) -> Result<Found> {
    let found = |exe: &Path, from: String, notes: Vec<String>| Found {
        exe: std::path::absolute(exe).unwrap_or_else(|_| exe.to_owned()),
        from,
        notes,
    };
    if let Some(arg) = &places.arg {
        if arg.is_file() {
            return Ok(found(arg, "--game".into(), Vec::new()));
        }
        if !arg.is_dir() {
            bail!("--game {}: no such file or folder", arg.display());
        }
        return match exe_in(arg) {
            Some(exe) => Ok(found(&exe, "--game".into(), Vec::new())),
            None => bail!(
                "--game {}: no game there (neither the title's executable nor a {MANIFEST} naming one that is there)",
                arg.display()
            ),
        };
    }
    let mut notes = Vec::new();
    for (place, from) in [(&places.env, "PILOTEBOX_GAME_DIR"), (&places.config, "the config file's [paths] game_dir")] {
        let Some(place) = place else { continue };
        let exe = if place.is_file() { Some(place.clone()) } else { exe_in(place) };
        match exe {
            Some(exe) => return Ok(found(&exe, from.into(), notes)),
            None => notes.push(format!("{from} is {}, which holds no game: looked further", place.display())),
        }
    }
    for (dir, how) in &places.around {
        if let Some(exe) = exe_in(dir) {
            return Ok(found(&exe, format!("found {how}"), notes));
        }
    }
    let mut message = String::from(
        "no game found: run `pilotebox setup --game <the title's executable or folder>` (it writes pilotebox.toml), \
         or set PILOTEBOX_GAME_DIR, or keep this program in a folder under the title's (the title's executable \
         is looked for beside it and in each folder above it)",
    );
    for note in &notes {
        message.push_str("; ");
        message.push_str(note);
    }
    bail!(message)
}

/// The game's executable in `dir`: the one its manifest names, else
/// [`DEFAULT_EXE`].
pub(crate) fn exe_in(dir: &Path) -> Option<PathBuf> {
    if let Ok(text) = std::fs::read_to_string(dir.join(MANIFEST))
        && let Some(name) = manifest_exe(&text)
    {
        let exe = dir.join(name);
        if exe.is_file() {
            return Some(exe);
        }
    }
    let exe = dir.join(DEFAULT_EXE);
    exe.is_file().then_some(exe)
}

/// The first executable a manifest names (`<Executable Name="..."`).
fn manifest_exe(text: &str) -> Option<&str> {
    let mut rest = text;
    while let Some(at) = rest.find("<Executable") {
        rest = &rest[at + "<Executable".len()..];
        // Not `<ExecutableList>`.
        if !rest.starts_with(char::is_whitespace) {
            continue;
        }
        let tag = &rest[..rest.find('>')?];
        let mut attrs = tag;
        while let Some(at) = attrs.find("Name=") {
            let before = attrs[..at].chars().next_back();
            let value = &attrs[at + "Name=".len()..];
            attrs = value;
            if !before.is_some_and(char::is_whitespace) {
                continue;
            }
            let quote = value.chars().next().filter(|&q| q == '"' || q == '\'')?;
            let value = &value[1..];
            let name = &value[..value.find(quote)?];
            return (!name.trim().is_empty()).then_some(name.trim());
        }
    }
    None
}

/// The title's id from the manifest beside `exe` (0 without one), as
/// `run` names its data folder.
pub(crate) fn title_id(exe: &Path) -> u32 {
    let manifest = exe
        .parent()
        .and_then(|dir| std::fs::read_to_string(dir.join(MANIFEST)).ok())
        .unwrap_or_default();
    victory_hle::runtime::Title::from_manifest(&manifest).id
}

/// Where a run of `exe` with the data folder `data` keeps its crash log.
pub(crate) fn crash_log(data: &Path, exe: &Path) -> PathBuf {
    data.join(format!("{:08X}", title_id(exe))).join("crashes.log")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"not a real executable").unwrap();
    }

    /// Folders: `cfg/` and `env/` each with a game, `none/` without, and
    /// `top/sub/bin/` with a game in `top/`.
    fn tree() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        for dir in ["env", "cfg", "top"] {
            touch(&root.path().join(dir).join(DEFAULT_EXE));
        }
        std::fs::create_dir_all(root.path().join("none")).unwrap();
        std::fs::create_dir_all(root.path().join("top/sub/bin")).unwrap();
        root
    }

    fn around(dir: &Path) -> Vec<(PathBuf, &'static str)> {
        dir.ancestors()
            .enumerate()
            .map(|(i, d)| (d.to_owned(), if i == 0 { "beside" } else { "above" }))
            .collect()
    }

    #[test]
    fn the_search_goes_in_order() {
        let root = tree();
        let at = |p: &str| root.path().join(p);
        let mut places = Places {
            arg: Some(at("cfg").join(DEFAULT_EXE)),
            env: Some(at("env")),
            config: Some(at("cfg")),
            around: around(&at("top/sub/bin")),
        };
        // --game first, an executable or a folder.
        assert_eq!(find(&places).unwrap().from, "--game");
        places.arg = Some(at("top"));
        let found = find(&places).unwrap();
        assert_eq!((found.exe, found.from), (at("top").join(DEFAULT_EXE), "--game".to_owned()));
        // Then the environment, then the config.
        places.arg = None;
        let found = find(&places).unwrap();
        assert_eq!((found.exe, found.from.as_str()), (at("env").join(DEFAULT_EXE), "PILOTEBOX_GAME_DIR"));
        places.env = None;
        let found = find(&places).unwrap();
        assert_eq!(found.exe, at("cfg").join(DEFAULT_EXE));
        assert!(found.from.contains("game_dir"));
        // Then beside the program and above it: `top/`, two folders up.
        places.config = None;
        let found = find(&places).unwrap();
        assert_eq!((found.exe, found.from.as_str()), (at("top").join(DEFAULT_EXE), "found above"));
        assert!(found.notes.is_empty());
    }

    #[test]
    fn a_configured_folder_without_the_game_is_passed_over() {
        let root = tree();
        let at = |p: &str| root.path().join(p);
        let places = Places {
            env: Some(at("none")),
            around: around(&at("top/sub/bin")),
            ..Places::default()
        };
        let found = find(&places).unwrap();
        assert_eq!(found.exe, at("top").join(DEFAULT_EXE));
        assert_eq!(found.notes.len(), 1);
        assert!(found.notes[0].starts_with("PILOTEBOX_GAME_DIR is "), "{:?}", found.notes);
    }

    #[test]
    fn a_wrong_game_option_is_an_error() {
        let root = tree();
        let at = |p: &str| root.path().join(p);
        for arg in [at("none"), at("missing.exe")] {
            let places = Places {
                arg: Some(arg),
                env: Some(at("env")),
                ..Places::default()
            };
            let error = find(&places).unwrap_err().to_string();
            assert!(error.starts_with("--game "), "{error}");
        }
        // Nowhere: the message says what to do.
        let error = find(&Places::default()).unwrap_err().to_string();
        assert!(error.contains("--game") && error.contains("PILOTEBOX_GAME_DIR") && error.contains("pilotebox setup"));
        assert!(!error.contains(DEFAULT_EXE));
    }

    #[test]
    fn the_manifest_names_the_executable() {
        let root = tempfile::tempdir().unwrap();
        let manifest = r#"<?xml version="1.0"?>
<Game configVersion="1">
  <ExecutableList>
    <Executable Name="title_main.exe" TargetDeviceFamily="Scarlett" OverrideDisplayName="x" Id="Game" />
  </ExecutableList>
</Game>"#;
        assert_eq!(manifest_exe(manifest), Some("title_main.exe"));
        assert_eq!(manifest_exe("<ExecutableList></ExecutableList>"), None);
        assert_eq!(manifest_exe("<Executable OverrideDisplayName='a' Name='b.exe'/>"), Some("b.exe"));
        std::fs::write(root.path().join(MANIFEST), manifest).unwrap();
        // Named but not there: the default name is looked for.
        assert_eq!(exe_in(root.path()), None);
        touch(&root.path().join(DEFAULT_EXE));
        assert_eq!(exe_in(root.path()), Some(root.path().join(DEFAULT_EXE)));
        touch(&root.path().join("title_main.exe"));
        assert_eq!(exe_in(root.path()), Some(root.path().join("title_main.exe")));
    }

    #[test]
    fn the_crash_log_is_in_the_title_folder() {
        let root = tempfile::tempdir().unwrap();
        let exe = root.path().join(DEFAULT_EXE);
        touch(&exe);
        let data = Path::new("data");
        assert_eq!(crash_log(data, &exe), data.join("00000000").join("crashes.log"));
        std::fs::write(root.path().join(MANIFEST), "<Game><TitleId>12AB34CD</TitleId></Game>").unwrap();
        assert_eq!(crash_log(data, &exe), data.join("12AB34CD").join("crashes.log"));
    }

    #[test]
    fn profiles_name_their_switches() {
        let profile = Profile {
            mode: Mode::S,
            rt: true,
            safe: true,
            game: None,
        };
        assert_eq!(profile.label(), "series-s-rt-safe");
        let names: Vec<_> = profile.switches().into_iter().map(|(n, v, _)| format!("{n}={v}")).collect();
        assert_eq!(names, ["VICTORY_BVH_BUILDER=1", "VICTORY_PLAY_DEFAULTS=safe"]);
        assert!(matches!(profile.mode.console(), Console::SeriesS));
    }
}
