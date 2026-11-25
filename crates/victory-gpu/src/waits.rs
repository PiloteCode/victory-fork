//! The submitting thread's waits for flights (`crate::host::late`, stage 1
//! of the coherence plan): what each wait finds ahead of it on the card,
//! what it waits for and how long it lasts (diagnostics builds), and the
//! switches that shorten the waits (both off by default):
//!
//! - `VICTORY_WAIT_PRODUCER=1`: a settle for title memory changed under
//!   flights' shader writes waits only until the flights that may write the
//!   memory it could not merge have landed (their producers), not for
//!   everything in the air (`crate::host::wait_producers`);
//! - `VICTORY_SUBMIT_EVERY=N`: a title submission of more than `N` draws
//!   goes to the card as several flights of about `N` draws each, cut
//!   between its host lists, while waits happened in the last frames.
//!
//! Why: the wave 7 gameplay waits 105-118 ms a frame in 2-3 settles, 76-108
//! ms of it for "title memory changed under shader writes", each settle a
//! wait for every flight in the air and for the relay to land them all.
//! shadPS4 measures some 68 µs a wait on the same card. Each wait's line
//! here says how much of it was the card's work still queued, how much the
//! relay's landing, and how many flights (and draws) were queued behind the
//! last one that writes what the wait is for: what a wait for that producer
//! alone would have skipped.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// `VICTORY_WAIT_PRODUCER=1`: settles for title memory changed under
/// flights' shader writes wait for that memory's producers only (with the
/// relay, `crate::host::relay`).
pub(crate) fn producer() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var_os("VICTORY_WAIT_PRODUCER").is_some_and(|v| v == "1");
        if on {
            tracing::info!(
                "settles for title memory changed under shader writes wait for the flights that may write it, not for all (VICTORY_WAIT_PRODUCER=1)"
            );
        }
        on
    })
}

/// `VICTORY_SUBMIT_EVERY=N` (draws, at least 16): title submissions are
/// sent as flights of about `N` draws while waits happen.
pub(crate) fn every() -> Option<u32> {
    static EVERY: OnceLock<Option<u32>> = OnceLock::new();
    *EVERY.get_or_init(|| {
        let n = std::env::var("VICTORY_SUBMIT_EVERY").ok().and_then(|v| v.trim().parse::<u32>().ok()).filter(|&n| n > 0);
        let n = n.map(|n| n.max(16));
        if let Some(n) = n {
            tracing::info!(
                draws = n,
                "title submissions go to the card as flights of about this many draws while the frames wait for flights (VICTORY_SUBMIT_EVERY)"
            );
        }
        n
    })
}

/// The frame of the last wait for flights, plus one (0: none yet).
static LAST_WAIT_FRAME: AtomicU64 = AtomicU64::new(0);

/// The submitting thread waits for flights: noted for [`cut`], which cuts
/// only while waits happen.
pub(crate) fn waited() {
    LAST_WAIT_FRAME.store(crate::frame::current() + 1, Relaxed);
}

/// Whether a wait for flights happened in this frame or the two before.
fn waiting_lately() -> bool {
    let last = LAST_WAIT_FRAME.load(Relaxed);
    last > 0 && crate::frame::current() < last + 2
}

/// Where a submission whose host lists have `draws` draws each, in their
/// order, is cut into flights of about `every` draws: the positions where
/// each flight after the first starts (empty: one flight). A flight ends
/// after the list that brings it to `every` draws or more; a last flight
/// of fewer than `every / 4` draws joins the one before.
pub(crate) fn cuts(draws: &[u32], every: u32) -> Vec<usize> {
    let every = u64::from(every.max(1));
    let total: u64 = draws.iter().map(|&d| u64::from(d)).sum();
    if total <= every {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut acc = 0u64;
    let mut left = total;
    for (i, &d) in draws.iter().enumerate() {
        acc += u64::from(d);
        left -= u64::from(d);
        if acc >= every && i + 1 < draws.len() && left > 0 {
            if left < every / 4 {
                break;
            }
            out.push(i + 1);
            acc = 0;
        }
    }
    out
}

/// With [`every`] on and waits lately: where to cut a submission whose
/// lists have `draws` draws each (as [`cuts`]); empty: not cut.
pub(crate) fn cut(draws: &[u32]) -> Vec<usize> {
    match every() {
        Some(n) if waiting_lately() => {
            let at = cuts(draws, n);
            if !at.is_empty() {
                CUT[0].fetch_add(1, Relaxed);
                CUT[1].fetch_add(at.len() as u64, Relaxed);
            }
            at
        }
        _ => Vec::new(),
    }
}

/// Since the last frame-rate line ([`every`]): submissions cut, flights
/// they gave beyond one each.
static CUT: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];

/// Since the last frame-rate line ([`producer`]): settles that waited for
/// producers only, their rounds, and those that still waited for all.
pub(crate) static PRODUCER_STATS: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];

/// What a wait for flights finds when it begins.
#[derive(Default, Clone, Copy, Debug)]
pub(crate) struct Air {
    /// Flights not landed yet (those being landed too).
    pub(crate) flights: u32,
    /// Of them, those the card has run, waiting for the relay to land them.
    pub(crate) done: u32,
    /// Host lists and draws of the flights the card has not run yet, and
    /// the bytes of title memory their preparing copied (what they bring
    /// over the bus: a count, where the wait's time depends on the bus).
    pub(crate) lists: u32,
    pub(crate) draws: u64,
    pub(crate) up: u64,
    /// Behind the last flight that may write what the wait is for: the
    /// flights, their draws and bytes (all of them if no flight in the
    /// line may; `None`: the wait is for everything in the air).
    pub(crate) behind: Option<(u32, u64, u64)>,
}

/// A wait under way ([`begin`], [`end`]).
pub(crate) struct Begun {
    #[cfg(feature = "diagnostics")]
    why: usize,
    #[cfg(feature = "diagnostics")]
    at: std::time::Instant,
    #[cfg(feature = "diagnostics")]
    air: Air,
    #[cfg(feature = "diagnostics")]
    unsent: i64,
    #[cfg(feature = "diagnostics")]
    relay: [u64; 3],
    #[cfg(feature = "diagnostics")]
    what: Option<(usize, usize)>,
}

/// A wait for flights for reason `why` (`crate::host`'s `WHY_` places)
/// begins: `air` says what is in the air (looked at in diagnostics builds
/// only), `what` the title memory waited for, if it is a range.
pub(crate) fn begin(why: usize, air: impl FnOnce() -> Air, what: Option<(usize, usize)>) -> Begun {
    waited();
    #[cfg(feature = "diagnostics")]
    {
        Begun {
            why,
            at: std::time::Instant::now(),
            air: air(),
            unsent: UNSENT.load(Relaxed),
            relay: relay_now(),
            what,
        }
    }
    #[cfg(not(feature = "diagnostics"))]
    {
        let _ = (why, air, what);
        Begun {}
    }
}

/// The wait `begun` is over. `card_us`: of it, the microseconds spent
/// waiting for the card, when the waiting thread itself waited for it
/// (without the relay); with the relay its thread's time is taken instead.
pub(crate) fn end(begun: Begun, card_us: Option<u64>) {
    #[cfg(feature = "diagnostics")]
    {
        let us = begun.at.elapsed().as_micros() as u64;
        let (card, landing) = match card_us {
            Some(card) => (card.min(us), us.saturating_sub(card)),
            None => {
                let now = relay_now();
                let (c, l) = (CARD as usize, LANDING as usize);
                (now[c].saturating_sub(begun.relay[c]), now[l].saturating_sub(begun.relay[l]))
            }
        };
        let why = begun.why.min(WHYS - 1);
        let a = &begun.air;
        let s = &STATS[why];
        s[0].fetch_add(1, Relaxed);
        s[1].fetch_add(us, Relaxed);
        s[2].fetch_add(card, Relaxed);
        s[3].fetch_add(landing, Relaxed);
        s[4].fetch_add(u64::from(a.flights), Relaxed);
        s[5].fetch_add(u64::from(a.done), Relaxed);
        s[6].fetch_add(u64::from(a.lists), Relaxed);
        s[7].fetch_add(a.draws, Relaxed);
        s[8].fetch_add(begun.unsent.max(0) as u64, Relaxed);
        if let Some((flights, draws, up)) = a.behind {
            s[9].fetch_add(1, Relaxed);
            s[10].fetch_add(u64::from(flights), Relaxed);
            s[11].fetch_add(draws, Relaxed);
            s[13].fetch_add(up, Relaxed);
        }
        s[12].fetch_add(a.up, Relaxed);
        static SEEN: AtomicU64 = AtomicU64::new(0);
        let n = SEEN.fetch_add(1, Relaxed);
        if n < 48 || n.is_multiple_of(128) {
            let what = begun.what.map_or_else(|| "everything in the air".to_owned(), |(from, to)| format!("{from:#x}+{:#x}", to - from));
            let behind = a.behind.map_or_else(
                || "a wait for all".to_owned(),
                |(f, d, up)| format!("{f} flights, {d} draws and {:.1} MB copied up were behind its last producer", mb(up)),
            );
            tracing::info!(
                n,
                "a wait for flights: {}, {:.1} ms (the relay waiting for the card {:.1}, landing {:.1}); at its start \
                 {} flights in the air ({} run by the card, not landed), {} lists, {} draws and {:.1} MB copied up not run, \
                 {} draws recorded and not sent; it waits for {what}: {behind}",
                why_name(why),
                us as f64 / 1e3,
                card as f64 / 1e3,
                landing as f64 / 1e3,
                a.flights,
                a.done,
                a.lists,
                a.draws,
                mb(a.up),
                begun.unsent.max(0),
            );
        }
    }
    #[cfg(not(feature = "diagnostics"))]
    let _ = (begun, card_us);
}

/// The `WHY_` places of `crate::host`, as the settle lines name them.
#[cfg(feature = "diagnostics")]
const WHYS: usize = 11;
#[cfg(feature = "diagnostics")]
fn why_name(why: usize) -> &'static str {
    match why {
        2 => "a synchronous submission, present or remap",
        3 => "the card had run them",
        4 => "a wait for a memory value",
        5 => "title memory changed under shader writes",
        6 => "title memory read",
        7 => "a step of ours",
        8 => "lists in the air",
        9 => "bottom-of-pipe writes",
        10 => "a blocking fence wait",
        _ => "other",
    }
}

/// Per `WHY_` place, since the last frame-rate line: waits, µs, µs the
/// relay waited for the card, µs it landed; at their starts, the sums of
/// flights, flights run, lists and draws not run, draws not sent; waits
/// whose producer is known, flights and draws behind it; bytes copied up
/// not run, and of them behind the producer.
#[cfg(feature = "diagnostics")]
static STATS: [[AtomicU64; 14]; WHYS] = [const { [const { AtomicU64::new(0) }; 14] }; WHYS];

/// Bytes as MB.
#[cfg(feature = "diagnostics")]
fn mb(bytes: u64) -> f64 {
    bytes as f64 / f64::from(1 << 20)
}

/// Draws recorded into host lists and not sent to the card yet.
#[cfg(feature = "diagnostics")]
static UNSENT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// `n` draws were recorded (`HostList::drew`).
#[cfg(feature = "diagnostics")]
pub(crate) fn recorded(n: u32) {
    UNSENT.fetch_add(i64::from(n), Relaxed);
}

/// `n` draws recorded went to the card, or their list was reset unsent.
#[cfg(feature = "diagnostics")]
pub(crate) fn sent(n: u32) {
    if n > 0 {
        UNSENT.fetch_sub(i64::from(n), Relaxed);
    }
}

/// What the thread that lands flights does (`crate::host::relay`): waits
/// for flights, waits for the card, lands.
pub(crate) const IDLE: u64 = 0;
pub(crate) const CARD: u64 = 1;
pub(crate) const LANDING: u64 = 2;

/// The relay's phase and since when (µs): `phase << 60 | since`.
#[cfg(feature = "diagnostics")]
static PHASE: AtomicU64 = AtomicU64::new(0);
/// The relay's microseconds per phase, phases ended.
#[cfg(feature = "diagnostics")]
static PHASE_US: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
#[cfg(feature = "diagnostics")]
const SINCE: u64 = (1 << 60) - 1;

/// The thread that lands flights enters `phase` ([`IDLE`], [`CARD`],
/// [`LANDING`]).
pub(crate) fn relay_phase(phase: u64) {
    #[cfg(feature = "diagnostics")]
    {
        let now = crate::host::now_us().max(1) & SINCE;
        let old = PHASE.swap((phase << 60) | now, Relaxed);
        let since = old & SINCE;
        if since != 0 {
            PHASE_US[((old >> 60) as usize).min(2)].fetch_add(now.saturating_sub(since), Relaxed);
        }
    }
    #[cfg(not(feature = "diagnostics"))]
    let _ = phase;
}

/// The relay's microseconds per phase so far, the current one included.
#[cfg(feature = "diagnostics")]
fn relay_now() -> [u64; 3] {
    let current = PHASE.load(Relaxed);
    let mut out = [0, 1, 2].map(|i| PHASE_US[i].load(Relaxed));
    let since = current & SINCE;
    if since != 0 {
        out[((current >> 60) as usize).min(2)] += (crate::host::now_us() & SINCE).saturating_sub(since);
    }
    out
}

/// For the frame-rate line, per frame since the last one.
pub(crate) fn line(frames: f64) {
    let [cut, flights] = [0, 1].map(|i| CUT[i].swap(0, Relaxed) as f64 / frames);
    if every().is_some() {
        tracing::info!(
            "submissions cut into flights per frame (VICTORY_SUBMIT_EVERY={}): {cut:.1} cut, {flights:.1} flights more",
            every().unwrap_or(0)
        );
    }
    let [narrow, rounds, all] = [0, 1, 2].map(|i| PRODUCER_STATS[i].swap(0, Relaxed) as f64 / frames);
    if producer() {
        tracing::info!(
            "settles for title memory changed under shader writes per frame (VICTORY_WAIT_PRODUCER=1): {narrow:.1} waited \
             for producers only ({rounds:.1} rounds), {all:.1} still waited for everything in the air"
        );
    }
    #[cfg(feature = "diagnostics")]
    {
        let s: Vec<[f64; 14]> = STATS.iter().map(|w| w.each_ref().map(|v| v.swap(0, Relaxed) as f64)).collect();
        let sum = |i: usize| s.iter().map(|w| w[i]).sum::<f64>();
        let n = sum(0);
        let per = |i: usize| if n > 0.0 { sum(i) / n } else { 0.0 };
        let known = sum(9);
        let per_known = |i: usize| if known > 0.0 { sum(i) / known } else { 0.0 };
        let mut by = Vec::new();
        for (why, w) in s.iter().enumerate() {
            if w[0] > 0.0 {
                let behind = if w[9] > 0.0 { format!("{:.1}", w[10] / w[9]) } else { "-".to_owned() };
                by.push(format!(
                    "{} {:.2}x {:.1} ms (card {:.1}, landing {:.1}; {:.1} flights, {:.0} draws, {:.1} MB not run at the start; \
                     {behind} flights behind the producer)",
                    why_name(why),
                    w[0] / frames,
                    w[1] / 1e3 / frames,
                    w[2] / 1e3 / frames,
                    w[3] / 1e3 / frames,
                    w[4] / w[0],
                    w[7] / w[0],
                    w[12] / w[0] / f64::from(1 << 20),
                ));
            }
        }
        tracing::info!(
            "waits for flights per frame (coh-submit): {:.2} waits, {:.1} ms (the relay waiting for the card {:.1} ms, \
             landing {:.1} ms); at their start on average {:.1} flights in the air ({:.1} run by the card, not landed), \
             {:.0} lists, {:.0} draws and {:.1} MB copied up not run, {:.0} draws recorded and not sent; behind the last \
             producer of what they wait for: {:.1} flights, {:.0} draws, {:.1} MB ({:.2} waits a frame with a known \
             producer); by why: {}",
            n / frames,
            sum(1) / 1e3 / frames,
            sum(2) / 1e3 / frames,
            sum(3) / 1e3 / frames,
            per(4),
            per(5),
            per(6),
            per(7),
            per(12) / f64::from(1 << 20),
            per(8),
            per_known(10),
            per_known(11),
            per_known(13) / f64::from(1 << 20),
            known / frames,
            if by.is_empty() { "none".to_owned() } else { by.join("; ") }
        );
    }
}

#[cfg(test)]
mod tests {
    use super::cuts;

    #[test]
    fn a_submission_is_cut_between_lists_at_about_every_draws() {
        // Small enough: one flight.
        assert!(cuts(&[10, 20, 30], 64).is_empty());
        assert!(cuts(&[], 64).is_empty());
        // Each flight ends after the list that reaches the count.
        assert_eq!(cuts(&[40, 40, 40, 40, 40], 64), vec![2, 4]);
        // A big list is a flight of its own; nothing is cut inside a list.
        assert_eq!(cuts(&[500, 10, 10, 500, 70], 64), vec![1, 4]);
        // A short tail joins the flight before it.
        assert_eq!(cuts(&[64, 64, 8], 64), vec![1]);
        assert_eq!(cuts(&[64, 64, 16], 64), vec![1, 2]);
        // Lists without draws go with their neighbours.
        assert_eq!(cuts(&[0, 0, 70, 0, 0, 70], 64), vec![3]);
    }
}
