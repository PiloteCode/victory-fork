//! `pilotebox bench`: a timed run that ends by itself, then the layer's own
//! frame lines summed up (`frames`), printed, and written beside the cache
//! folder (`<cache>\..\bench`, never inside the repository) as Markdown and
//! JSON, with the run's whole log.
//!
//! The run is `pilotebox run <the game> --execute` in a process of its own,
//! whose output is read here. At the end its window is asked to close, as
//! its close button does (the title suspends, the pipelines made are kept
//! for the next run); it is ended if it has not 20 s later. It plays on a
//! copy of the data folder unless `--keep-data`, so that a benchmark never
//! writes into the user's saves.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::frames::{Problem, Seen, Summary, Tally, plain};
use crate::game::{self, Mode, Places, Profile};
use crate::play::line;

/// Keys pressed by script unless told otherwise: from the title's start
/// screen, through a saved game, into the world in about two minutes.
pub(crate) const DEFAULT_KEYS: &str = "30:Enter,40:Enter,50:Enter,60:Enter,70:Enter,80:Enter,90:Enter";

/// How long a run has to end once its window was asked to close.
const GRACE: Duration = Duration::from_secs(20);

pub(crate) const AFTER_HELP: &str = "\
The figures are the layer's own frame lines (gpu::host, logged at info level by every build: one \
second's frames, every five seconds); bench asks the run for them whatever PILOTEBOX_LOG says. The \
dropped draws' reasons are logged by diagnostics builds only \
(cargo build --profile diag -p victory --features diagnostics). Means are over the intervals of the \
world (1,000 draws a frame or more) with no pipeline being made. The report (.md, .json and the whole \
.log) goes to a bench folder beside the cache folder: %LOCALAPPDATA%\\PiloteBox\\bench by default. \
The last line is a one-line summary to paste into an issue.";

#[derive(clap::Args, Debug)]
pub(crate) struct Bench {
    /// How long the run lasts before its window is closed, in seconds.
    #[arg(long, default_value_t = 300, value_name = "N")]
    pub seconds: u64,
    #[command(flatten)]
    pub profile: Profile,
    /// A label for the report's files (default: the mode and switches, for
    /// instance series-x-rt).
    #[arg(long, value_name = "LABEL")]
    pub name: Option<String>,
    /// Plays on the real data folder (your saves and settings) instead of a
    /// copy of it.
    #[arg(long)]
    pub keep_data: bool,
    /// Keys pressed by script (PILOTEBOX_KEYS: seconds:key,...), or none.
    /// Default: PILOTEBOX_KEYS if set, else Enter every ten seconds from 30 s
    /// to 90 s.
    #[arg(long, value_name = "KEYS")]
    pub keys: Option<String>,
    /// Prints the report as JSON at the end (the progress goes to stderr).
    #[arg(long)]
    pub json: bool,
}

/// What a measured run is asked to be.
#[derive(Debug, Clone)]
pub(crate) struct Options {
    pub seconds: u64,
    pub profile: Profile,
    pub label: String,
    pub keep_data: bool,
    pub keys: Option<String>,
    /// The progress to stderr, stdout being kept for JSON.
    pub json: bool,
}

impl From<Bench> for Options {
    fn from(b: Bench) -> Options {
        let label = b.name.as_deref().map(sanitize).filter(|l| !l.is_empty());
        Options {
            seconds: b.seconds,
            label: label.unwrap_or_else(|| b.profile.label()),
            profile: b.profile,
            keep_data: b.keep_data,
            keys: b.keys,
            json: b.json,
        }
    }
}

/// Prints progress: to stdout, or to stderr when stdout is for JSON.
fn say(json: bool, text: impl std::fmt::Display) {
    if json {
        eprintln!("{text}");
    } else {
        println!("{text}");
    }
}

/// One line of a block, as [`line`] prints it, by [`say`].
fn say_line(json: bool, label: &str, value: impl std::fmt::Display) {
    say(json, format_args!("  {label:<11}{value}"));
}

/// A label made fit for a file name.
pub(crate) fn sanitize(label: &str) -> String {
    let mut out: String = label
        .trim()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '-' })
        .take(60)
        .collect();
    while out.ends_with(['.', '-']) {
        out.pop();
    }
    out
}

/// The folder reports go to: beside the cache folder.
pub(crate) fn reports_dir(cfg: &victory_core::Config) -> PathBuf {
    crate::state::beside_cache(cfg, "bench")
}

/// A copy of the data folder in a temporary folder, removed when dropped.
pub(crate) struct DataCopy {
    root: PathBuf,
    pub data: PathBuf,
    pub files: u64,
    pub bytes: u64,
}

/// Prefix of the temporary folders of [`DataCopy`] (`clean` lists those
/// left behind).
pub(crate) const COPY_PREFIX: &str = "pilotebox-bench-";

impl DataCopy {
    /// Copies `from` (if it exists) into a new temporary folder.
    pub(crate) fn make(from: &Path, temp: &Path) -> Result<DataCopy> {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let root = temp.join(format!("{COPY_PREFIX}{}-{stamp}", std::process::id()));
        let data = root.join("data");
        victory_core::ensure_outside_repo(&data)?;
        std::fs::create_dir_all(&data).with_context(|| format!("creating {}", data.display()))?;
        let mut copy = DataCopy { root, data, files: 0, bytes: 0 };
        if from.is_dir() {
            copy.fill(from)?;
        }
        Ok(copy)
    }

    /// Everything under `from` but the crash logs (the copy's own then holds
    /// this run's alone).
    fn fill(&mut self, from: &Path) -> Result<()> {
        for entry in walkdir::WalkDir::new(from).min_depth(1) {
            let entry = entry?;
            let to = self.data.join(entry.path().strip_prefix(from)?);
            if entry.file_type().is_dir() {
                std::fs::create_dir_all(&to)?;
            } else if entry.file_type().is_file() && entry.file_name() != "crashes.log" {
                if let Some(parent) = to.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                self.bytes += std::fs::copy(entry.path(), &to).with_context(|| format!("copying {}", entry.path().display()))?;
                self.files += 1;
            }
        }
        Ok(())
    }
}

impl Drop for DataCopy {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// How the run ended.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Ended {
    /// Its window was asked to close and it did.
    Closed,
    /// It had to be ended: no window to close, or it did not close in time.
    Killed,
    /// By itself, before the time was up.
    ByItself,
}

/// A measured run's report.
#[derive(Debug, Clone)]
pub(crate) struct Report {
    pub label: String,
    pub when: String,
    pub mode: Mode,
    pub switches: Vec<String>,
    pub build: &'static str,
    pub game: String,
    pub data: String,
    pub keys: String,
    pub seconds: u64,
    pub ran: f64,
    pub ended: Ended,
    pub code: Option<i32>,
    pub summary: Summary,
    pub problem_lines: Vec<(Problem, Vec<String>)>,
    /// The graphics card, short (`RTX 5090`).
    pub card: Option<String>,
}

/// A count with thousands set apart: 5,123.
pub(crate) fn thousands(n: f64) -> String {
    let digits = format!("{:.0}", n.max(0.0));
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// An exit code as Windows shows it (`0xc0000005`) when it is one of its
/// codes, else as a number.
fn code_text(code: i32) -> String {
    if code < 0 { format!("{:#010x}", code as u32) } else { code.to_string() }
}

impl Report {
    /// The run in one line, for an issue or a table: `PiloteBox 0.12.2 |
    /// RTX 5090 | Series X | 4,800 draws | 84.3 ms (11.9 fps) | card 75 %
    /// busy | 5 min, no crash`.
    pub(crate) fn one_line(&self) -> String {
        let s = &self.summary;
        let mut parts = vec![format!("{} {}", victory_core::product::NAME, victory_core::product::VERSION)];
        parts.extend(self.card.clone());
        let mut mode = self.mode.name().to_owned();
        for switch in &self.switches {
            match switch.as_str() {
                "VICTORY_BVH_BUILDER=1" => mode.push_str(" --rt"),
                "VICTORY_PLAY_DEFAULTS=safe" => mode.push_str(" --safe"),
                _ => {}
            }
        }
        parts.push(mode);
        if self.build != "release" {
            parts.push(format!("{} build", self.build));
        }
        if s.measured > 0 {
            parts.push(format!("{} draws", thousands(s.draws)));
            parts.push(format!("{:.1} ms ({:.1} fps)", s.ms, s.fps));
            if let Some(c) = s.clock {
                parts.push(format!("card {:.0} % busy", c.busy));
            }
        } else if s.reached_world {
            parts.push("the world reached, nothing measured".to_owned());
        } else {
            parts.push("the world not reached".to_owned());
        }
        let length =
            if self.ran >= 120.0 { format!("{:.0} min", self.ran / 60.0) } else { format!("{:.0} s", self.ran) };
        let problems: Vec<&str> =
            Problem::ALL.iter().zip(s.problems).filter(|(_, n)| *n > 0).map(|(p, _)| p.name()).collect();
        let how = match (problems.is_empty(), self.ended) {
            (true, Ended::ByItself) => "ended by itself".to_owned(),
            (true, _) => "no crash".to_owned(),
            (false, _) => problems.join(", "),
        };
        parts.push(format!("{length}, {how}"));
        parts.join(" | ")
    }

    /// The switches by the names the user knows.
    fn shown_switches(&self) -> Vec<String> {
        self.switches.iter().map(|s| victory_core::product::shown(s)).collect()
    }

    pub(crate) fn ended_text(&self) -> String {
        let code = self.code.map_or_else(|| "no exit code".to_owned(), code_text);
        match self.ended {
            Ended::Closed => format!("its window closed after {:.0} s, as asked (exit code {code})", self.ran),
            Ended::Killed => format!("ended after {:.0} s: its window did not close in time, or it had none", self.ran),
            Ended::ByItself => format!("by itself after {:.0} s of {} (exit code {code})", self.ran, self.seconds),
        }
    }

    /// Label and value of each line of the summary.
    pub(crate) fn rows(&self) -> Vec<(&'static str, String)> {
        let s = &self.summary;
        let mut rows = vec![
            ("when", self.when.clone()),
            ("mode", self.mode.name().to_owned()),
            ("switches", if self.switches.is_empty() { "the play defaults".to_owned() } else { self.shown_switches().join(" ") }),
            ("card", self.card.clone().unwrap_or_else(|| "not known".to_owned())),
            ("build", self.build.to_owned()),
            ("game", self.game.clone()),
            ("data", self.data.clone()),
            ("keys", self.keys.clone()),
            ("ended", self.ended_text()),
            (
                "the world after",
                match (s.reached_world, s.to_world) {
                    (true, Some(t)) => format!("{t:.1} s (the first interval of 1,000 draws a frame or more)"),
                    (true, None) => "reached (no time in the log)".to_owned(),
                    _ => "not reached".to_owned(),
                },
            ),
            (
                "measured",
                format!(
                    "{} of {} intervals of the world (one second's frames, every five seconds), {} left out while \
                     pipelines were being made; {} frames",
                    s.measured,
                    s.world_intervals,
                    s.world_intervals - s.measured,
                    thousands(s.frames)
                ),
            ),
        ];
        if s.measured == 0 {
            rows.push(("figures", "none: no interval of the world was measured".to_owned()));
        } else {
            rows.extend([
                ("mean", format!("{:.1} ms a frame, {:.1} fps", s.ms, s.fps)),
                (
                    "worst interval",
                    s.worst.map_or_else(String::new, |w| match w.at {
                        Some(at) => format!("{:.1} ms a frame ({:.1} fps), {at:.0} s in", w.ms, w.fps),
                        None => format!("{:.1} ms a frame ({:.1} fps)", w.ms, w.fps),
                    }),
                ),
                (
                    "preparing",
                    format!(
                        "{:.1} ms a frame (copies {:.1} ms {:.1} MB, textures {:.1} ms {:.1} MB)",
                        s.preparing, s.copies_ms, s.copies_mb, s.textures_ms, s.textures_mb
                    ),
                ),
                ("card wait", format!("{:.1} ms a frame", s.gpu)),
                ("copy-back", format!("{:.1} ms a frame, {:.1} MB", s.back_ms, s.back_mb)),
                ("draws", format!("{} a frame", thousands(s.draws))),
                ("dropped draws", format!("{} a frame", thousands(s.dropped))),
            ]);
            for (why, n) in s.reasons.iter().take(10) {
                rows.push(("  dropped for", format!("{} a frame: {why}", thousands(*n))));
            }
            if s.dropped >= 0.5 && s.reasons.is_empty() {
                rows.push(("  dropped for", "(reasons: a diagnostics build logs them)".to_owned()));
            }
            if let Some(c) = s.clock {
                rows.push((
                    "card's clock",
                    format!(
                        "the card about {:.0}% busy (nothing of ours queued {:.1} ms a frame, {:.1} ms of it while the \
                         submitting thread worked; it stood waiting for the card {:.1} ms)",
                        c.busy, c.idle_ms, c.behind_ms, c.stood_ms
                    ),
                ));
            }
        }
        let problems: Vec<String> = Problem::ALL
            .iter()
            .zip(s.problems)
            .filter(|(_, n)| *n > 0)
            .map(|(p, n)| format!("{} ({n} lines)", p.name()))
            .collect();
        rows.push(("problems", if problems.is_empty() { "none seen".to_owned() } else { problems.join(", ") }));
        rows
    }

    /// The summary as printed; the one-line summary last.
    pub(crate) fn text(&self) -> String {
        let mut out = format!("{} bench {}\n", victory_core::product::NAME, self.label);
        for (label, value) in self.rows() {
            out.push_str(&format!("  {label:<16}{value}\n"));
        }
        out.push_str(&format!("\n{}\n", self.one_line()));
        out
    }

    pub(crate) fn markdown(&self) -> String {
        let cell = |s: &str| s.replace('|', "\\|");
        let mut out = format!(
            "# {} bench: {}\n\n{} {}\n\n| | |\n|---|---|\n",
            victory_core::product::NAME,
            self.label,
            victory_core::product::NAME,
            victory_core::product::VERSION
        );
        for (label, value) in self.rows() {
            out.push_str(&format!("| {} | {} |\n", cell(label.trim()), cell(&value)));
        }
        out.push_str(&format!("\nIn one line:\n\n    {}\n", self.one_line()));
        for (p, lines) in &self.problem_lines {
            out.push_str(&format!("\n## {}\n\n```\n", p.name()));
            for l in lines {
                out.push_str(l);
                out.push('\n');
            }
            out.push_str("```\n");
        }
        out
    }

    pub(crate) fn json(&self) -> String {
        let s = &self.summary;
        let num = |n: f64| if n.is_finite() { format!("{:.2}", n) } else { "null".to_owned() };
        let opt = |n: Option<f64>| n.map_or_else(|| "null".to_owned(), num);
        let list = |items: &[String]| items.iter().map(|i| json_string(i)).collect::<Vec<_>>().join(", ");
        let mut fields: Vec<(&str, String)> = vec![
            ("product", json_string(&format!("{} {}", victory_core::product::NAME, victory_core::product::VERSION))),
            ("line", json_string(&self.one_line())),
            ("label", json_string(&self.label)),
            ("when", json_string(&self.when)),
            ("card", self.card.as_deref().map_or_else(|| "null".to_owned(), json_string)),
            ("mode", json_string(self.mode.name())),
            ("switches", format!("[{}]", list(&self.shown_switches()))),
            ("build", json_string(self.build)),
            ("game", json_string(&self.game)),
            ("data", json_string(&self.data)),
            ("keys", json_string(&self.keys)),
            ("seconds", self.seconds.to_string()),
            ("ran_seconds", num(self.ran)),
            ("ended", json_string(&self.ended_text())),
            ("exit_code", self.code.map_or_else(|| "null".to_owned(), |c| c.to_string())),
            ("reached_world", s.reached_world.to_string()),
            ("to_world_seconds", opt(s.to_world)),
            ("intervals", s.intervals.to_string()),
            ("world_intervals", s.world_intervals.to_string()),
            ("measured_intervals", s.measured.to_string()),
            ("frames", num(s.frames)),
            ("ms_per_frame", num(s.ms)),
            ("fps", num(s.fps)),
            (
                "worst",
                s.worst.map_or_else(
                    || "null".to_owned(),
                    |w| format!("{{\"ms_per_frame\": {}, \"fps\": {}, \"at_seconds\": {}}}", num(w.ms), num(w.fps), opt(w.at)),
                ),
            ),
            ("preparing_ms", num(s.preparing)),
            ("copies_ms", num(s.copies_ms)),
            ("copies_mb", num(s.copies_mb)),
            ("textures_ms", num(s.textures_ms)),
            ("textures_mb", num(s.textures_mb)),
            ("card_wait_ms", num(s.gpu)),
            ("copy_back_ms", num(s.back_ms)),
            ("copy_back_mb", num(s.back_mb)),
            ("draws", num(s.draws)),
            ("dropped", num(s.dropped)),
            (
                "card_clock",
                s.clock.map_or_else(
                    || "null".to_owned(),
                    |c| {
                        format!(
                            "{{\"busy_percent\": {}, \"idle_ms\": {}, \"behind_ms\": {}, \"stood_ms\": {}}}",
                            num(c.busy),
                            num(c.idle_ms),
                            num(c.behind_ms),
                            num(c.stood_ms)
                        )
                    },
                ),
            ),
            (
                "dropped_by_reason",
                format!(
                    "[{}]",
                    s.reasons
                        .iter()
                        .map(|(why, n)| format!("{{\"reason\": {}, \"per_frame\": {}}}", json_string(why), num(*n)))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ),
        ];
        let problems: Vec<String> = Problem::ALL
            .iter()
            .zip(s.problems)
            .map(|(p, n)| format!("{}: {n}", json_string(&p.name().replace(' ', "_"))))
            .collect();
        fields.push(("problems", format!("{{{}}}", problems.join(", "))));
        let body: Vec<String> = fields.iter().map(|(k, v)| format!("  {}: {v}", json_string(k))).collect();
        format!("{{\n{}\n}}\n", body.join(",\n"))
    }
}

/// `s` as a JSON string.
pub(crate) fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A measured run: its report, everything read from its lines, and the
/// files written.
pub(crate) struct Outcome {
    pub report: Report,
    pub tally: Tally,
    pub files: Vec<PathBuf>,
}

#[derive(Clone, Copy)]
enum Stream {
    Out,
    Err,
}

/// Sends `stream`'s lines to `to` until it ends.
fn reader(stream: impl Read + Send + 'static, kind: Stream, to: mpsc::Sender<(Stream, String)>) {
    std::thread::spawn(move || {
        let mut stream = BufReader::new(stream);
        let mut bytes = Vec::new();
        loop {
            bytes.clear();
            match stream.read_until(b'\n', &mut bytes) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let text = String::from_utf8_lossy(&bytes).trim_end_matches(['\r', '\n']).to_owned();
                    if to.send((kind, text)).is_err() {
                        break;
                    }
                }
            }
        }
    });
}

/// The log filter the run gets: the user's, with the frame lines at info.
fn log_filter() -> String {
    let theirs = std::env::var("VICTORY_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .ok()
        .filter(|f| !f.trim().is_empty())
        .unwrap_or_else(|| "info".to_owned());
    format!("{theirs},victory_gpu::host=info")
}

/// The local time as a file name's start and as text.
fn stamp() -> (String, String) {
    let [y, mo, d, h, mi, s] = crate::sys::local_time();
    (format!("{y:04}-{mo:02}-{d:02}_{h:02}{mi:02}{s:02}"), format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}"))
}

/// Runs the game for `o.seconds` and reads its lines; prints its progress.
pub(crate) fn measure(o: &Options) -> Result<Outcome> {
    let cfg = victory_core::Config::load()?;
    let found = game::find(&Places::new(o.profile.game.as_deref(), &cfg))?;
    let reports = reports_dir(&cfg);
    victory_core::ensure_outside_repo(&reports)?;
    std::fs::create_dir_all(&reports).with_context(|| format!("creating {}", reports.display()))?;
    let (file_stamp, when) = stamp();
    let stem = format!("{file_stamp}-{}", o.label);
    let log_path = reports.join(format!("{stem}.log"));
    let copy = if o.keep_data { None } else { Some(DataCopy::make(&cfg.data_dir, &std::env::temp_dir())?) };
    let data_dir = copy.as_ref().map_or(cfg.data_dir.clone(), |c| c.data.clone());
    let data = match &copy {
        Some(c) => format!(
            "a copy of {} ({} files, {:.1} MB), removed at the end",
            cfg.data_dir.display(),
            c.files,
            c.bytes as f64 / 1e6
        ),
        None => format!("{} itself (--keep-data)", cfg.data_dir.display()),
    };
    let keys = match o.keys.as_deref().map(str::trim) {
        Some("none") | Some("") => None,
        Some(keys) => Some(keys.to_owned()),
        None => Some(std::env::var("VICTORY_KEYS").unwrap_or_else(|_| DEFAULT_KEYS.to_owned())),
    };
    let switches: Vec<String> = o.profile.switches().iter().map(|(n, v, _)| format!("{n}={v}")).collect();
    let card = crate::doctor::card_name();

    let json = o.json;
    say(json, victory_core::product::heading(&format!("bench {}", o.label)));
    say_line(json, "length", format!("{} s, then the window is closed", o.seconds));
    say_line(json, "mode", o.profile.mode.name());
    say_line(json, "card", card.as_deref().unwrap_or("not known"));
    say_line(json, "game", format!("{} ({})", found.exe.display(), found.from));
    for note in found.notes.iter().cloned().chain(crate::play::legacy_note(&cfg)) {
        say_line(json, "note", note);
    }
    say_line(json, "data", &data);
    say_line(json, "cache", cfg.cache_dir.display());
    say_line(json, "keys", keys.as_deref().unwrap_or("none"));
    if !switches.is_empty() {
        let shown: Vec<String> = switches.iter().map(|s| victory_core::product::shown(s)).collect();
        say_line(json, "switches", shown.join(" "));
    }
    say_line(json, "log", log_path.display());

    let mut command = Command::new(std::env::current_exe().context("finding this program")?);
    command.arg("run").arg(&found.exe).arg("--execute");
    if o.profile.mode == Mode::S {
        command.args(["--console", "series-s"]);
    }
    for (name, value, _) in o.profile.switches() {
        command.env(name, value);
    }
    if copy.is_some() {
        command.env("VICTORY_DATA_DIR", &data_dir);
    }
    match &keys {
        Some(keys) => command.env("VICTORY_KEYS", keys),
        None => command.env_remove("VICTORY_KEYS"),
    };
    command
        .env("VICTORY_LOG", log_filter())
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut log = std::io::BufWriter::new(
        std::fs::File::create(&log_path).with_context(|| format!("creating {}", log_path.display()))?,
    );
    let start_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64());
    let started = Instant::now();
    let mut child = command.spawn().context("starting the run")?;
    let (to, lines) = mpsc::channel();
    reader(child.stdout.take().context("the run's output")?, Stream::Out, to.clone());
    reader(child.stderr.take().context("the run's log")?, Stream::Err, to);

    let deadline = started + Duration::from_secs(o.seconds);
    let mut tally = Tally::default();
    let mut world = false;
    // When the window was asked to close, and whether the run was ended.
    let mut closing: Option<Instant> = None;
    let mut killed = false;
    let mut by_itself = false;
    loop {
        let now = Instant::now();
        let until = match (closing, killed) {
            (None, _) => deadline,
            (Some(at), false) => at + GRACE,
            (Some(at), true) => at + GRACE + Duration::from_secs(10),
        };
        match lines.recv_timeout(until.saturating_duration_since(now).max(Duration::from_millis(10))) {
            Ok((stream, text)) => {
                let text = plain(&text);
                let _ = writeln!(log, "{text}");
                let seen = tally.feed(&text);
                let at = started.elapsed().as_secs_f64();
                match (stream, seen) {
                    (Stream::Out, _) => say(json, format_args!("  | {text}")),
                    (_, Seen::Frame) => {
                        let i = tally.intervals.last().expect("a frame line was just read");
                        let first = i.world() && !world;
                        world |= i.world();
                        say(
                            json,
                            format_args!(
                                "  {at:>5.0} s  {:>5.1} fps {:>7.1} ms/frame {:>6} draws, dropped {}{}",
                                i.fps,
                                i.ms,
                                thousands(i.draws),
                                thousands(i.dropped),
                                if first { "  <- the world" } else { "" }
                            ),
                        );
                    }
                    (_, Seen::Problem(p)) => say(
                        json,
                        format_args!("  {at:>5.0} s  {}: {}", p.name(), text.chars().take(200).collect::<String>()),
                    ),
                    _ => {}
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let target = tally.relaunched.unwrap_or(child.id());
                let at = started.elapsed().as_secs_f64();
                if closing.is_none() {
                    let asked = crate::sys::close_windows(target);
                    closing = Some(Instant::now());
                    if asked > 0 {
                        say(json, format_args!("  {at:>5.0} s  time is up: the window is asked to close"));
                        continue;
                    }
                    say(json, format_args!("  {at:>5.0} s  time is up, and the run has no window to close: it is ended"));
                } else if !killed {
                    say(json, format_args!("  {at:>5.0} s  the run did not close in {} s: it is ended", GRACE.as_secs()));
                } else {
                    // Its output is held by something else: done all the same.
                    break;
                }
                killed = true;
                if target != child.id() {
                    crate::sys::kill(target);
                }
                let _ = child.kill();
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                by_itself = closing.is_none();
                break;
            }
        }
    }
    let ran = started.elapsed().as_secs_f64();
    let _ = log.flush();
    if by_itself {
        say(json, format_args!("  {ran:>5.0} s  the run ended by itself"));
    }
    // Ends a run that kept going after its output closed.
    if !matches!(child.try_wait(), Ok(Some(_))) && !by_itself {
        let _ = child.kill();
    }
    let code = child.wait().ok().and_then(|s| s.code());

    // This run's crash log: the copy's is its own (the copy has none of
    // the real one's); kept beside the report.
    let mut crashes_kept = None;
    if let Some(copy) = &copy {
        let crashes = game::crash_log(&copy.data, &found.exe);
        if std::fs::metadata(&crashes).is_ok_and(|m| m.len() > 0) {
            let kept = reports.join(format!("{stem}-crashes.log"));
            if std::fs::copy(&crashes, &kept).is_ok() {
                crashes_kept = Some(kept);
            }
        }
    }
    drop(copy);

    let summary = tally.summary(Some(start_unix));
    let report = Report {
        label: o.label.clone(),
        when,
        mode: o.profile.mode,
        switches,
        build: if cfg!(feature = "diagnostics") { "diagnostics" } else { "release" },
        game: found.exe.display().to_string(),
        data,
        keys: keys.unwrap_or_else(|| "none".to_owned()),
        seconds: o.seconds,
        ran,
        ended: if by_itself {
            Ended::ByItself
        } else if killed {
            Ended::Killed
        } else {
            Ended::Closed
        },
        code,
        problem_lines: Problem::ALL
            .iter()
            .filter(|p| tally.count(**p) > 0)
            .map(|p| (*p, tally.lines(*p).to_vec()))
            .collect(),
        summary,
        card,
    };
    let mut files = Vec::new();
    for (extension, text) in [("md", report.markdown()), ("json", report.json())] {
        let path = reports.join(format!("{stem}.{extension}"));
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        files.push(path);
    }
    files.push(log_path);
    files.extend(crashes_kept);
    Ok(Outcome { report, tally, files })
}

pub(crate) fn run(b: Bench) -> Result<()> {
    let options = Options::from(b);
    let outcome = measure(&options)?;
    if options.json {
        for (i, file) in outcome.files.iter().enumerate() {
            say_line(true, if i == 0 { "written" } else { "" }, file.display());
        }
        print!("{}", outcome.report.json());
        return Ok(());
    }
    println!();
    let text = outcome.report.text();
    let (summary, one_line) = text.trim_end().rsplit_once('\n').unwrap_or((&text, ""));
    println!("{}", summary.trim_end());
    println!();
    for (i, file) in outcome.files.iter().enumerate() {
        line(if i == 0 { "written" } else { "" }, file.display());
    }
    // Last, to paste into an issue.
    println!();
    println!("{one_line}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frames::Tally;

    fn report() -> Report {
        let mut tally = Tally::default();
        for line in [
            "2026-11-26T19:34:25.837067Z  INFO victory_hle::modules::xmem::low_guards: space below 4 GiB kept for the title",
            "2026-11-26T19:35:27.219370Z  INFO victory_gpu::host: the card's clock per frame: nothing of ours queued 113.2 ms (the card about 31% busy by this count), 70.0 ms of that while the submitting thread was working (the card idle behind us), the rest while the title had not submitted; the submitting thread stood waiting for the card 7.1 ms",
            "2026-11-26T19:35:27.220565Z  INFO victory_gpu::host: 6.1 fps, 163.4 ms/frame (96.9 preparing: copies 29.8 ms 142.3 MB, textures 9.0 ms 251.4 MB; 7.1 on the GPU; 22.9 copying back 52.4 MB), 2985 draws/frame, dropped 1179 without a host pipeline (0 being made) and 622 indirect",
            "2026-11-26T19:35:32.537556Z  INFO victory_gpu::host: 10.5 fps, 94.8 ms/frame (58.3 preparing: copies 25.1 ms 107.4 MB, textures 18.7 ms 175.5 MB; 12.9 on the GPU; 25.5 copying back 36.0 MB), 1715 draws/frame, dropped 503 without a host pipeline (0 being made) and 614 indirect",
            "2026-11-26T19:35:33Z  WARN victory_gpu::pipeline::host: host pipeline refused error=\"E_INVALIDARG | x\"",
        ] {
            tally.feed(line);
        }
        Report {
            label: "series-x".into(),
            when: "2026-11-27 15:30:00".into(),
            mode: Mode::X,
            switches: vec!["VICTORY_BVH_BUILDER=1".into()],
            build: "release",
            game: r"D:\games\title\title.exe".into(),
            data: "a copy | two".into(),
            keys: DEFAULT_KEYS.into(),
            seconds: 300,
            ran: 301.4,
            ended: Ended::Closed,
            code: Some(0),
            problem_lines: vec![(Problem::PipelineRefused, tally.lines(Problem::PipelineRefused).to_vec())],
            summary: tally.summary(None),
            card: Some("RTX 5090".into()),
        }
    }

    #[test]
    fn the_report_says_the_figures() {
        let text = report().text();
        for wanted in [
            "the world after 61.4 s",
            "2 of 2 intervals",
            "mean            ",
            "worst interval  163.4 ms a frame (6.1 fps), 61 s in",
            "card wait       ",
            "copy-back       ",
            "draws           2,182 a frame",
            "the card about ",
            "pipeline refused (1 lines)",
            "its window closed after 301 s",
            "switches        PILOTEBOX_BVH_BUILDER=1",
            "card            RTX 5090",
        ] {
            assert!(text.contains(wanted), "{wanted:?} not in\n{text}");
        }
    }

    #[test]
    fn the_run_fits_in_one_line() {
        let mut r = report();
        let ms = (6.1 * 163.4 + 10.5 * 94.8) / 16.6;
        let busy = (6.1 * 31.0 + 10.5 * 0.0) / 6.1;
        assert_eq!(
            r.one_line(),
            format!(
                "PiloteBox 0.12.2 | RTX 5090 | Series X --rt | 2,182 draws | {ms:.1} ms ({:.1} fps) | card {busy:.0} % busy | \
                 5 min, pipeline refused",
                1000.0 / ms
            )
        );
        assert!(r.text().trim_end().ends_with(&r.one_line()));
        assert!(r.json().contains(&format!("\"line\": {}", json_string(&r.one_line()))));
        // A short run that never reached the world, cleanly.
        r.summary = Tally::default().summary(None);
        r.ran = 90.0;
        r.card = None;
        r.build = "diagnostics";
        assert_eq!(
            r.one_line(),
            "PiloteBox 0.12.2 | Series X --rt | diagnostics build | the world not reached | 90 s, no crash"
        );
    }

    #[test]
    fn the_markdown_and_the_json_hold_the_same() {
        let r = report();
        let md = r.markdown();
        assert!(md.starts_with("# PiloteBox bench: series-x"));
        assert!(md.contains("| mean | "));
        assert!(md.contains("| data | a copy \\| two |"), "{md}");
        assert!(md.contains("## pipeline refused\n\n```\n2026-11-26T19:35:33Z"), "{md}");
        assert!(md.contains("E_INVALIDARG | x"), "{md}");
        let json = r.json();
        assert!(json.contains("\"game\": \"D:\\\\games\\\\title\\\\title.exe\""), "{json}");
        assert!(json.contains("\"measured_intervals\": 2,"));
        assert!(json.contains("\"to_world_seconds\": 61.38,"));
        assert!(json.contains("\"pipeline_refused\": 1"));
        assert!(json.contains("\"worst\": {\"ms_per_frame\": 163.40, \"fps\": 6.10, \"at_seconds\": 61.38}"));
        // Balanced, and every line but the braces a field.
        assert_eq!(json.matches('{').count(), json.matches('}').count());
        assert!(json.trim_end().ends_with('}'));
    }

    #[test]
    fn numbers_and_labels_are_written_plainly() {
        assert_eq!(thousands(0.0), "0");
        assert_eq!(thousands(999.4), "999");
        assert_eq!(thousands(4_812.0), "4,812");
        assert_eq!(thousands(1_234_567.0), "1,234,567");
        assert_eq!(code_text(0), "0");
        assert_eq!(code_text(0xc000_0005_u32 as i32), "0xc0000005");
        assert_eq!(sanitize(" my run: rt/on. "), "my-run--rt-on");
        assert_eq!(json_string("a\"b\\c\n"), "\"a\\\"b\\\\c\\n\"");
    }

    #[test]
    fn the_data_copy_leaves_the_crash_log_and_goes_away() {
        let real = tempfile::tempdir().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let title = real.path().join("12AB34CD");
        std::fs::create_dir_all(title.join("saves/0001")).unwrap();
        std::fs::write(title.join("saves/0001/slot"), b"a save").unwrap();
        std::fs::write(title.join("crashes.log"), b"an old crash").unwrap();
        let copy = DataCopy::make(real.path(), temp.path()).unwrap();
        assert!(copy.data.starts_with(temp.path()));
        assert_eq!((copy.files, copy.bytes), (1, 6));
        assert_eq!(std::fs::read(copy.data.join("12AB34CD/saves/0001/slot")).unwrap(), b"a save");
        assert!(!copy.data.join("12AB34CD/crashes.log").exists());
        let root = copy.root.clone();
        drop(copy);
        assert!(!root.exists());
        // The real folder is as it was.
        assert_eq!(std::fs::read(title.join("crashes.log")).unwrap(), b"an old crash");
        // No data folder yet: an empty copy.
        let none = DataCopy::make(&real.path().join("absent"), temp.path()).unwrap();
        assert_eq!(none.files, 0);
    }

    #[test]
    fn reports_go_beside_the_cache() {
        let cfg = victory_core::Config {
            cache_dir: PathBuf::from(r"D:\x\cache"),
            ..Default::default()
        };
        assert_eq!(reports_dir(&cfg), PathBuf::from(r"D:\x\bench"));
    }
}
