//! `pilotebox check`: `doctor`, then a short `bench` on a copy of the data
//! folder, ending with PASS or FAIL: the world was reached, the device was
//! not removed, nothing panicked or crashed, the driver refused no pipeline,
//! the run lasted its time; the dropped draws named by reason.

use anyhow::Result;

use crate::bench::{self, Ended, Options, Outcome};
use crate::doctor::{self, Level};
use crate::frames::Problem;
use crate::game::Profile;
use crate::play::line;

#[derive(clap::Args, Debug)]
pub(crate) struct Check {
    /// How long the run lasts, in seconds.
    #[arg(long, default_value_t = 90, value_name = "N")]
    pub seconds: u64,
    #[command(flatten)]
    pub profile: Profile,
    /// Keys pressed by script, as for bench.
    #[arg(long, value_name = "KEYS")]
    pub keys: Option<String>,
}

/// DXGI_ERROR_DEVICE_REMOVED, the code a run ends with when the card goes.
const DEVICE_REMOVED: i32 = 0x887A_0005_u32 as i32;

/// Rust's exit code after a panic on the main thread.
const PANICKED: i32 = 101;

/// Each condition of a pass, and whether it held.
pub(crate) fn verdict(outcome: &Outcome) -> Vec<(bool, String)> {
    let r = &outcome.report;
    let s = &r.summary;
    let count = |p: Problem| outcome.tally.count(p);
    let code = r.code;
    vec![
        (
            s.reached_world,
            match s.to_world {
                Some(t) if s.reached_world => format!("the world was reached after {t:.0} s"),
                _ if s.reached_world => "the world was reached".to_owned(),
                _ => format!("the world was not reached in {} s", r.seconds),
            },
        ),
        (
            count(Problem::DeviceRemoved) == 0 && code != Some(DEVICE_REMOVED),
            format!("device removals: {}", count(Problem::DeviceRemoved) + usize::from(code == Some(DEVICE_REMOVED))),
        ),
        (
            count(Problem::Panic) == 0 && code != Some(PANICKED),
            format!("panics: {}", count(Problem::Panic).max(usize::from(code == Some(PANICKED)))),
        ),
        (count(Problem::Crash) == 0, format!("crashes: {}", count(Problem::Crash))),
        (count(Problem::PipelineRefused) == 0, format!("pipelines refused: {}", count(Problem::PipelineRefused))),
        (r.ended != Ended::ByItself, format!("ended: {}", r.ended_text())),
    ]
}

pub(crate) fn run(c: Check) -> Result<()> {
    println!("{} check", victory_core::product::banner());
    let checks = doctor::checks(c.profile.game.as_deref());
    doctor::print(&checks);
    if checks.iter().any(|c| c.level == Level::Problem) {
        println!("FAIL: doctor found a problem (above); no run was started");
        std::process::exit(1);
    }
    println!();
    let outcome = bench::measure(&Options {
        seconds: c.seconds,
        label: format!("check-{}", c.profile.label()),
        profile: c.profile,
        keep_data: false,
        keys: c.keys,
    })?;
    println!();
    print!("{}", outcome.report.text());
    println!();
    let verdict = verdict(&outcome);
    for (ok, text) in &verdict {
        line(if *ok { "ok" } else { "FAILED" }, text);
    }
    let s = &outcome.report.summary;
    if s.reasons.is_empty() {
        line(
            "dropped",
            format!(
                "{:.0} draws a frame{}",
                s.dropped,
                if s.dropped >= 0.5 { " (a diagnostics build names the reasons)" } else { "" }
            ),
        );
    } else {
        for (why, n) in &s.reasons {
            line("dropped", format!("{n:.0} draws a frame: {why}"));
        }
    }
    for file in &outcome.files {
        line("written", file.display());
    }
    if verdict.iter().all(|(ok, _)| *ok) {
        println!("PASS");
        Ok(())
    } else {
        println!("FAIL");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench::Report;
    use crate::frames::Tally;
    use crate::game::Mode;

    fn outcome(lines: &[&str], ended: Ended, code: Option<i32>) -> Outcome {
        let mut tally = Tally::default();
        for l in lines {
            tally.feed(l);
        }
        let report = Report {
            label: "check".into(),
            when: String::new(),
            mode: Mode::X,
            switches: Vec::new(),
            build: "release",
            game: String::new(),
            data: String::new(),
            keys: String::new(),
            seconds: 90,
            ran: 91.0,
            ended,
            code,
            summary: tally.summary(None),
            problem_lines: Vec::new(),
        };
        Outcome { report, tally, files: Vec::new() }
    }

    const START: &str = "2026-11-26T19:34:25.837067Z  INFO victory_hle::modules::xmem::low_guards: space below 4 GiB kept";
    const WORLD: &str = "2026-11-26T19:35:27.220565Z  INFO victory_gpu::host: 6.1 fps, 163.4 ms/frame (96.9 preparing: copies 29.8 ms 142.3 MB, textures 9.0 ms 251.4 MB; 7.1 on the GPU; 22.9 copying back 52.4 MB), 2985 draws/frame, dropped 1179 without a host pipeline (0 being made) and 622 indirect";

    #[test]
    fn a_run_that_reaches_the_world_cleanly_passes() {
        let verdict = verdict(&outcome(&[START, WORLD], Ended::Closed, Some(0)));
        assert!(verdict.iter().all(|(ok, _)| *ok), "{verdict:?}");
        assert_eq!(verdict[0].1, "the world was reached after 61 s");
    }

    #[test]
    fn each_failure_is_named() {
        let fails = |o: &Outcome| verdict(o).into_iter().filter(|(ok, _)| !ok).map(|(_, t)| t).collect::<Vec<_>>();
        assert_eq!(fails(&outcome(&[START], Ended::Closed, Some(0))), ["the world was not reached in 90 s"]);
        let removed = outcome(&[START, WORLD], Ended::ByItself, Some(DEVICE_REMOVED));
        let failed = fails(&removed);
        assert_eq!(failed.len(), 2, "{failed:?}");
        assert_eq!(failed[0], "device removals: 1");
        assert!(failed[1].starts_with("ended: by itself after 91 s of 90"), "{failed:?}");
        let refused = outcome(&[START, WORLD, "x WARN victory_gpu::pipeline::host: host pipeline refused"], Ended::Closed, Some(0));
        assert_eq!(fails(&refused), ["pipelines refused: 1"]);
        let panicked = outcome(&[START, WORLD, "thread 'main' panicked at src/x.rs:1:1:"], Ended::Closed, Some(0));
        assert_eq!(fails(&panicked), ["panics: 1"]);
    }
}
