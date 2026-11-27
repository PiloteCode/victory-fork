//! The layer's own frame lines, read back (`bench`, `check`).
//!
//! The frame meter (`crates/victory-gpu/src/host.rs`, `Meter::frame`) logs,
//! at info level in every build, every five seconds, over the last second's
//! frames:
//!
//! `host: X fps, Y ms/frame (Z preparing: copies .. ms .. MB, textures .. ms
//! .. MB; W on the GPU; V copying back .. MB), N draws/frame, dropped M
//! without a host pipeline (K being made) ...`
//!
//! and every second the card's clock line (`the card's clock per frame:
//! ...`). Diagnostics builds add the dropped draws' reasons after the frame
//! line (`dropped 1.0/frame without a host pipeline: <why>`). Other lines
//! tell that a run went wrong: a panic, a crash, the device removed, a
//! pipeline the driver refused.

use std::borrow::Cow;

/// Draws a frame from which an interval shows the title's world (not a
/// menu, a video or a loading screen).
pub(crate) const WORLD_DRAWS: f64 = 1000.0;

/// One frame line: the frame meter's figures over one second.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Interval {
    /// Unix time of the line, when it has one.
    pub at: Option<f64>,
    pub fps: f64,
    pub ms: f64,
    /// Ms a frame preparing submissions, and of it: title memory copies (ms,
    /// MB) and texture refreshes (ms, MB).
    pub preparing: f64,
    pub copies_ms: f64,
    pub copies_mb: f64,
    pub textures_ms: f64,
    pub textures_mb: f64,
    /// Ms a frame waiting for the card.
    pub gpu: f64,
    /// Ms and MB a frame copying back what shaders wrote.
    pub back_ms: f64,
    pub back_mb: f64,
    pub draws: f64,
    /// Draws a frame dropped without a host pipeline.
    pub dropped: f64,
    /// Pipelines being made when the line was logged.
    pub making: f64,
    /// The card's clock line of the same second.
    pub clock: Option<Clock>,
    /// The dropped draws' reasons logged with it (diagnostics builds), per
    /// frame.
    pub reasons: Vec<(String, f64)>,
}

impl Interval {
    /// Whether it shows the world.
    pub(crate) fn world(&self) -> bool {
        self.draws >= WORLD_DRAWS
    }

    /// The frames it counted: a second's worth, or one frame when that
    /// took longer.
    fn frames(&self) -> f64 {
        (self.fps * (self.ms / 1000.0).max(1.0)).max(1.0)
    }
}

/// The card's clock line: ms a frame with nothing of ours queued, how busy
/// that makes the card, ms of it the submitting thread was working, and ms
/// the submitting thread stood waiting for the card.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Clock {
    pub at: Option<f64>,
    pub idle_ms: f64,
    pub busy: f64,
    pub behind_ms: f64,
    pub stood_ms: f64,
}

/// What went wrong in a run, by the lines that tell it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Problem {
    Panic,
    Crash,
    DeviceRemoved,
    PipelineRefused,
}

impl Problem {
    pub(crate) const ALL: [Problem; 4] = [Problem::Panic, Problem::Crash, Problem::DeviceRemoved, Problem::PipelineRefused];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Problem::Panic => "panic",
            Problem::Crash => "crash",
            Problem::DeviceRemoved => "device removed",
            Problem::PipelineRefused => "pipeline refused",
        }
    }
}

/// `line` without its terminal colour codes.
pub(crate) fn plain(line: &str) -> Cow<'_, str> {
    if !line.contains('\x1b') {
        return Cow::Borrowed(line);
    }
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        // `ESC [ parameters final`, the final byte in `@`..=`~`.
        if chars.clone().next() == Some('[') {
            chars.next();
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        }
    }
    Cow::Owned(out)
}

/// Unix time of a log line, from its first word (`2026-11-26T19:34:36.009817Z`).
pub(crate) fn timestamp(line: &str) -> Option<f64> {
    let word = line.split_whitespace().next()?;
    let (date, time) = word.strip_suffix('Z')?.split_once('T')?;
    let mut date = date.split('-').map(|p| p.parse::<i64>().ok());
    let (year, month, day) = (date.next()??, date.next()??, date.next()??);
    let mut time = time.split(':');
    let hour: f64 = time.next()?.parse().ok()?;
    let minute: f64 = time.next()?.parse().ok()?;
    let second: f64 = time.next()?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day) as f64 * 86_400.0 + hour * 3600.0 + minute * 60.0 + second)
}

/// Days since 1970-01-01 of a date of the Gregorian calendar.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let of_era = year - era * 400;
    let of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let of_cycle = of_era * 365 + of_era / 4 - of_era / 100 + of_year;
    era * 146_097 + of_cycle - 719_468
}

/// The numbers of `text` laid out as `pieces`: `text` starts with the first
/// piece, then each number is followed by the next piece.
fn numbers(text: &str, pieces: &[&str]) -> Option<Vec<f64>> {
    let mut rest = text.strip_prefix(*pieces.first()?)?;
    let mut out = Vec::with_capacity(pieces.len() - 1);
    for piece in &pieces[1..] {
        let len = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
            .unwrap_or(rest.len());
        out.push(rest[..len].parse().ok()?);
        rest = rest[len..].strip_prefix(piece)?;
    }
    Some(out)
}

/// The frame line's layout after `host: `.
const FRAME: &[&str] = &[
    "",
    " fps, ",
    " ms/frame (",
    " preparing: copies ",
    " ms ",
    " MB, textures ",
    " ms ",
    " MB; ",
    " on the GPU; ",
    " copying back ",
    " MB), ",
    " draws/frame, dropped ",
    " without a host pipeline (",
    " being made)",
];

/// The figures of a frame line (its time left out).
pub(crate) fn frame_line(line: &str) -> Option<Interval> {
    let n = line
        .match_indices("host: ")
        .find_map(|(at, marker)| numbers(&line[at + marker.len()..], FRAME))?;
    Some(Interval {
        fps: n[0],
        ms: n[1],
        preparing: n[2],
        copies_ms: n[3],
        copies_mb: n[4],
        textures_ms: n[5],
        textures_mb: n[6],
        gpu: n[7],
        back_ms: n[8],
        back_mb: n[9],
        draws: n[10],
        dropped: n[11],
        making: n[12],
        ..Interval::default()
    })
}

/// The figures of the card's clock line (its time left out).
pub(crate) fn clock_line(line: &str) -> Option<Clock> {
    const MARKER: &str = "the card's clock per frame: nothing of ours queued ";
    let at = line.find(MARKER)?;
    let n = numbers(
        &line[at + MARKER.len()..],
        &["", " ms (the card about ", "% busy by this count), ", " ms of that"],
    )?;
    const STOOD: &str = "stood waiting for the card ";
    let stood = line
        .find(STOOD)
        .and_then(|at| numbers(&line[at + STOOD.len()..], &["", " ms"]))
        .map_or(0.0, |n| n[0]);
    Some(Clock {
        at: None,
        idle_ms: n[0],
        busy: n[1],
        behind_ms: n[2],
        stood_ms: stood,
    })
}

/// A dropped draws' reason line: draws a frame and the reason.
pub(crate) fn drop_reason(line: &str) -> Option<(f64, String)> {
    const PIECE: &str = "/frame without a host pipeline: ";
    let end = line.find(PIECE)?;
    let start = line[..end].rfind("dropped ")? + "dropped ".len();
    let per_frame = line[start..end].parse().ok()?;
    Some((per_frame, line[end + PIECE.len()..].trim().to_owned()))
}

/// What went wrong, if `line` tells it.
pub(crate) fn problem(line: &str) -> Option<Problem> {
    if line.contains("panicked at") || line.contains("VIctory: panic on thread") {
        return Some(Problem::Panic);
    }
    if line.contains("VIctory: unhandled exception") {
        return Some(Problem::Crash);
    }
    let lower = line.to_ascii_lowercase();
    // DXGI_ERROR_DEVICE_REMOVED, _HUNG, _RESET.
    let removal_code = ["0x887a0005", "0x887a0006", "0x887a0007"].iter().any(|c| lower.contains(c));
    if line.contains("device removed") || line.contains("host GPU failure") || line.contains("when the host device went") || removal_code
    {
        return Some(Problem::DeviceRemoved);
    }
    if line.contains("pipeline refused") || line.contains("root signature refused") {
        return Some(Problem::PipelineRefused);
    }
    None
}

/// The process a relaunch started (`relaunched pid=N`), whose window the
/// bench closes at its end instead.
fn relaunched(line: &str) -> Option<u32> {
    let at = line.find("relaunched")?;
    let rest = &line[line[at..].find("pid=")? + at + 4..];
    let len = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..len].parse().ok()
}

/// What a line was, for the bench's progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Seen {
    Frame,
    Problem(Problem),
    Other,
}

/// Lines kept of each problem.
const KEPT: usize = 3;

/// Everything read from a run's lines.
#[derive(Debug, Default)]
pub(crate) struct Tally {
    pub intervals: Vec<Interval>,
    /// Time of the first line with one.
    first_at: Option<f64>,
    /// The last clock line, for the next frame line.
    clock: Option<Clock>,
    /// By problem: how many lines told it, and the first few.
    problems: [(usize, Vec<String>); 4],
    /// See [`relaunched`].
    pub relaunched: Option<u32>,
}

impl Tally {
    /// Reads one line (colour codes already taken out).
    pub(crate) fn feed(&mut self, line: &str) -> Seen {
        let at = timestamp(line);
        if self.first_at.is_none() {
            self.first_at = at;
        }
        if let Some(mut interval) = frame_line(line) {
            interval.at = at;
            // The clock line comes just before, from the same second.
            interval.clock = self.clock.take().filter(|c| match (c.at, at) {
                (Some(c), Some(at)) => (at - c).abs() < 0.5,
                _ => true,
            });
            self.intervals.push(interval);
            return Seen::Frame;
        }
        if let Some(mut clock) = clock_line(line) {
            clock.at = at;
            self.clock = Some(clock);
            return Seen::Other;
        }
        if let Some((per_frame, why)) = drop_reason(line) {
            if let Some(last) = self.intervals.last_mut() {
                last.reasons.push((why, per_frame));
            }
            return Seen::Other;
        }
        if let Some(pid) = relaunched(line) {
            self.relaunched = Some(pid);
            return Seen::Other;
        }
        if let Some(p) = problem(line) {
            let (count, kept) = &mut self.problems[p as usize];
            *count += 1;
            if kept.len() < KEPT {
                kept.push(line.chars().take(300).collect());
            }
            return Seen::Problem(p);
        }
        Seen::Other
    }

    /// How many lines told `p`.
    pub(crate) fn count(&self, p: Problem) -> usize {
        self.problems[p as usize].0
    }

    /// The first lines that told `p`.
    pub(crate) fn lines(&self, p: Problem) -> &[String] {
        &self.problems[p as usize].1
    }

    /// The run's figures; `start` is the unix time the run started (else the
    /// first line's).
    pub(crate) fn summary(&self, start: Option<f64>) -> Summary {
        let start = start.or(self.first_at);
        let since = |at: Option<f64>| Some(at? - start?);
        let world: Vec<&Interval> = self.intervals.iter().filter(|i| i.world()).collect();
        let measured: Vec<&Interval> = world.iter().copied().filter(|i| i.making == 0.0).collect();
        let frames: f64 = measured.iter().map(|i| i.frames()).sum();
        let mean = |f: fn(&Interval) -> f64| {
            if frames > 0.0 {
                measured.iter().map(|i| i.frames() * f(i)).sum::<f64>() / frames
            } else {
                0.0
            }
        };
        let ms = mean(|i| i.ms);
        let worst = measured
            .iter()
            .max_by(|a, b| a.ms.total_cmp(&b.ms))
            .map(|i| Worst { ms: i.ms, fps: i.fps, at: since(i.at) });
        let clocked: Vec<(f64, Clock)> = measured.iter().filter_map(|i| Some((i.frames(), i.clock?))).collect();
        let clock_frames: f64 = clocked.iter().map(|(w, _)| w).sum();
        let clock = (clock_frames > 0.0).then(|| {
            let mean = |f: fn(&Clock) -> f64| clocked.iter().map(|(w, c)| w * f(c)).sum::<f64>() / clock_frames;
            Clock {
                at: None,
                idle_ms: mean(|c| c.idle_ms),
                busy: mean(|c| c.busy),
                behind_ms: mean(|c| c.behind_ms),
                stood_ms: mean(|c| c.stood_ms),
            }
        });
        // Per frame over the measured intervals, most first.
        let mut reasons: Vec<(String, f64)> = Vec::new();
        for i in &measured {
            for (why, per_frame) in &i.reasons {
                let share = i.frames() * per_frame / frames;
                match reasons.iter_mut().find(|(w, _)| w == why) {
                    Some((_, n)) => *n += share,
                    None => reasons.push((why.clone(), share)),
                }
            }
        }
        reasons.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Summary {
            intervals: self.intervals.len(),
            world_intervals: world.len(),
            to_world: world.first().and_then(|i| since(i.at)),
            reached_world: !world.is_empty(),
            measured: measured.len(),
            frames,
            ms,
            fps: if ms > 0.0 { 1000.0 / ms } else { 0.0 },
            worst,
            preparing: mean(|i| i.preparing),
            copies_ms: mean(|i| i.copies_ms),
            copies_mb: mean(|i| i.copies_mb),
            textures_ms: mean(|i| i.textures_ms),
            textures_mb: mean(|i| i.textures_mb),
            gpu: mean(|i| i.gpu),
            back_ms: mean(|i| i.back_ms),
            back_mb: mean(|i| i.back_mb),
            draws: mean(|i| i.draws),
            dropped: mean(|i| i.dropped),
            clock,
            reasons,
            problems: Problem::ALL.map(|p| self.count(p)),
        }
    }
}

/// The slowest interval measured: ms a frame, fps, seconds after the start.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Worst {
    pub ms: f64,
    pub fps: f64,
    pub at: Option<f64>,
}

/// A run's figures. Means are over the frames of the intervals measured:
/// those of the world (1,000 draws a frame or more) with no pipeline being
/// made.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Summary {
    /// Frame lines read, and of them those of the world.
    pub intervals: usize,
    pub world_intervals: usize,
    /// Whether an interval showed the world, and when the first one did
    /// (seconds after the start).
    pub reached_world: bool,
    pub to_world: Option<f64>,
    /// Intervals measured and the frames they counted.
    pub measured: usize,
    pub frames: f64,
    pub ms: f64,
    pub fps: f64,
    pub worst: Option<Worst>,
    pub preparing: f64,
    pub copies_ms: f64,
    pub copies_mb: f64,
    pub textures_ms: f64,
    pub textures_mb: f64,
    pub gpu: f64,
    pub back_ms: f64,
    pub back_mb: f64,
    pub draws: f64,
    pub dropped: f64,
    /// The card's clock, over the measured intervals that had its line.
    pub clock: Option<Clock>,
    /// Dropped draws a frame by reason (diagnostics builds), most first.
    pub reasons: Vec<(String, f64)>,
    /// Lines that told each of [`Problem::ALL`].
    pub problems: [usize; 4],
}

#[cfg(test)]
mod tests {
    use super::*;

    // Lines of a real run's log (an RTX 5090, a diagnostics build), the
    // long tails of the frame lines cut where nothing is read any more.
    const MENU_CLOCK: &str = "2026-11-26T19:35:22.161645Z  INFO victory_gpu::host: the card's clock per frame: nothing of ours queued 34.4 ms (the card about 7% busy by this count), 5.6 ms of that while the submitting thread was working (the card idle behind us), the rest while the title had not submitted; the submitting thread stood waiting for the card 0.5 ms";
    const MENU: &str = "2026-11-26T19:35:22.162229Z  INFO victory_gpu::host: 27.1 fps, 36.9 ms/frame (5.6 preparing: copies 2.0 ms 11.8 MB, textures 1.0 ms 19.7 MB; 0.5 on the GPU; 0.0 copying back 0.0 MB), 12 draws/frame, dropped 0 without a host pipeline (0 being made) and 0 indirect; heap buffers 0.0 passes 0.0 ms;                  texture slots written so far: 167933 placed, 18019 of objects; textures picked by index: 26708 uploaded so far (1344 MB), 14009 waiting, 96 passed over unmapped";
    const WORLD_CLOCK: &str = "2026-11-26T19:35:27.219370Z  INFO victory_gpu::host: the card's clock per frame: nothing of ours queued 113.2 ms (the card about 31% busy by this count), 70.0 ms of that while the submitting thread was working (the card idle behind us), the rest while the title had not submitted; the submitting thread stood waiting for the card 7.1 ms";
    const WORLD: &str = "2026-11-26T19:35:27.220565Z  INFO victory_gpu::host: 6.1 fps, 163.4 ms/frame (96.9 preparing: copies 29.8 ms 142.3 MB, textures 9.0 ms 251.4 MB; 7.1 on the GPU; 22.9 copying back 52.4 MB), 2985 draws/frame, dropped 1179 without a host pipeline (0 being made) and 622 indirect; heap buffers 0.9 passes 35.5 ms;                  texture slots written so far: 234007 placed, 42126 of objects";
    const WORLD_WHY: [&str; 2] = [
        "2026-11-26T19:35:27.234278Z  INFO victory_gpu::host: dropped 1178.3/frame without a host pipeline: a stage of the ray-tracing structure builder, which is off",
        "2026-11-26T19:35:27.234319Z  INFO victory_gpu::host: dropped 623.0/frame without a host pipeline: being made",
    ];
    const NEXT_CLOCK: &str = "2026-11-26T19:35:32.536794Z  INFO victory_gpu::host: the card's clock per frame: nothing of ours queued 33.3 ms (the card about 65% busy by this count), 23.9 ms of that while the submitting thread was working (the card idle behind us), the rest while the title had not submitted; the submitting thread stood waiting for the card 12.9 ms";
    const NEXT: &str = "2026-11-26T19:35:32.537556Z  INFO victory_gpu::host: 10.5 fps, 94.8 ms/frame (58.3 preparing: copies 25.1 ms 107.4 MB, textures 18.7 ms 175.5 MB; 12.9 on the GPU; 25.5 copying back 36.0 MB), 1715 draws/frame, dropped 503 without a host pipeline (0 being made) and 614 indirect; heap buffers 1.0 passes 25.7 ms;";
    const NEXT_WHY: [&str; 2] = [
        "2026-11-26T19:35:32.552151Z  INFO victory_gpu::host: dropped 615.2/frame without a host pipeline: being made",
        "2026-11-26T19:35:32.552191Z  INFO victory_gpu::host: dropped 502.0/frame without a host pipeline: a stage of the ray-tracing structure builder, which is off",
    ];
    /// The log's first line, at the run's start.
    const FIRST: &str = "2026-11-26T19:34:25.837067Z  INFO victory_hle::modules::xmem::low_guards: space below 4 GiB kept for the title guards=2 mb=3839";

    fn run() -> Vec<&'static str> {
        let mut lines = vec![FIRST, MENU_CLOCK, MENU, WORLD_CLOCK, WORLD];
        lines.extend(WORLD_WHY);
        lines.extend([NEXT_CLOCK, NEXT]);
        lines.extend(NEXT_WHY);
        lines
    }

    #[test]
    fn a_frame_line_is_read_whole() {
        let i = frame_line(WORLD).unwrap();
        let read = [
            i.fps,
            i.ms,
            i.preparing,
            i.copies_ms,
            i.copies_mb,
            i.textures_ms,
            i.textures_mb,
            i.gpu,
            i.back_ms,
            i.back_mb,
            i.draws,
            i.dropped,
            i.making,
        ];
        assert_eq!(read, [6.1, 163.4, 96.9, 29.8, 142.3, 9.0, 251.4, 7.1, 22.9, 52.4, 2985.0, 1179.0, 0.0]);
        assert!(i.world());
        assert!(!frame_line(MENU).unwrap().world());
        // Other lines of the same target are not frame lines.
        for line in [WORLD_CLOCK, WORLD_WHY[0], FIRST] {
            assert_eq!(frame_line(line), None, "{line}");
        }
    }

    #[test]
    fn colour_codes_are_taken_out() {
        let coloured = "\x1b[2m2026-11-26T19:35:27.220565Z\x1b[0m \x1b[32m INFO\x1b[0m \x1b[2mvictory_gpu::host\x1b[0m\x1b[2m:\x1b[0m 6.1 fps, 163.4 ms/frame (96.9 preparing: copies 29.8 ms 142.3 MB, textures 9.0 ms 251.4 MB; 7.1 on the GPU; 22.9 copying back 52.4 MB), 2985 draws/frame, dropped 1179 without a host pipeline (0 being made) and 622 indirect";
        let line = plain(coloured);
        assert!(line.starts_with("2026-11-26T19:35:27.220565Z  INFO victory_gpu::host: 6.1 fps"), "{line}");
        assert_eq!(frame_line(&line).unwrap().draws, 2985.0);
        assert!(matches!(plain(WORLD), Cow::Borrowed(_)));
    }

    #[test]
    fn times_are_unix_seconds() {
        // 2026-11-26 is day 20,783 since 1970-01-01.
        let at = timestamp(WORLD).unwrap();
        assert!((at - (20_783.0 * 86_400.0 + 19.0 * 3600.0 + 35.0 * 60.0 + 27.220565)).abs() < 1e-6);
        assert_eq!(timestamp("1970-01-01T00:00:00Z"), Some(0.0));
        assert_eq!(timestamp("2000-03-01T00:00:00Z"), Some(951_868_800.0));
        assert_eq!(timestamp("title exited with 0"), None);
    }

    #[test]
    fn the_clock_and_the_reasons_are_read() {
        let clock = clock_line(WORLD_CLOCK).unwrap();
        assert_eq!((clock.idle_ms, clock.busy, clock.behind_ms, clock.stood_ms), (113.2, 31.0, 70.0, 7.1));
        assert_eq!(
            drop_reason(WORLD_WHY[1]),
            Some((623.0, "being made".to_owned()))
        );
        assert_eq!(drop_reason(WORLD), None);
    }

    #[test]
    fn the_summary_is_over_the_world() {
        let mut tally = Tally::default();
        for line in run() {
            tally.feed(line);
        }
        assert_eq!(tally.intervals.len(), 3);
        assert_eq!(tally.intervals[1].clock.map(|c| c.busy), Some(31.0));
        assert_eq!(tally.intervals[1].reasons.len(), 2);
        let s = tally.summary(None);
        assert_eq!((s.intervals, s.world_intervals, s.measured), (3, 2, 2));
        assert!(s.reached_world);
        // From the first line, 19:34:25.837067, to 19:35:27.220565.
        assert!((s.to_world.unwrap() - 61.383498).abs() < 1e-3, "{:?}", s.to_world);
        // 6.1 and 10.5 frames.
        assert!((s.frames - 16.6).abs() < 1e-9);
        let ms = (6.1 * 163.4 + 10.5 * 94.8) / 16.6;
        assert!((s.ms - ms).abs() < 1e-9);
        assert!((s.fps - 1000.0 / ms).abs() < 1e-9);
        let worst = s.worst.unwrap();
        assert_eq!((worst.ms, worst.fps), (163.4, 6.1));
        assert!((worst.at.unwrap() - 61.383498).abs() < 1e-3);
        assert!((s.draws - (6.1 * 2985.0 + 10.5 * 1715.0) / 16.6).abs() < 1e-9);
        assert!((s.gpu - (6.1 * 7.1 + 10.5 * 12.9) / 16.6).abs() < 1e-9);
        let busy = (6.1 * 31.0 + 10.5 * 65.0) / 16.6;
        assert!((s.clock.unwrap().busy - busy).abs() < 1e-9);
        assert_eq!(s.reasons.len(), 2);
        assert_eq!(s.reasons[0].0, "a stage of the ray-tracing structure builder, which is off");
        assert!((s.reasons[1].1 - (6.1 * 623.0 + 10.5 * 615.2) / 16.6).abs() < 1e-9);
        assert_eq!(s.problems, [0; 4]);
        // A start given: the time to the world is from it.
        let start = timestamp(FIRST).unwrap() - 1.0;
        assert!((tally.summary(Some(start)).to_world.unwrap() - 62.383498).abs() < 1e-3);
    }

    #[test]
    fn intervals_while_pipelines_are_made_are_left_out() {
        let making = WORLD.replace("(0 being made)", "(3 being made)");
        let mut tally = Tally::default();
        for line in [FIRST, MENU, &making, NEXT] {
            tally.feed(line);
        }
        let s = tally.summary(None);
        assert_eq!((s.world_intervals, s.measured), (2, 1));
        assert_eq!(s.ms, 94.8);
        // The world was reached at the first world interval all the same.
        assert!((s.to_world.unwrap() - 61.383498).abs() < 1e-3);
    }

    #[test]
    fn a_run_that_never_reaches_the_world_has_no_figures() {
        let mut tally = Tally::default();
        for line in [FIRST, MENU_CLOCK, MENU] {
            tally.feed(line);
        }
        let s = tally.summary(None);
        assert!(!s.reached_world);
        assert_eq!((s.to_world, s.measured, s.ms, s.worst), (None, 0, 0.0, None));
    }

    #[test]
    fn problems_are_told_apart() {
        let lines = [
            ("thread 'victory-submit' panicked at crates/victory-gpu/src/host.rs:1:1:", Some(Problem::Panic)),
            ("VIctory: panic on thread \"main\": oops", Some(Problem::Panic)),
            ("VIctory: unhandled exception 0xc0000005 at title+0x1234", Some(Problem::Crash)),
            ("VIctory: host GPU failure: presenting; device removed: The GPU device instance has been suspended. (0x887A0005)", Some(Problem::DeviceRemoved)),
            ("2026-11-26T19:35:27Z ERROR victory_gpu::host: when the host device went: ...", Some(Problem::DeviceRemoved)),
            ("2026-11-26T19:35:27Z  WARN victory_gpu::pipeline::host: host pipeline refused error=E_INVALIDARG", Some(Problem::PipelineRefused)),
            ("2026-11-26T19:35:27Z  WARN victory_gpu::pipeline::host: host root signature refused", Some(Problem::PipelineRefused)),
            ("VIctory: first-chance exception 0xc0000005 at title+0x1234", None),
            (WORLD, None),
        ];
        let mut tally = Tally::default();
        for (line, expected) in lines {
            assert_eq!(problem(line), expected, "{line}");
            let seen = tally.feed(line);
            assert_eq!(seen == Seen::Other, expected.is_none() && line != WORLD, "{line}");
        }
        assert_eq!(tally.count(Problem::Panic), 2);
        assert_eq!(tally.count(Problem::DeviceRemoved), 2);
        assert_eq!(tally.summary(None).problems, [2, 1, 2, 2]);
        assert_eq!(tally.lines(Problem::Crash).len(), 1);
    }

    #[test]
    fn a_relaunch_names_the_new_process() {
        let mut tally = Tally::default();
        tally.feed("2026-11-26T19:35:27.1Z  INFO victory_hle::modules::lifecycle: relaunched pid=4242 args=[\"run\"]");
        assert_eq!(tally.relaunched, Some(4242));
    }
}
