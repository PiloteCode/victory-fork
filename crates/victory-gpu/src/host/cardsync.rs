//! What goes up to copies in the card's memory before a submission
//! ([`Host::card_before`]), counted, and joined over small gaps.
//!
//! With every title buffer of 64 KiB or more in the card's memory
//! (`VICTORY_CARD_BUFFERS_MIN_KB=64`: 3,714 copies, 1,279 MB) 36,000 to
//! 49,000 ranges a frame went up instead of 4,600, for about the same bytes
//! (run integ-w121-kb64, RTX 5090): the frame line counted them, not where
//! they came from. They are noted by the refreshes (a range copied whole, the
//! words a refresh found changed, pages a landing had written), by the
//! title's bytes put in under flights' shader writes, and by what is merged
//! back; a copy's ranges are joined where they overlap or touch, and each
//! goes up as a copy command, or as pairs of offset and word if it is short
//! (`send.rs`).
//!
//! `VICTORY_CARD_JOIN_GAP=<bytes>` (0, off): ranges of one copy with at most
//! that many bytes between them go up as one, the bytes between included.
//! Those bytes are what the card holds already, where a copy's card side and
//! its CPU side hold the same: outside every range a flight in the air (or
//! being landed) may write, and in a copy whose shaders store nothing
//! undeclared (`Shadow::stores_kept`). A landing puts what shaders wrote into
//! the CPU side as well (`Shadow::card_merge_relay`), and a refresh's bytes are
//! noted to go up. Where that is not known a gap is left as it is. What it
//! costs: the gaps' bytes over the bus (counted); what it saves: runs, pairs
//! and copy commands.

use super::*;
use std::sync::Arc;

/// The most bytes between two ranges of a copy that go up as one
/// (`VICTORY_CARD_JOIN_GAP`; 0: none).
pub(super) fn join_gap() -> usize {
    static GAP: OnceLock<usize> = OnceLock::new();
    *GAP.get_or_init(|| {
        std::env::var("VICTORY_CARD_JOIN_GAP")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0)
    })
}

/// Since the last frame line (the `AT_` places).
pub(super) static STATS: [std::sync::atomic::AtomicU64; 16] =
    [const { std::sync::atomic::AtomicU64::new(0) }; 16];
/// Copies touched (with ranges to go up), and of them those under 1 MiB.
const AT_TOUCHED: usize = 0;
const AT_SMALL: usize = 1;
/// The bytes the copies under 1 MiB touched would be if each went up whole.
const AT_SMALL_WHOLE: usize = 2;
/// Ranges as the copies' own joining left them, and after the join over gaps.
const AT_RANGES: usize = 3;
const AT_JOINED: usize = 4;
/// Bytes of gaps that went up with the ranges beside them.
const AT_GAP_BYTES: usize = 5;
/// Gaps not joined because a flight may write there, or the copy's shaders
/// store undeclared.
const AT_GAPS_KEPT: usize = 6;
/// Bytes that go up; runs short enough to go as pairs, and their bytes.
const AT_BYTES: usize = 7;
const AT_SHORT: usize = 8;
const AT_SHORT_BYTES: usize = 9;
/// Passes hashing pieces again of copies no range of the submission
/// declares written.
pub(super) const AT_REHASH_UNDECLARED: usize = 10;
/// Microseconds: what changed, the bytes taken, the list that sends them,
/// the compares' plan and list.
pub(super) const AT_GATHER_US: usize = 11;
pub(super) const AT_TAKE_US: usize = 12;
pub(super) const AT_LIST_US: usize = 13;
pub(super) const AT_COMPARE_US: usize = 14;
/// Submissions with something for copies in the card's memory.
pub(super) const AT_SUBMISSIONS: usize = 15;

fn add(at: usize, n: u64) {
    STATS[at].fetch_add(n, Ordering::Relaxed);
}

/// Microseconds since `since` into `at`.
pub(super) fn spent(at: usize, since: std::time::Instant) {
    add(at, since.elapsed().as_micros() as u64);
}

/// Runs this short of whole words go up as pairs (`send::goes_as_words`).
fn as_words(at: usize, len: usize) -> bool {
    send::goes_as_words(at, len)
}

/// What [`crate::shadow::card_dirty`] gave, before the join: counted.
pub(super) fn gathered(dirty: &[(Arc<crate::shadow::Shadow>, Vec<(usize, usize)>)]) {
    let mut ranges = 0;
    for (shadow, r) in dirty {
        add(AT_TOUCHED, 1);
        let size = shadow.card_sides().map_or(0, |(_, _, size)| size);
        if size < 1 << 20 {
            add(AT_SMALL, 1);
            add(AT_SMALL_WHOLE, size);
        }
        ranges += r.len() as u64;
    }
    add(AT_RANGES, ranges);
}

/// Joins each copy's ranges over gaps of at most [`join_gap`] bytes where
/// the gap is the card's already: not in `air` (title memory flights in the
/// air may write, joined and in order), and the copy's shaders store nothing
/// undeclared. Then counts what goes up.
pub(super) fn join(
    dirty: &mut [(Arc<crate::shadow::Shadow>, Vec<(usize, usize)>)],
    air: &[(usize, usize)],
) {
    let gap = join_gap();
    for (shadow, ranges) in dirty.iter_mut() {
        if gap > 0 && ranges.len() > 1 {
            let start = shadow.span().0 as usize;
            let kept = shadow.stores_kept();
            let (joined, added, left) = join_ranges_over(ranges, gap, |from, to| {
                !kept && !touches(air, start + from, start + to)
            });
            *ranges = joined;
            add(AT_GAP_BYTES, added as u64);
            add(AT_GAPS_KEPT, left as u64);
        }
        add(AT_JOINED, ranges.len() as u64);
        for &(a, b) in ranges.iter() {
            add(AT_BYTES, (b - a) as u64);
            if as_words(a, b - a) {
                add(AT_SHORT, 1);
                add(AT_SHORT_BYTES, (b - a) as u64);
            }
        }
    }
}

/// `ranges` (in order, apart) with each gap of at most `gap` bytes that
/// `may(from, to)` allows taken in: the result, the bytes taken in, and the
/// gaps short enough that were not allowed.
fn join_ranges_over(
    ranges: &[(usize, usize)],
    gap: usize,
    may: impl Fn(usize, usize) -> bool,
) -> (Vec<(usize, usize)>, usize, usize) {
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    let (mut added, mut left) = (0, 0);
    for &(a, b) in ranges {
        match out.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            Some(last) if a - last.1 <= gap => {
                if may(last.1, a) {
                    added += a - last.1;
                    last.1 = b;
                } else {
                    left += 1;
                    out.push((a, b));
                }
            }
            _ => out.push((a, b)),
        }
    }
    (out, added, left)
}

/// The frame line's.
/// The counts since the last call, per frame of `frames` (every frame-rate
/// line, as `crate::shadow::card_stats`): [`STATS`]' and then
/// `CARD_NOTED`'s.
pub(super) fn taken(frames: f64) -> (Vec<f64>, Vec<f64>) {
    let s = STATS
        .iter()
        .map(|a| a.swap(0, Ordering::Relaxed) as f64 / frames)
        .collect();
    let n = crate::shadow::CARD_NOTED
        .iter()
        .map(|a| a.swap(0, Ordering::Relaxed) as f64 / frames)
        .collect();
    (s, n)
}

/// The line of what [`taken`] gave (with the long lines, every few seconds).
pub(super) fn log((s, n): &(Vec<f64>, Vec<f64>)) {
    if !crate::shadow::cards() {
        return;
    }
    tracing::info!(
        "copies in the card's memory, what goes up per frame (VICTORY_CARD_JOIN_GAP={}): runs noted {:.0} (refreshed ranges \
         copied whole {:.0}, words refreshes found changed {:.0}, pages landings had written {:.0}, the title's bytes under \
         flights' writes {:.0}, merged back {:.0}, sent again {:.0}); in {:.1} submissions {:.0} copies touched ({:.0} under 1 MiB, \
         {:.1} MB if each of those went up whole); {:.0} ranges as the copies' own joining left them, {:.0} after the join over \
         gaps ({:.1} KB of gaps went up, {:.0} short gaps kept as a flight may write there); {:.1} MB up, {:.0} runs of them \
         short ({:.1} KB) going as pairs; {:.1} passes hashing again copies the submission declares nothing of; {:.2} ms what \
         changed, {:.2} ms taking the bytes, {:.2} ms the list, {:.2} ms the compares",
        join_gap(),
        n.iter().sum::<f64>(),
        n[0],
        n[1],
        n[2],
        n[3],
        n[4],
        n[5],
        s[AT_SUBMISSIONS],
        s[AT_TOUCHED],
        s[AT_SMALL],
        s[AT_SMALL_WHOLE] / 1e6,
        s[AT_RANGES],
        s[AT_JOINED],
        s[AT_GAP_BYTES] / 1e3,
        s[AT_GAPS_KEPT],
        s[AT_BYTES] / 1e6,
        s[AT_SHORT],
        s[AT_SHORT_BYTES] / 1e3,
        s[AT_REHASH_UNDECLARED],
        s[AT_GATHER_US] / 1e3,
        s[AT_TAKE_US] / 1e3,
        s[AT_LIST_US] / 1e3,
        s[AT_COMPARE_US] / 1e3,
    );
}

#[cfg(test)]
mod tests {
    use super::join_ranges_over;

    #[test]
    fn ranges_are_joined_over_short_gaps_that_are_allowed() {
        let ranges = [(0, 16), (20, 40), (100, 104), (104, 108), (400, 500)];
        // No gap: only what touches.
        assert_eq!(
            join_ranges_over(&ranges, 0, |_, _| true),
            (vec![(0, 16), (20, 40), (100, 108), (400, 500)], 0, 0)
        );
        // Gaps up to 64 bytes: the first three, not the last.
        assert_eq!(
            join_ranges_over(&ranges, 64, |_, _| true),
            (vec![(0, 108), (400, 500)], 4 + 60, 0)
        );
        // A gap a flight may write stays.
        let (out, added, left) = join_ranges_over(&ranges, 64, |from, _| from != 16);
        assert_eq!(
            (out, added, left),
            (vec![(0, 16), (20, 108), (400, 500)], 60, 1)
        );
        // Everything, with a gap as wide as the copy.
        assert_eq!(
            join_ranges_over(&ranges, 1 << 20, |_, _| true),
            (vec![(0, 500)], 4 + 60 + 292, 0)
        );
        assert_eq!(join_ranges_over(&[], 64, |_, _| true), (vec![], 0, 0));
    }
}
