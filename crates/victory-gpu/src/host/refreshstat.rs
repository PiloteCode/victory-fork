//! Diagnostics: what a submission's refreshes of host copies cost, by what
//! they do (the frame-rate line's "refreshes ... in N flushes of M" is the
//! time of the flushes as a whole).
//!
//! A frame has some 10,000 refreshes (one per host copy and flush, the ranges
//! of a copy joined), nine in ten known to copy nothing; together 10 to 16 ms
//! of the preparing thread. Whether that is the checks of the nine or the
//! copying of the tenth, the line says.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Since the last frame line (the `AT_` places), all threads.
static STATS: [AtomicU64; 7] = [const { AtomicU64::new(0) }; 7];
/// Refreshes found to copy nothing, and the nanoseconds of finding it.
const AT_NOTHING: usize = 0;
const AT_NOTHING_NS: usize = 1;
/// Refreshes done, their nanoseconds and the bytes they copied.
const AT_DONE: usize = 2;
const AT_DONE_NS: usize = 3;
const AT_DONE_BYTES: usize = 4;
/// Nanoseconds of gathering the spans of a submission's refreshes (the
/// submitting thread), and of the flushes as a whole (its wall time).
const AT_PLANNING_NS: usize = 5;
const AT_FLUSH_NS: usize = 6;

fn add(at: usize, n: u64) {
    STATS[at].fetch_add(n, Relaxed);
}

/// A check found a refresh to copy nothing (`nothing`) or not, in the time
/// since `since`.
pub(super) fn checked(nothing: bool, since: std::time::Instant) {
    if nothing {
        add(AT_NOTHING, 1);
    }
    add(AT_NOTHING_NS, since.elapsed().as_nanos() as u64);
}

/// A refresh copied `bytes` in the time since `since`.
pub(super) fn done(bytes: usize, since: std::time::Instant) {
    add(AT_DONE, 1);
    add(AT_DONE_NS, since.elapsed().as_nanos() as u64);
    add(AT_DONE_BYTES, bytes as u64);
}

/// The spans of a submission's refreshes were gathered, from `since`.
pub(super) fn planned(since: std::time::Instant) {
    add(AT_PLANNING_NS, since.elapsed().as_nanos() as u64);
}

/// A flush of refreshes took the time since `since`.
pub(super) fn flushed(since: std::time::Instant) {
    add(AT_FLUSH_NS, since.elapsed().as_nanos() as u64);
}

/// The frame line's.
pub(super) fn line(frames: f64) {
    let s: Vec<f64> = STATS.iter().map(|a| a.swap(0, Relaxed) as f64 / frames).collect();
    if s[AT_DONE] + s[AT_NOTHING] == 0.0 {
        return;
    }
    let each = |ns: f64, n: f64| if n > 0.0 { ns / n / 1e3 } else { 0.0 };
    tracing::info!(
        "refreshes of host copies per frame: gathering their spans {:.1} ms, the flushes {:.1} ms; checks {:.1} ms of the \
         threads' time ({:.0} found nothing to copy, {:.1} us a check); {:.0} done in {:.1} ms of the threads' time \
         ({:.1} us each, {:.1} MB, {:.2} GB/s a thread)",
        s[AT_PLANNING_NS] / 1e6,
        s[AT_FLUSH_NS] / 1e6,
        s[AT_NOTHING_NS] / 1e6,
        s[AT_NOTHING],
        each(s[AT_NOTHING_NS], s[AT_NOTHING] + s[AT_DONE]),
        s[AT_DONE],
        s[AT_DONE_NS] / 1e6,
        each(s[AT_DONE_NS], s[AT_DONE]),
        s[AT_DONE_BYTES] / 1e6,
        if s[AT_DONE_NS] > 0.0 { s[AT_DONE_BYTES] / s[AT_DONE_NS] } else { 0.0 },
    );
}
