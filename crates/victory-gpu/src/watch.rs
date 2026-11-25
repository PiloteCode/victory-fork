//! Which title memory the title wrote since a host copy took it (decision
//! 0007, "Copies of title memory"). The runtime reserves title GPU memory
//! with write watching (`victory-hle` xmem), so the kernel keeps a dirty bit
//! per 4 KiB page; asking for a range's written pages also clears them. Host
//! copies of the same memory can overlap (nested buffers, textures in
//! buffers), so what one asks is kept here for the others: a journal of
//! each page's last write, numbered by the asking.
//!
//! Physical-page views (xmem's sections) cannot be write-watched. There a
//! copy leaves each 64 KiB block it took read-only, and our vectored handler
//! takes the first write to it: it notes the block as written and gives the
//! write access back. The runtime tells us when it maps or unmaps views
//! ([`title_memory_remapped`]), which resets their protection. Blocks found
//! wholly uncommitted are skipped while they remain so; one query can
//! confirm a whole region, including ordinary reservations between views.
//!
//! Writes into physical pages through another view are seen too: the
//! runtime tells us the views of the pages it wrote itself ([`writing`],
//! xmem's `write_physical` and DirectStorage's page destinations), and a
//! write by the title through any view of a guarded page faults like one
//! at the guarded address (the guard makes every view of the page
//! read-only, [`open`] gives them all back; not a page of more than
//! [`MANY_VIEWS`] views). A
//! system call writing into a guarded block fails (none is known to).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

/// The unit tracked and copied: 64 KiB, the console's page (the kernel's
/// 4 KiB pages fold into it). Finer units made the checks of the world's
/// big buffers cost more than the copies they saved.
pub(crate) const PAGE: usize = 64 << 10;
/// The kernel's page, the unit of its answers.
const KERNEL_PAGE: usize = 4096;

/// A title-memory range whose changed pages another subsystem needs to read.
pub(crate) struct Tracked {
    start: usize,
    end: usize,
    seen: Mutex<BTreeMap<usize, u64>>,
}

/// Tracks a runtime GPU-memory range, including physical-page views when
/// `views` is true. Other process memory is not tracked.
pub(crate) fn track(start: usize, len: usize, views: bool) -> Option<Tracked> {
    let end = start.checked_add(len)?;
    if len == 0 || writes(start, end, views).is_none() {
        return None;
    }
    Some(Tracked {
        start,
        end,
        seen: Mutex::default(),
    })
}

struct Journal {
    /// Numbers the askings; 0 is "never".
    asked: u64,
    /// Page number → the asking that last found it written.
    written: BTreeMap<usize, u64>,
    /// Room for the kernel's answer.
    found: Vec<usize>,
    /// Page ranges asked ahead for a submission's refreshes ([`ask_ahead`])
    /// and the asking that did: `writes` answers for a range inside one
    /// from the journal, without another kernel call.
    ahead: Vec<(usize, usize)>,
    ahead_asked: u64,
    /// The ahead asking stands for a whole submission ([`ask_once`]): no
    /// other ahead asking is made, and `ahead` stays, until
    /// [`ask_once_end`].
    kept: bool,
}

static JOURNAL: Mutex<Journal> = Mutex::new(Journal {
    asked: 0,
    written: BTreeMap::new(),
    found: Vec::new(),
    ahead: Vec::new(),
    ahead_asked: 0,
    kept: false,
});

/// Ranges of one submission closer than this are asked about together:
/// walking the pages between them costs less than another kernel call (a
/// call that resets written pages costs 30-70 microseconds whatever the
/// range).
const AHEAD_GAP: usize = (8 << 20) / PAGE;
/// The most one asking covers (its answer needs 8 bytes per 4 KiB page).
const AHEAD_MAX: usize = (1 << 30) / PAGE;

/// Asks the kernel about many ranges `(start, end)` at once, before the
/// [`writes`] of each: ranges close together are one asking, and `writes`
/// then answers from the journal until [`ask_ahead_done`]. The world's
/// submissions refresh thousands of host copies (6,664 askings a frame,
/// 456 ms, in run 14). Ranges that are not write-watched, or lie alone, are
/// left to `writes`.
pub(crate) fn ask_ahead(ranges: &[(usize, usize)]) {
    let mut pieces: Vec<(usize, usize)> = ranges
        .iter()
        .filter(|r| r.0 < r.1)
        .map(|&(start, end)| (start / PAGE, end.div_ceil(PAGE)))
        .collect();
    if pieces.len() < 2 {
        return;
    }
    pieces.sort_unstable();
    let mut j = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    let j = &mut *j;
    // The submission's one asking stands ([`ask_once`]): what it does not
    // hold is left to `writes`.
    if j.kept {
        return;
    }
    // View memory the guards hold is left to `writes`, which answers it
    // from the guards ([`known_view`]): the kernel's write watch can only
    // fail there. Asked here with the others, a run holding such a range
    // failed, was asked again by halves down to single ranges, failing
    // each time, and the range then went to `writes` all the same (after
    // triippywhale d0581a55). `VICTORY_VIEW_ASK=1` asks as before.
    #[cfg(feature = "diagnostics")]
    let all = pieces.len();
    pieces.retain(|&(first, _)| !known_view(first * PAGE));
    #[cfg(feature = "diagnostics")]
    AHEAD_VIEWS.fetch_add((all - pieces.len()) as u64, std::sync::atomic::Ordering::Relaxed);
    j.asked += 1;
    j.ahead_asked = j.asked;
    j.ahead.clear();
    let mut i = 0;
    while i < pieces.len() {
        let first = pieces[i].0;
        let mut last = pieces[i].1;
        let mut k = i + 1;
        while k < pieces.len()
            && pieces[k].0 <= last + AHEAD_GAP
            && last.max(pieces[k].1) - first <= AHEAD_MAX
        {
            last = last.max(pieces[k].1);
            k += 1;
        }
        ask_run(j, &pieces[i..k]);
        i = k;
    }
}

/// Since the last frame line: the kernel askings that took a millisecond
/// or more, their microseconds, and the longest asking's. An asking walks
/// page tables, some ten microseconds for most ranges; in run 75 the 2,644
/// askings of a frame took 219 ms in the lines where the pass over the
/// heap's views ran long, 25-50 ms in the others. These numbers say
/// whether a few askings wait for something or all of them are slow.
#[cfg(feature = "diagnostics")]
static SLOW: [std::sync::atomic::AtomicU64; 3] = [const { std::sync::atomic::AtomicU64::new(0) }; 3];

#[cfg(feature = "diagnostics")]
fn slow(us: u64) {
    use std::sync::atomic::Ordering::Relaxed;
    if us >= 1000 {
        SLOW[0].fetch_add(1, Relaxed);
        SLOW[1].fetch_add(us, Relaxed);
    }
    SLOW[2].fetch_max(us, Relaxed);
}

/// Since the last frame line: ranges of view memory the guards hold that a
/// submission's askings ahead ([`ask_ahead`], [`ask_once`]) left out. Each
/// was a kernel asking that failed, or several, before.
#[cfg(feature = "diagnostics")]
pub(crate) static AHEAD_VIEWS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// [`SLOW`], taken.
#[cfg(feature = "diagnostics")]
pub(crate) fn slow_asks() -> [u64; 3] {
    [0, 1, 2].map(|i| SLOW[i].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// The submission's refreshes are done: `writes` asks the kernel again.
pub(crate) fn ask_ahead_done() {
    let mut j = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    if !j.kept {
        j.ahead.clear();
    }
}

/// Whether a submission asks the kernel once, before its first refresh,
/// about every range its lists refresh (`VICTORY_ASK_ONCE=1`; off by
/// default).
///
/// A submission's refreshes are done in groups, between the title's own
/// buffer copies (each copy flushes the refreshes recorded before it), and
/// every group asked the kernel again: 2,644 askings a frame, 70 a
/// submission. With the switch the journal answers all of them from one
/// asking made when the submission starts.
///
/// What changes: memory the title's other threads write while the
/// submission is being prepared is seen by the next submission's asking,
/// not by this one's later groups. What this submission's lists read was
/// written before they were submitted, so before the asking. Our own
/// writes into title memory while preparing are told to the journal
/// ([`wrote`]) or end the asking ([`ask_once_end`]).
pub(crate) fn once() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    // On by default since run 81 (with card buffers: 375-450 ms a frame
    // against 573-647, the picture unchanged over four minutes);
    // `VICTORY_ASK_ONCE=0` asks per range, as before.
    *ON.get_or_init(|| std::env::var_os("VICTORY_ASK_ONCE").is_none_or(|v| v != "0"))
}

/// With [`once`]: asks about `ranges` (everything a submission's lists
/// refresh) as [`ask_ahead`] does, and lets that asking stand until
/// [`ask_once_end`].
pub(crate) fn ask_once(ranges: &[(usize, usize)]) {
    SUBMISSIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    #[cfg(feature = "diagnostics")]
    track::submitting();
    if !once() {
        return;
    }
    ask_once_end();
    ask_ahead(ranges);
    let mut j = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    let j = &mut *j;
    if j.ahead.is_empty() {
        // `ask_ahead` asked nothing (fewer than two ranges).
        j.asked += 1;
        j.ahead_asked = j.asked;
    }
    // `ask_ahead` leaves a range that lies alone to `writes`, which asks
    // once per flush: with one asking for the submission that was every
    // such range asked again at each of its 70 flushes (run 76: 2,300
    // askings a frame with the switch on, as without it). They are asked
    // here, once; what is not write-watched stays with `writes`.
    for &(start, end) in ranges {
        if start >= end {
            continue;
        }
        // View memory the guards hold is not asked about: the asking can
        // only fail there, one failed kernel call for each such copy at
        // every submission, and `writes` answers it from the guards
        // (after triippywhale cc368c7c).
        if known_view(start) {
            #[cfg(feature = "diagnostics")]
            AHEAD_VIEWS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            continue;
        }
        let (first, last) = (start / PAGE, end.div_ceil(PAGE));
        if !j.ahead.iter().any(|&(a, b)| a <= first && last <= b) && ask_kernel(j, first, last) {
            j.ahead.push((first, last));
        }
    }
    j.kept = !j.ahead.is_empty();
}

/// The submission's one asking stands no longer: its refreshes are done,
/// or title memory was written in a way the journal was not told.
pub(crate) fn ask_once_end() {
    let mut j = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    if j.kept {
        j.kept = false;
        j.ahead.clear();
    }
}

/// We wrote title memory `[at, at + len)` while a submission's one asking
/// stands (the title's buffer copies, done at submission): its pages count
/// as written after that asking, so the refreshes that follow copy them.
pub(crate) fn wrote(at: usize, len: usize) {
    let mut j = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    if !j.kept || len == 0 {
        return;
    }
    j.asked += 1;
    let now = j.asked;
    // The asking that stands moves with it: `asked` and `writes` give it as
    // "now", and a page noted written after "now" stays stale for every
    // copy that takes it (copied at `now`, written at `now + 1`): copied
    // again at every refresh, and a reason to settle flights
    // (fxceless/victory-fork#410).
    j.ahead_asked = now;
    for page in at / PAGE..at.saturating_add(len).div_ceil(PAGE) {
        j.written.insert(page, now);
    }
}

/// We wrote the 64 KiB page numbered `page` of title memory through a
/// view of our own (`crate::inplace::write_own`): no guard was opened and
/// no kernel write-watch bit set, so the journal is told here. The page,
/// and the same physical page at its other title addresses (`others`,
/// page numbers), count as written at a new asking, which every host copy
/// that holds them takes at its next refresh. Returns the asking that had
/// last found `page` written (0: never) and the new one, for the copy
/// that wrote to note itself current.
pub(crate) fn wrote_own(page: usize, others: &[usize]) -> (u64, u64) {
    let mut j = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    let j = &mut *j;
    j.asked += 1;
    let now = j.asked;
    // As in [`wrote`]: an asking that stands for a submission, or for a
    // flush, moves with it, or a copy taking the page at the standing
    // asking would find it written "later" at every refresh.
    if j.kept || !j.ahead.is_empty() {
        j.ahead_asked = now;
    }
    let before = j.written.insert(page, now).unwrap_or(0);
    for &other in others {
        if other != page {
            j.written.insert(other, now);
        }
    }
    (before, now)
}

/// One kernel asking for `pieces` (page ranges in order); by halves if it
/// fails, as a run across two reservations or into memory that is not
/// write-watched does. A piece alone is left to [`writes`].
fn ask_run(j: &mut Journal, pieces: &[(usize, usize)]) {
    if pieces.len() < 2 {
        return;
    }
    let first = pieces[0].0;
    let last = pieces.iter().map(|p| p.1).max().unwrap_or(first);
    if ask_kernel(j, first, last) {
        j.ahead.push((first, last));
        return;
    }
    let (a, b) = pieces.split_at(pieces.len() / 2);
    ask_run(j, a);
    ask_run(j, b);
}

/// Asks the kernel for the written pages of pages `[first, last)` and notes
/// them in the journal under the ahead asking; false (and nothing changed)
/// if the range is not write-watched memory of one reservation.
fn ask_kernel(j: &mut Journal, first: usize, last: usize) -> bool {
    use windows_sys::Win32::System::Memory::GetWriteWatch;
    const WRITE_WATCH_FLAG_RESET: u32 = 1;
    let mut count = (last - first) * PAGE / KERNEL_PAGE;
    j.found.resize(count.max(j.found.len()), 0);
    let mut granularity = 0;
    #[cfg(feature = "diagnostics")]
    let started = std::time::Instant::now();
    // SAFETY: the answer buffer holds `count` entries.
    let failed = unsafe {
        GetWriteWatch(
            WRITE_WATCH_FLAG_RESET,
            (first * PAGE) as *const _,
            (last - first) * PAGE,
            j.found.as_mut_ptr().cast(),
            &mut count,
            &mut granularity,
        )
    } != 0;
    if failed {
        return false;
    }
    #[cfg(feature = "diagnostics")]
    {
        use crate::copystat::{C, add};
        add(C::Watches, 1);
        add(C::WatchUs, started.elapsed().as_micros() as u64);
        slow(started.elapsed().as_micros() as u64);
    }
    let now = j.ahead_asked;
    for k in 0..count {
        let page = j.found[k] / PAGE;
        j.written.insert(page, now);
    }
    true
}

/// What [`writes`] would say of `[start, end)` from the journal alone, if
/// the range lies in one asked ahead ([`ask_ahead`], [`ask_once`]): the
/// pages written since tracking began and the asking that last found each
/// written, with the number of the asking that stands (as [`writes`] gives
/// it). `None` if the kernel would have to be asked.
pub(crate) fn asked(start: usize, end: usize) -> Option<(u64, Vec<(usize, u64)>)> {
    let (first, last) = (start / PAGE, end.div_ceil(PAGE));
    let j = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    j.ahead
        .iter()
        .any(|&(a, b)| a <= first && last <= b)
        .then(|| (j.ahead_asked, j.written.range(first..last).map(|(&p, &w)| (p, w)).collect()))
}

/// The pages of `[start, end)` (whole pages) as last written, for pages
/// written since tracking began: `(page number, asking)` in page order, and
/// this asking's number, which is higher than any in the list but those the
/// kernel just reported. `None` if the memory is not write-watched (nor,
/// with `guarded`, view memory, which is then guarded).
pub(crate) fn writes(start: usize, end: usize, guarded: bool) -> Option<(u64, Vec<(usize, u64)>)> {
    use windows_sys::Win32::System::Memory::GetWriteWatch;
    const WRITE_WATCH_FLAG_RESET: u32 = 1;
    #[cfg(feature = "diagnostics")]
    let _asking = track::Asking::begin();
    let (first, last) = (start / PAGE, end.div_ceil(PAGE));
    #[cfg(feature = "diagnostics")]
    let mut j = track::locked(&JOURNAL);
    #[cfg(not(feature = "diagnostics"))]
    let mut j = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    let j = &mut *j;
    // Asked ahead with the submission's other ranges: the journal has it.
    if j.ahead.iter().any(|&(a, b)| a <= first && last <= b) {
        return Some((
            j.ahead_asked,
            j.written
                .range(first..last)
                .map(|(&p, &w)| (p, w))
                .collect(),
        ));
    }
    let mut count = (last - first) * PAGE / KERNEL_PAGE;
    j.found.resize(count.max(j.found.len()), 0);
    let mut granularity = 0;
    #[cfg(feature = "diagnostics")]
    let started = std::time::Instant::now();
    // View memory the guards already hold cannot be write-watched: the
    // kernel asking would only fail. Copies of it are flushed many times a
    // frame, each time paying that failed call before the guards answer.
    let known_view = guarded && known_view(first * PAGE);
    // SAFETY: the answer buffer holds `count` entries.
    let failed = known_view
        || unsafe {
            GetWriteWatch(
                WRITE_WATCH_FLAG_RESET,
                (first * PAGE) as *const _,
                (last - first) * PAGE,
                j.found.as_mut_ptr().cast(),
                &mut count,
                &mut granularity,
            )
        } != 0;
    #[cfg(feature = "diagnostics")]
    crate::copystat::add(
        if known_view {
            crate::copystat::C::WatchKnownView
        } else {
            crate::copystat::C::WatchFailed
        },
        failed as u64,
    );
    let guarded = if failed {
        count = 0;
        if !guarded {
            return None;
        }
        #[cfg(feature = "diagnostics")]
        let guarding = std::time::Instant::now();
        let written = guard(first * PAGE, last * PAGE);
        #[cfg(feature = "diagnostics")]
        crate::copystat::add(crate::copystat::C::GuardUs, guarding.elapsed().as_micros() as u64);
        written?
    } else {
        Vec::new()
    };
    #[cfg(feature = "diagnostics")]
    {
        use crate::copystat::{C, add};
        add(C::Watches, 1);
        add(C::WatchUs, started.elapsed().as_micros() as u64);
        slow(started.elapsed().as_micros() as u64);
    }
    j.asked += 1;
    let now = j.asked;
    let pages = j.found[..count].iter().map(|&at| at / PAGE).chain(guarded);
    for page in pages.collect::<Vec<_>>() {
        j.written.insert(page, now);
    }
    Some((
        now,
        j.written
            .range(first..last)
            .map(|(&p, &w)| (p, w))
            .collect(),
    ))
}

impl Tracked {
    /// Takes changed pages of `[from, to)`, cut to the tracked range. A
    /// partial page stays changed so a later take can read its other bytes.
    pub(crate) fn take(&self, from: usize, to: usize) -> Vec<(usize, usize)> {
        let (from, to) = (from.max(self.start), to.min(self.end));
        if from >= to {
            return Vec::new();
        }
        let Some((_, pages)) = writes(from, to, true) else {
            return Vec::new();
        };
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        for (page, write) in pages {
            if seen.get(&page).is_some_and(|&last| last >= write) {
                continue;
            }
            let (start, end) = ((page * PAGE).max(from), ((page + 1) * PAGE).min(to));
            if start >= end {
                continue;
            }
            // Keep partial pages dirty: the caller has not taken the rest.
            if (page * PAGE).max(self.start) >= from
                && ((page + 1) * PAGE).min(self.end) <= to
            {
                seen.insert(page, write);
            }
            match ranges.last_mut() {
                Some((_, last)) if *last == start => *last = end,
                _ => ranges.push((start, end)),
            }
        }
        ranges
    }
}

/// Guarded blocks of view memory.
const BLOCK: usize = PAGE;

// The write tracking's own switches (the coherence plan's stage 2; all off
// by default, the guards then work as before). Wave 7's logs place the
// guards' cost: 300-700 guard faults a frame even while the title streams
// (the 30,000-225,000 "page faults" a frame beside them are the process's
// soft faults of new mappings, not ours), but 7,000-16,000 view blocks
// walked and guarded again a frame, some 470 calls a frame guarding other
// views of a page, and 8-30 ms a frame of "guards".

/// `VICTORY_TRACK_REARM=consume`: a check guards what it found written in
/// runs cut at the views the runtime told of (a protection asked across two
/// views is refused, then retried block by block with a question to Windows
/// for each block that fails), and the other views of those blocks in runs
/// gathered over the whole check instead of one protection per block. Re-arms
/// were already made where a copy consumes the block (the check before each
/// copy, protect then copy); what this changes is how many calls they take.
fn rearm_runs() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("VICTORY_TRACK_REARM").is_ok_and(|v| v.trim() == "consume"))
}

/// `VICTORY_TRACK_VOLATILE=<n>`: a block found written at `n` consecutive
/// checks that covered it is left open: not guarded again, reported written
/// at every check (copied at every consumption, as it was), until a probe
/// guards it once more after [`VOLATILE_PROBE`] checks. A block with other
/// views is never left open (a write through it would not be seen at them).
/// Only with the view set ([`view_set`], the default). 0 or unset: off.
fn volatile_after() -> u32 {
    static N: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *N.get_or_init(|| std::env::var("VICTORY_TRACK_VOLATILE").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(0))
}

/// Checks of a block left open ([`volatile_after`]) before it is guarded
/// again once, to see whether it still is written at every cycle.
const VOLATILE_PROBE: u32 = 32;

/// `VICTORY_TRACK_WIDEN=<size>` (`256K`, `1M`; a bare number is KiB): while
/// DirectStorage writes into title memory (in the last half second), a
/// guard fault opens every guarded block of the aligned window of that size
/// around it, so a title thread filling a streamed buffer faults once per
/// window instead of once per block. In blocks; 0 (unset) off.
fn widen_blocks() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        let Ok(v) = std::env::var("VICTORY_TRACK_WIDEN") else {
            return 0;
        };
        let v = v.trim().to_ascii_uppercase();
        let (number, unit) = match v.strip_suffix('M') {
            Some(n) => (n, 1 << 20),
            None => (v.strip_suffix('K').unwrap_or(&v), 1 << 10),
        };
        let bytes = number.trim().parse::<usize>().unwrap_or(0).saturating_mul(unit);
        // A power of two of 2 to 64 blocks (128 KiB to 4 MiB).
        if bytes < 2 * BLOCK { 0 } else { (bytes / BLOCK).next_power_of_two().min(64) }
    })
}

/// When DirectStorage last wrote into title memory (milliseconds since
/// [`since_start_ms`] began): the guards' notion of "streaming".
static DISK_WRITTEN_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn since_start_ms() -> u64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    // One past the start, so that 0 stays "never".
    START.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64 + 1
}

/// `VICTORY_TRACK_HYGIENE=1`: whether a range is a view is answered from the
/// runtime's views when one of them lies in it (no walk of the address
/// space, which steps through every 64 KiB placeholder of a hole); a
/// protection refused for a run is retried block by block, and a block found
/// uncommitted answers for its whole uncommitted run (one question to
/// Windows, none for the rest; fxceless/victory-fork#987).
fn hygiene() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_TRACK_HYGIENE").is_some_and(|v| v == "1"))
}

/// Diagnostics of the write tracking, per frame with the frame-rate line
/// ([`track_line`]). Plain atomic adds; the distinct blocks are sets in
/// [`Guards`], under the lock the counting sites hold anyway.
#[cfg(feature = "diagnostics")]
mod track {
    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};

    /// Guard faults taken; of them on a block guarded with other views of
    /// its page; of them on the submitting thread.
    pub(super) const FAULTS: usize = 0;
    pub(super) const FAULTS_ALIASED: usize = 1;
    pub(super) const FAULTS_SUBMITTER: usize = 2;
    /// The handler's time for the faults it took, and of it the wait for the
    /// guards' lock (held by a check on another thread).
    pub(super) const HANDLER_NS: usize = 3;
    pub(super) const HANDLER_LOCK_NS: usize = 4;
    /// Blocks made read-only again by checks (re-arms), the protections asked
    /// for them and their time; protections of other views and their time;
    /// protections giving write access back and their time.
    pub(super) const ARMS: usize = 5;
    pub(super) const ARM_CALLS: usize = 6;
    pub(super) const ARM_NS: usize = 7;
    pub(super) const ALIAS_CALLS: usize = 8;
    pub(super) const ALIAS_NS: usize = 9;
    pub(super) const OPEN_CALLS: usize = 10;
    pub(super) const OPEN_NS: usize = 11;
    /// Protections of a run refused (retried block by block).
    pub(super) const REFUSED: usize = 12;
    /// Questions to Windows about memory by the guards, and their time.
    pub(super) const QUERIES: usize = 13;
    pub(super) const QUERY_NS: usize = 14;
    /// Askings (`writes`) on the submitting thread, their time, and of it the
    /// waits for the journal's and the guards' locks.
    pub(super) const ASKS: usize = 15;
    pub(super) const ASK_NS: usize = 16;
    pub(super) const ASK_LOCK_NS: usize = 17;
    /// View blocks made not clean, by cause: a guard fault, a write told
    /// (`writing`, `title_memory_written`), a write through another view, a
    /// mapping (mapped, unmapped, committed), first seen by a check.
    pub(super) const BY_FAULT: usize = 18;
    pub(super) const BY_TOLD: usize = 19;
    pub(super) const BY_ALIAS: usize = 20;
    pub(super) const BY_MAPPING: usize = 21;
    pub(super) const BY_FRESH: usize = 22;
    /// Blocks left open ([`super::volatile_after`]), probed again, and the
    /// reports of an open block as written (each a copy at a consumption).
    pub(super) const VOLATILE_MADE: usize = 23;
    pub(super) const VOLATILE_PROBED: usize = 24;
    pub(super) const VOLATILE_REPORTED: usize = 25;
    /// Blocks opened around a fault ([`super::widen_blocks`]).
    pub(super) const WIDENED: usize = 26;
    const N: usize = 27;

    static COUNT: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];

    pub(super) fn add(i: usize, n: u64) {
        COUNT[i].fetch_add(n, Relaxed);
    }

    pub(super) fn ns(i: usize, since: std::time::Instant) {
        add(i, since.elapsed().as_nanos() as u64);
    }

    pub(super) fn take() -> [u64; N] {
        std::array::from_fn(|i| COUNT[i].swap(0, Relaxed))
    }

    /// The thread that submits: the one that began the last submission's
    /// asking ([`super::ask_once`]).
    static SUBMITTER: AtomicU32 = AtomicU32::new(0);

    fn thread() -> u32 {
        // SAFETY: a plain query.
        unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() }
    }

    pub(super) fn submitting() {
        SUBMITTER.store(thread(), Relaxed);
    }

    pub(super) fn on_submitter() -> bool {
        SUBMITTER.load(Relaxed) == thread()
    }

    /// An asking ([`super::writes`]) on the submitting thread, timed until
    /// dropped.
    pub(super) struct Asking(Option<std::time::Instant>);

    impl Asking {
        pub(super) fn begin() -> Self {
            Asking(on_submitter().then(std::time::Instant::now))
        }
    }

    impl Drop for Asking {
        fn drop(&mut self) {
            if let Some(started) = self.0 {
                add(ASKS, 1);
                ns(ASK_NS, started);
            }
        }
    }

    /// `lock`'s mutex, the wait counted when it is held elsewhere and this is
    /// the submitting thread.
    pub(super) fn locked<T>(lock: &'static std::sync::Mutex<T>) -> std::sync::MutexGuard<'static, T> {
        match lock.try_lock() {
            Ok(g) => g,
            Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                let started = std::time::Instant::now();
                let g = lock.lock().unwrap_or_else(|e| e.into_inner());
                if on_submitter() {
                    ns(ASK_LOCK_NS, started);
                }
                g
            }
        }
    }

    /// Distinct blocks since the last line: faulted, the pages of the faults
    /// on blocks guarded with other views (by their lowest view), re-armed.
    #[derive(Default)]
    pub(super) struct Seen {
        pub(super) faulted: std::collections::BTreeSet<usize>,
        pub(super) pages: std::collections::BTreeSet<usize>,
        pub(super) armed: std::collections::BTreeSet<usize>,
    }

    impl Seen {
        pub(super) const fn new() -> Self {
            Seen {
                faulted: std::collections::BTreeSet::new(),
                pages: std::collections::BTreeSet::new(),
                armed: std::collections::BTreeSet::new(),
            }
        }
    }
}

/// Kinds of protection change, for the counters: a check's re-arm, another
/// view guarded, write access given back.
const ARM: usize = 0;
const ALIAS: usize = 1;
const OPEN: usize = 2;

/// Sets the protection of blocks `[from, to)` (title view memory) to
/// `protect`; true if Windows did. Timed by `kind` (diagnostics).
fn set_protection(from: usize, to: usize, protect: u32, kind: usize) -> bool {
    use windows_sys::Win32::System::Memory::VirtualProtect;
    #[cfg(feature = "diagnostics")]
    let started = std::time::Instant::now();
    let mut old = 0;
    // SAFETY: title view memory (or memory the guards hold); only its
    // protection changes.
    let ok = unsafe { VirtualProtect((from * BLOCK) as *const _, (to - from) * BLOCK, protect, &mut old) } != 0;
    #[cfg(feature = "diagnostics")]
    {
        let (calls, ns) = match kind {
            ARM => (track::ARM_CALLS, track::ARM_NS),
            ALIAS => (track::ALIAS_CALLS, track::ALIAS_NS),
            _ => (track::OPEN_CALLS, track::OPEN_NS),
        };
        track::add(calls, 1);
        track::ns(ns, started);
    }
    #[cfg(not(feature = "diagnostics"))]
    let _ = kind;
    if ok {
        // The copy-back measurement keeps its guards over ours (coh-lazy).
        crate::coherence::protection_set(from * BLOCK, to * BLOCK, protect);
    }
    ok
}

/// What Windows says of the memory at `at` (`VirtualQuery`), timed
/// (diagnostics); `None` if it says nothing.
fn query(at: usize) -> Option<windows_sys::Win32::System::Memory::MEMORY_BASIC_INFORMATION> {
    use windows_sys::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, VirtualQuery};
    #[cfg(feature = "diagnostics")]
    let started = std::time::Instant::now();
    // SAFETY: plain data filled in by the query.
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a valid out pointer of the right size.
    let n = unsafe { VirtualQuery(at as *const _, &mut info, size_of_val(&info)) };
    #[cfg(feature = "diagnostics")]
    {
        track::add(track::QUERIES, 1);
        track::ns(track::QUERY_NS, started);
    }
    (n != 0).then_some(info)
}

/// Counts `n` view blocks made not clean under `cause` (diagnostics).
fn count_unclean(cause: usize, n: usize) {
    #[cfg(feature = "diagnostics")]
    track::add(cause, n as u64);
    #[cfg(not(feature = "diagnostics"))]
    let _ = (cause, n);
}

/// With the frame line: the write tracking per frame over `frames`.
#[cfg(feature = "diagnostics")]
pub(crate) fn track_line(frames: f64) {
    let c = track::take();
    // The sets are freed after the lock is let go (a second of streaming
    // re-arms some 100,000 distinct blocks).
    let (seen, volatile) = {
        let mut g = guards();
        (std::mem::take(&mut g.seen), g.volatile.len())
    };
    let (faulted, pages, armed) = (seen.faulted.len(), seen.pages.len(), seen.armed.len());
    drop(seen);
    let f = |i: usize| c[i] as f64 / frames;
    let ms = |i: usize| c[i] as f64 / 1e6 / frames;
    tracing::info!(
        "write tracking per frame (VICTORY_TRACK_REARM {}, VOLATILE {}, WIDEN {} blocks, HYGIENE {}): guard faults {:.0} \
         on {:.0} distinct blocks ({:.0} on blocks guarded with other views, {:.0} distinct pages of those; {:.0} on the \
         submitting thread), the handler {:.2} ms ({:.2} waiting for the guards' lock); re-arms {:.0} blocks on {:.0} \
         distinct in {:.0} protections {:.2} ms, other views {:.0} protections {:.2} ms, {:.1} runs refused; write access \
         given back {:.0} protections {:.2} ms; blocks made not clean by: faults {:.0}, writes told {:.0}, another view \
         written {:.0}, mappings {:.0}, first seen {:.0}; questions to Windows {:.0} {:.2} ms; askings on the submitting \
         thread {:.0} {:.2} ms ({:.2} waiting for locks); open blocks {volatile} ({:.1} made, {:.1} probed, {:.0} reported \
         written unguarded); {:.0} blocks opened around faults",
        if rearm_runs() { "consume" } else { "off" },
        volatile_after(),
        widen_blocks(),
        if hygiene() { "on" } else { "off" },
        f(track::FAULTS),
        faulted as f64 / frames,
        f(track::FAULTS_ALIASED),
        pages as f64 / frames,
        f(track::FAULTS_SUBMITTER),
        ms(track::HANDLER_NS),
        ms(track::HANDLER_LOCK_NS),
        f(track::ARMS),
        armed as f64 / frames,
        f(track::ARM_CALLS),
        ms(track::ARM_NS),
        f(track::ALIAS_CALLS),
        ms(track::ALIAS_NS),
        f(track::REFUSED),
        f(track::OPEN_CALLS),
        ms(track::OPEN_NS),
        f(track::BY_FAULT),
        f(track::BY_TOLD),
        f(track::BY_ALIAS),
        f(track::BY_MAPPING),
        f(track::BY_FRESH),
        f(track::QUERIES),
        ms(track::QUERY_NS),
        f(track::ASKS),
        ms(track::ASK_NS),
        ms(track::ASK_LOCK_NS),
        f(track::VOLATILE_MADE),
        f(track::VOLATILE_PROBED),
        f(track::VOLATILE_REPORTED),
        f(track::WIDENED),
    );
}

/// The causes [`count_unclean`] counts under (diagnostics indexes; plain
/// numbers in other builds, unused there).
#[cfg(feature = "diagnostics")]
use track::{BY_ALIAS, BY_FAULT, BY_FRESH, BY_MAPPING, BY_TOLD};
#[cfg(not(feature = "diagnostics"))]
const BY_FAULT: usize = 0;
#[cfg(not(feature = "diagnostics"))]
const BY_TOLD: usize = 0;
#[cfg(not(feature = "diagnostics"))]
const BY_ALIAS: usize = 0;
#[cfg(not(feature = "diagnostics"))]
const BY_MAPPING: usize = 0;
#[cfg(not(feature = "diagnostics"))]
const BY_FRESH: usize = 0;

/// Whether the block holding `start` is view memory the guards already
/// hold (read-only by our hand, or noted absent): asking the kernel's write
/// watch about it can only fail. `VICTORY_VIEW_ASK=1` (or
/// `VICTORY_GUARD_ASK=1`) asks the kernel anyway, as before.
fn known_view(start: usize) -> bool {
    static ASK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let ask = *ASK.get_or_init(|| {
        ["VICTORY_VIEW_ASK", "VICTORY_GUARD_ASK"]
            .iter()
            .any(|n| std::env::var_os(n).is_some_and(|v| v == "1"))
    });
    if ask {
        return false;
    }
    let g = guards();
    let first = start / BLOCK;
    g.protected.contains(&first)
        || g.absent.contains(&first)
        || (view_known() && g.viewed.contains(&first))
        || (view_set() && covered(&g.views, first, first + 1))
}

struct Guards {
    /// Blocks we made read-only.
    protected: BTreeSet<usize>,
    /// Of those, the ones not written since a copy took them.
    clean: BTreeSet<usize>,
    /// Blocks already copied as zeros. While they remain wholly uncommitted,
    /// the copy can skip hundreds of MB of failing kernel reads per world frame.
    absent: BTreeSet<usize>,
    /// Blocks whose physical page was guarded together with its other views
    /// (those views' blocks are in it too): a write to one opens them all.
    /// A write range can be huge (`fread`), so only these are looked up.
    aliased: BTreeSet<usize>,
    /// With [`view_known`]: blocks made read-only at least once since the
    /// runtime last mapped or unmapped them. They are view memory whether
    /// their guard stands now or a write has opened it.
    viewed: BTreeSet<usize>,
    /// Blocks known to be view memory ([`guard`]), in order and joined:
    /// a range in them needs no question to Windows to tell.
    views: Vec<(usize, usize)>,
    /// The blocks of `views` that are not clean (written since a copy took
    /// them, never taken, or not guardable), but for the parked ones: an
    /// asking whose range holds none of them is answered in one lookup,
    /// and otherwise walks only them ([`guard_unclean`]).
    unclean: BTreeSet<usize>,
    /// View blocks found uncommitted ([`Guards::absent`]), taken out of
    /// `unclean` while they stay so ([`park`]). One goes back when the
    /// runtime maps it ([`title_memory_remapped`]), when the title commits
    /// it through the runtime ([`title_memory_committed`]), or, if it is no
    /// placeholder, when the look at those every [`park_epoch`] submissions
    /// finds it committed (an ordinary commit no one tells of). The walk
    /// over every block of a range asked Windows about each at every
    /// asking: in the first room ~86,000-96,000 such blocks, nearly all
    /// 64 KiB placeholders, each its own region, and ~88,000 questions a
    /// frame (guards 26 ms of the submitting thread's frame on an RTX 5080;
    /// 14 ms parked).
    parked: BTreeSet<usize>,
    /// The parked blocks that are no placeholder: the ones looked at again.
    parked_ordinary: BTreeSet<usize>,
    /// The epoch ([`SUBMISSIONS`] / [`park_epoch`]) of the last look at
    /// `parked_ordinary`.
    parked_at: u64,
    /// Blocks `[first, last)` a write through the kernel (or ours) is
    /// landing in ([`writing`], until its [`Writing`] is dropped): reported
    /// written at every check, and never made read-only meanwhile. A check
    /// on another thread used to guard them again under the write: the
    /// kernel then refused the rest of it (a buffered `ReadFile`, our
    /// `copy_mapped` and copy-back writes), or, once the pages were locked
    /// for the device, the write landed on a block noted clean (an
    /// unbuffered `ReadFile`), and the copies kept the bytes from before.
    busy: Vec<(usize, usize)>,
    /// With [`volatile_after`]: blocks found written (and open) at the last
    /// checks that covered them, and at how many in a row; a check that
    /// covers one and finds it clean ends its streak.
    streak: BTreeMap<usize, u32>,
    /// With [`volatile_after`]: blocks left open, never guarded again, each
    /// reported written at every check (they stay in `unclean`), with the
    /// checks since they were left open; at [`VOLATILE_PROBE`] one is guarded
    /// again (its streak kept one short, so one more written check leaves it
    /// open again).
    volatile: BTreeMap<usize, u32>,
    /// Diagnostics: distinct blocks since the last line ([`track_line`]).
    #[cfg(feature = "diagnostics")]
    seen: track::Seen,
}

/// Whether a block once guarded stays known as view memory while its
/// guard is open (`VICTORY_VIEW_KNOWN=1`; off by default).
///
/// A range whose first block the title wrote since the last check has
/// that block neither read-only nor noted absent, so nothing says it is a
/// view: the kernel's write watch is asked and fails, and then Windows is
/// asked what the memory is (`is_view`), a walk of the page tables to the
/// end of the pages that share the first one's state. In runs 222 and 225
/// that was 338-450 failed askings a frame and 6.5-7.8 ms a frame of the
/// submitting thread in `VirtualQuery`. With the switch the block is
/// remembered from the time it was first made read-only until the runtime
/// maps or unmaps it ([`title_memory_remapped`]).
fn view_known() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_VIEW_KNOWN").is_some_and(|v| v == "1"))
}

static GUARDS: Mutex<Guards> = Mutex::new(Guards {
    protected: BTreeSet::new(),
    clean: BTreeSet::new(),
    absent: BTreeSet::new(),
    aliased: BTreeSet::new(),
    viewed: BTreeSet::new(),
    views: Vec::new(),
    unclean: BTreeSet::new(),
    parked: BTreeSet::new(),
    parked_ordinary: BTreeSet::new(),
    parked_at: 0,
    busy: Vec::new(),
    streak: BTreeMap::new(),
    volatile: BTreeMap::new(),
    #[cfg(feature = "diagnostics")]
    seen: track::Seen::new(),
});

/// Forgets the open-block state ([`volatile_after`]) of blocks `[first,
/// last)`: their mapping changed, another view of them appeared, or another
/// owner (the GPU) takes them. They are guarded again at their next check.
fn forget_volatile(g: &mut Guards, first: usize, last: usize) {
    if g.volatile.is_empty() && g.streak.is_empty() {
        return;
    }
    let gone: Vec<usize> = g.volatile.range(first..last).map(|(&b, _)| b).collect();
    for block in gone {
        g.volatile.remove(&block);
    }
    let gone: Vec<usize> = g.streak.range(first..last).map(|(&b, _)| b).collect();
    for block in gone {
        g.streak.remove(&block);
    }
}

/// Whether a write through the kernel is landing in `block` ([`writing`]).
fn is_busy(g: &Guards, block: usize) -> bool {
    !g.busy.is_empty() && g.busy.iter().any(|&(a, b)| (a..b).contains(&block))
}

fn guards() -> std::sync::MutexGuard<'static, Guards> {
    // Diagnostics: the submitting thread's waits for the lock (a fault's
    // handler or another thread's check holding it) are counted.
    #[cfg(feature = "diagnostics")]
    return track::locked(&GUARDS);
    #[cfg(not(feature = "diagnostics"))]
    GUARDS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Runs `f` with the guards' lock held: no protection of the guards changes
/// meanwhile. For `crate::coherence`, whose one-shot guards on single pages
/// must not race a change of the blocks around them (its
/// `protection_set` is told of every such change, under this lock).
pub(crate) fn hold_guards<R>(f: impl FnOnce() -> R) -> R {
    let _g = guards();
    f()
}

/// `[first, last)` added to `ranges` (in order, joined). For ranges seen
/// rarely (the views, once per new range): it sorts the whole list.
fn add_range(ranges: &mut Vec<(usize, usize)>, first: usize, last: usize) {
    if first >= last {
        return;
    }
    ranges.push((first, last));
    ranges.sort_unstable();
    let mut joined: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    for &(a, b) in ranges.iter() {
        match joined.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => joined.push((a, b)),
        }
    }
    *ranges = joined;
}

/// Whether blocks `[first, last)` all lie in `ranges` (in order, joined).
fn covered(ranges: &[(usize, usize)], first: usize, last: usize) -> bool {
    let at = ranges.partition_point(|r| r.1 <= first);
    ranges.get(at).is_some_and(|r| r.0 <= first && last <= r.1)
}

/// Whether to keep views' not-clean blocks in a set and answer from it
/// ([`Guards::unclean`], [`Guards::parked`]; `VICTORY_VIEW_SET=0` walks
/// every block of a range, as before). Only where Windows tells
/// placeholders apart ([`placeholders_told`]).
fn view_set() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_VIEW_SET").is_none_or(|v| v != "0") && placeholders_told())
}

/// Whether [`in_placeholder`] knows a placeholder, asked once of one of
/// our own. Where it does not (Wine may not fill the flag), every parked
/// block would be looked at each epoch, more questions than the walk
/// asks: the walk stays there.
fn placeholders_told() -> bool {
    use windows_sys::Win32::System::Memory::{
        MEM_RELEASE, MEM_RESERVE, MEM_RESERVE_PLACEHOLDER, PAGE_NOACCESS, VirtualAlloc2, VirtualFree,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    // SAFETY: a fresh placeholder of one block, released at once.
    let p = unsafe {
        VirtualAlloc2(
            GetCurrentProcess(),
            std::ptr::null(),
            BLOCK,
            MEM_RESERVE | MEM_RESERVE_PLACEHOLDER,
            PAGE_NOACCESS,
            std::ptr::null_mut(),
            0,
        )
    } as usize;
    if p == 0 {
        return false;
    }
    let told = in_placeholder(p / BLOCK);
    // SAFETY: our own placeholder, known to nothing else.
    unsafe { VirtualFree(p as _, 0, MEM_RELEASE) };
    if !told {
        tracing::info!("Windows does not tell placeholders apart here: view memory askings walk every block (VICTORY_VIEW_SET)");
    }
    told
}

/// Since the last frame line ([`view_stats`]): view askings answered in one
/// lookup, view askings that walked their not-clean blocks, the blocks they
/// walked, the questions to Windows of whether a range is a view, the
/// questions whether blocks found uncommitted still are, and the parked
/// blocks found committed ([`Guards::parked`]).
static VIEW_STATS: [std::sync::atomic::AtomicU64; 6] = [const { std::sync::atomic::AtomicU64::new(0) }; 6];

/// [`VIEW_STATS`], taken.
pub(crate) fn view_stats() -> [u64; 6] {
    [0, 1, 2, 3, 4, 5].map(|i| VIEW_STATS[i].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// Submissions' askings begun ([`ask_once`]).
static SUBMISSIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Submissions' askings between looks at the parked blocks that are no
/// placeholder (`VICTORY_PARK_EPOCH`, default 32; 1 looks at every one).
fn park_epoch() -> u64 {
    static N: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("VICTORY_PARK_EPOCH")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&n| n > 0)
            .unwrap_or(32)
    })
}

/// The view blocks parked as uncommitted now ([`Guards::parked`]), and
/// those of them that are no placeholder.
pub(crate) fn parked_blocks() -> (usize, usize) {
    let g = guards();
    (g.parked.len(), g.parked_ordinary.len())
}

/// Parks `block`, found uncommitted and not clean ([`Guards::parked`]). A
/// block in a placeholder is not looked at again until the runtime maps
/// it: a plain commit into a placeholder fails, only a call that replaces
/// one fills it, and on title memory the runtime's are the only such calls
/// and tell of themselves ([`title_memory_remapped`]).
fn park(g: &mut Guards, block: usize) {
    g.unclean.remove(&block);
    g.parked.insert(block);
    if !in_placeholder(block) {
        g.parked_ordinary.insert(block);
    }
}

/// Whether `block` lies in a placeholder.
fn in_placeholder(block: usize) -> bool {
    use windows_sys::Win32::System::Memory::{
        MemoryRegionInfo, QueryVirtualMemoryInformation, WIN32_MEMORY_REGION_INFORMATION,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    // The region's flag for a placeholder reservation, after Private,
    // MappedDataFile, MappedImage, MappedPageFile, MappedPhysical,
    // DirectMapped, SoftwareEnclave and PageSize64K: the SDK's header calls
    // these bits reserved, the kernel fills them (Windows 10 1803 on). A
    // fresh, split or unmapped-to placeholder shows it, an ordinary
    // reservation (even one with no access) does not; `VirtualQuery` tells
    // the two apart by nothing.
    const PLACEHOLDER: u32 = 1 << 8;
    #[cfg(feature = "diagnostics")]
    let started = std::time::Instant::now();
    // SAFETY: plain data filled in by the query.
    let mut info: WIN32_MEMORY_REGION_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a valid out pointer of the right size; the returned size is
    // optional. Free memory fails the query: no placeholder.
    let ok = unsafe {
        QueryVirtualMemoryInformation(
            GetCurrentProcess(),
            (block * BLOCK) as *const _,
            MemoryRegionInfo,
            (&raw mut info).cast(),
            size_of_val(&info),
            std::ptr::null_mut(),
        )
    } != 0;
    #[cfg(feature = "diagnostics")]
    {
        track::add(track::QUERIES, 1);
        track::ns(track::QUERY_NS, started);
    }
    // SAFETY: the flags word of the union, filled in by the query.
    ok && unsafe { info.Anonymous.Flags } & PLACEHOLDER != 0
}

/// Once per [`park_epoch`] submissions: the parked blocks that are no
/// placeholder and were committed since go back to the unclean set, no
/// longer absent, for the next asking that covers them to find written.
/// One question to Windows per uncommitted region.
fn unpark_committed(g: &mut Guards) {
    use std::sync::atomic::Ordering::Relaxed;
    let epoch = SUBMISSIONS.load(Relaxed) / park_epoch();
    if g.parked_at == epoch {
        return;
    }
    g.parked_at = epoch;
    let parked: Vec<usize> = g.parked_ordinary.iter().copied().collect();
    let mut until = 0;
    for block in parked {
        if block >= until {
            VIEW_STATS[4].fetch_add(1, Relaxed);
            // The block itself if it is (partly) committed now.
            until = uncommitted_end(block);
        }
        if block >= until {
            VIEW_STATS[5].fetch_add(1, Relaxed);
            g.parked_ordinary.remove(&block);
            g.parked.remove(&block);
            g.absent.remove(&block);
            g.unclean.insert(block);
            count_unclean(BY_MAPPING, 1);
        }
    }
}

/// For view memory: the pages of `[start, end)`'s blocks written since
/// they were last guarded (all of a block's pages), guarding them again
/// before the caller copies them. `None` if the memory is not a view.
fn guard(start: usize, end: usize) -> Option<Vec<usize>> {
    // A range whose first block the guards hold already (read-only by our
    // hand, or noted absent) is a view: no question to Windows. The
    // question walks the page tables to the end of the pages that share the
    // first one's state, and a guarded view is one long run of read-only
    // pages: 0.6 ms for the title's 174 MB heap, 16 times a frame, and most
    // of the 68 ms a frame of "guards" (run 58).
    // `VICTORY_GUARD_ASK=1` asks every time, as before.
    static ASK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let (first, last) = (start / BLOCK, end.div_ceil(BLOCK));
    // With the view set, a range known to be views is one too, whatever
    // its first block's state: a written first block (open, not absent)
    // asked Windows again, the walk to the region's end.
    let known = !*ASK.get_or_init(|| std::env::var_os("VICTORY_GUARD_ASK").is_some_and(|v| v == "1")) && {
        let g = guards();
        g.protected.contains(&first)
            || g.absent.contains(&first)
            || (view_known() && g.viewed.contains(&first))
            || (view_set() && covered(&g.views, first, last))
    };
    if !known {
        VIEW_STATS[3].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if !is_view(start, end) {
            return None;
        }
    }
    install();
    let mut g = guards();
    if view_set() {
        return Some(guard_unclean(&mut g, first, last));
    }
    let mut written = Vec::new();
    let mut run: Option<usize> = None;
    // The range's clean blocks, in order: read off the set in one pass,
    // not looked up one block at a time. A frame asks about some 10 GB of
    // view memory, 150,000 blocks, nearly all clean: the lookups were most
    // of the 60-90 ms a frame of "guards" (run 54). What the loop adds to
    // the set meanwhile is the block it stands at or ones before it.
    let clean: Vec<usize> = g.clean.range(first..last).copied().collect();
    if clean.len() == last - first {
        // Nothing written, mapped or new since the range was last taken.
        return Some(written);
    }
    let mut next_clean = 0;
    let mut uncommitted_until = first;
    for block in first..=last {
        while next_clean < clean.len() && clean[next_clean] < block {
            next_clean += 1;
        }
        let is_clean = next_clean < clean.len() && clean[next_clean] == block;
        let mut open = block < last && !is_clean;
        if open && g.absent.contains(&block) {
            // Ordinary commits need no runtime mapping notification. Reuse
            // this asking's query for the following blocks in the same region.
            if block >= uncommitted_until {
                uncommitted_until = uncommitted_end(block);
            }
            open = block >= uncommitted_until;
            if open {
                g.absent.remove(&block);
            }
        }
        if open {
            written.extend(block * BLOCK / PAGE..(block + 1) * BLOCK / PAGE);
            if is_busy(&g, block) {
                // A write is landing there: reported written, left open.
                if let Some(from) = run.take() {
                    protect(&mut g, from, block);
                }
                continue;
            }
            if g.protected.contains(&block) {
                // Written through the runtime, or through another view of
                // its pages (`open`), still read-only. That view may be one
                // no copy checks: guarded again here, with the others.
                g.clean.insert(block);
                if view_guard() {
                    protect_aliases(&mut g, block);
                }
            } else {
                run.get_or_insert(block);
                continue;
            }
        }
        if let Some(from) = run.take() {
            protect(&mut g, from, block);
        }
    }
    Some(written)
}

/// [`guard`] with [`view_set`]: the same answer, from the view blocks that
/// are not clean ([`Guards::unclean`]) instead of every block of the range.
/// A range seen for the first time is noted as views, its blocks not clean
/// yet put in the set (once). Then: none in the range, nothing to do; else
/// each, in order, as the walk over the whole range treats it (one found
/// uncommitted parked; one written through the runtime, or guarded with
/// another view of its page, but still read-only noted clean; the others
/// guarded again in runs).
fn guard_unclean(g: &mut Guards, first: usize, last: usize) -> Vec<usize> {
    guard_unclean_as(g, first, last, volatile_after(), rearm_runs())
}

/// [`guard_unclean`] with the switches given: `volatile` as
/// [`volatile_after`] (0: off), `joined` as [`rearm_runs`].
fn guard_unclean_as(g: &mut Guards, first: usize, last: usize, volatile: u32, joined: bool) -> Vec<usize> {
    use std::sync::atomic::Ordering::Relaxed;
    unpark_committed(g);
    if !covered(&g.views, first, last) {
        let fresh: Vec<usize> = (first..last)
            .filter(|b| !g.clean.contains(b) && !covered(&g.views, *b, *b + 1))
            .collect();
        count_unclean(BY_FRESH, fresh.len());
        g.unclean.extend(fresh);
        add_range(&mut g.views, first, last);
    }
    let unclean: Vec<usize> = g.unclean.range(first..last).copied().collect();
    if volatile > 0 && !g.streak.is_empty() {
        // A check that covers a block of a streak and finds it clean ends
        // the streak: the block was not written between two consumptions.
        let ended: Vec<usize> =
            g.streak.range(first..last).map(|(&b, _)| b).filter(|b| unclean.binary_search(b).is_err()).collect();
        for block in ended {
            g.streak.remove(&block);
        }
    }
    let mut written = Vec::new();
    if unclean.is_empty() {
        VIEW_STATS[0].fetch_add(1, Relaxed);
        return written;
    }
    VIEW_STATS[1].fetch_add(1, Relaxed);
    VIEW_STATS[2].fetch_add(unclean.len() as u64, Relaxed);
    // With `VICTORY_TRACK_REARM=consume` the check's re-arms are gathered
    // and made at its end ([`rearm`]): its runs, and the blocks still
    // guarded whose other views are guarded again.
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut also: Vec<usize> = Vec::new();
    let mut run: Option<(usize, usize)> = None;
    let flush = |g: &mut Guards, run: &mut Option<(usize, usize)>, runs: &mut Vec<(usize, usize)>| {
        if let Some((a, b)) = run.take() {
            if joined {
                runs.push((a, b));
            } else {
                protect(g, a, b);
            }
        }
    };
    for block in unclean {
        if g.absent.contains(&block) {
            // Found uncommitted before and still in the set (`protect`
            // parks the blocks it finds so): parked while it stays so.
            VIEW_STATS[4].fetch_add(1, Relaxed);
            if block < uncommitted_end(block) {
                park(g, block);
                flush(g, &mut run, &mut runs);
                continue;
            }
            g.absent.remove(&block);
        }
        written.extend(block * BLOCK / PAGE..(block + 1) * BLOCK / PAGE);
        if is_busy(g, block) {
            // A write is landing there: reported written, left open (and
            // not clean) until it is done.
            flush(g, &mut run, &mut runs);
            continue;
        }
        if g.protected.contains(&block) {
            // Written through the runtime, or through another view of its
            // pages (`open`), or guarded with another view of its page:
            // still read-only. The view written may be one no copy checks:
            // guarded again here, with the others (as in the walk).
            g.clean.insert(block);
            g.unclean.remove(&block);
            flush(g, &mut run, &mut runs);
            if view_guard() {
                if joined {
                    also.push(block);
                } else {
                    protect_aliases(g, block);
                }
            }
            continue;
        }
        // Open, and written since its last guard (or never guarded): left
        // open if it is written at every consumption.
        if volatile > 0 && keep_open(g, block, volatile) {
            flush(g, &mut run, &mut runs);
            continue;
        }
        run = match run {
            Some((a, b)) if b == block => Some((a, block + 1)),
            other => {
                if let Some((a, b)) = other {
                    if joined {
                        runs.push((a, b));
                    } else {
                        protect(g, a, b);
                    }
                }
                Some((block, block + 1))
            }
        };
    }
    flush(g, &mut run, &mut runs);
    if joined {
        rearm(g, &runs, &also);
    }
    written
}

/// With [`volatile_after`] `n`: whether `block`, found open and written at
/// a check, is left open (not guarded again; it stays not clean, so every
/// check reports it written). One found written at `n` consecutive checks
/// that covered it is; one left open is guarded again once after
/// [`VOLATILE_PROBE`] checks, its streak kept one short. Never a block with
/// other views: a write through it would not fault, and the copies at the
/// other views would keep the bytes from before.
fn keep_open(g: &mut Guards, block: usize, n: u32) -> bool {
    if let Some(age) = g.volatile.get_mut(&block) {
        *age += 1;
        if *age < VOLATILE_PROBE && !g.aliased.contains(&block) {
            #[cfg(feature = "diagnostics")]
            track::add(track::VOLATILE_REPORTED, 1);
            return true;
        }
        g.volatile.remove(&block);
        g.streak.insert(block, n - 1);
        #[cfg(feature = "diagnostics")]
        track::add(track::VOLATILE_PROBED, 1);
        return false;
    }
    let streak = g.streak.entry(block).or_insert(0);
    *streak += 1;
    if *streak < n {
        return false;
    }
    g.streak.remove(&block);
    if g.aliased.contains(&block) || crate::inplace::pages_of(block * BLOCK).len() > 1 {
        return false;
    }
    g.volatile.insert(block, 0);
    #[cfg(feature = "diagnostics")]
    track::add(track::VOLATILE_MADE, 1);
    true
}

/// [`guard_unclean`]'s re-arms with [`rearm_runs`]: the check's `runs`
/// guarded ([`protect_here`] cuts them at the runtime's views), then the
/// other views of their blocks and of `also` (blocks still guarded whose
/// other views a write may have opened), gathered over the whole check and
/// guarded in runs, one protection a run where Windows takes it. The same
/// blocks end guarded as with [`protect`] and [`protect_aliases`] block by
/// block, before the caller copies anything.
fn rearm(g: &mut Guards, runs: &[(usize, usize)], also: &[usize]) {
    for &(from, to) in runs {
        protect_here(g, from, to);
    }
    if !view_guard() {
        return;
    }
    let mut aliases: Vec<usize> = Vec::new();
    let mut gather = |g: &mut Guards, block: usize| {
        let shown = crate::inplace::pages_of(block * BLOCK);
        if shown.len() < 2 || shown.len() > MANY_VIEWS {
            return;
        }
        #[cfg(feature = "diagnostics")]
        ALIASES[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        for alias in shown {
            let alias = alias / BLOCK;
            g.aliased.insert(alias);
            if alias != block {
                aliases.push(alias);
            }
        }
    };
    for &(from, to) in runs {
        // As `protect`: the blocks of the runtime's views only.
        for (base, bytes) in crate::inplace::views_between(from.saturating_mul(BLOCK), to.saturating_mul(BLOCK)) {
            let first = (base / BLOCK).max(from);
            let last = (base.saturating_add(bytes).div_ceil(BLOCK)).min(to);
            for block in first..last {
                gather(g, block);
            }
        }
    }
    for &block in also {
        gather(g, block);
    }
    aliases.sort_unstable();
    aliases.dedup();
    aliases.retain(|&b| !g.protected.contains(&b) && !is_busy(g, b));
    let mut i = 0;
    while i < aliases.len() {
        let mut j = i + 1;
        while j < aliases.len() && aliases[j] == aliases[j - 1] + 1 {
            j += 1;
        }
        for (a, b) in view_pieces(aliases[i], aliases[j - 1] + 1) {
            if b - a > 1 && set_protection(a, b, windows_sys::Win32::System::Memory::PAGE_READONLY, ALIAS) {
                // As `protect_alias`, for the run: guarded, not noted clean.
                g.protected.extend(a..b);
                if view_known() {
                    g.viewed.extend(a..b);
                }
                forget_volatile(g, a, b);
                continue;
            }
            for block in a..b {
                protect_alias(g, block);
            }
        }
        i = j;
    }
}

/// Blocks `[from, to)` cut where a view the runtime told of begins or ends
/// (a protection asked across two views is refused); one piece for a block.
fn view_pieces(from: usize, to: usize) -> Vec<(usize, usize)> {
    if to - from < 2 {
        return vec![(from, to)];
    }
    let mut cuts: Vec<usize> = Vec::new();
    for (base, bytes) in crate::inplace::views_between(from.saturating_mul(BLOCK), to.saturating_mul(BLOCK)) {
        for edge in [base / BLOCK, base.saturating_add(bytes).div_ceil(BLOCK)] {
            if edge > from && edge < to {
                cuts.push(edge);
            }
        }
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut out = Vec::with_capacity(cuts.len() + 1);
    let mut at = from;
    for cut in cuts {
        out.push((at, cut));
        at = cut;
    }
    out.push((at, to));
    out
}

/// Whether `[start, end)` is view memory. The title's views lie in
/// placeholders, mapped and unmapped block by block as it streams, so a
/// range can begin in a block that is not mapped right now: the first
/// pages of the range that are there decide. A range with nothing there
/// counts as a view (its blocks are noted absent, and looked at again when
/// the runtime maps them).
///
/// Looking at the first address only took such a range for "not a view":
/// its copy was marked not watched for good and copied whole at every
/// refresh, around its unmapped pages (run 54: 300 such ranges, 570 MB
/// copied a frame for them, 255 copies a frame reading around holes).
/// `VICTORY_GUARD_HOLES=0` looks at the first address only, as before.
fn is_view(start: usize, end: usize) -> bool {
    use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEM_MAPPED};
    static HOLES: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let holes = *HOLES.get_or_init(|| std::env::var_os("VICTORY_GUARD_HOLES").is_none_or(|v| v != "0"));
    // With `VICTORY_TRACK_HYGIENE=1`: a view the runtime told of lies in the
    // range, so it is one (the walk below would find that view, or a hole
    // before it, and say so too), with no question to Windows.
    if hygiene() && holes && !crate::inplace::views_between(start, end).is_empty() {
        return true;
    }
    let mut at = start;
    loop {
        let Some(info) = query(at) else {
            return false;
        };
        if info.Type == MEM_MAPPED {
            return true;
        }
        // Committed memory that is no view (the title's heaps, ours), or
        // the answer as it was before.
        if info.State == MEM_COMMIT || !holes {
            return false;
        }
        // A hole (a placeholder, or free): on to what follows it.
        let next = (info.BaseAddress as usize).saturating_add(info.RegionSize);
        if next <= at || next >= end {
            return true;
        }
        at = next;
    }
}

/// Makes blocks `[from, to)` read-only and notes them guarded and clean;
/// block by block if they span views, leaving out any that fail, and
/// noting those not committed (unmapped) absent. The same physical pages
/// at every other view of them are made read-only too: a write through
/// one of those is this memory written, and only a fault can say so.
/// `VICTORY_VIEW_GUARD=0` leaves the other views as they are, as before.
fn protect(g: &mut Guards, from: usize, to: usize) {
    protect_here(g, from, to);
    if !view_guard() {
        return;
    }
    for (base, bytes) in crate::inplace::views_between(from.saturating_mul(BLOCK), to.saturating_mul(BLOCK)) {
        let first = (base / BLOCK).max(from);
        let last = (base.saturating_add(bytes).div_ceil(BLOCK)).min(to);
        for block in first..last {
            protect_aliases(g, block);
        }
    }
}

/// Makes the other views of `block`'s pages read-only and notes them all
/// guarded together, `block` included (see [`protect`]).
fn protect_aliases(g: &mut Guards, block: usize) {
    let shown = crate::inplace::pages_of(block * BLOCK);
    // One view (nothing else to guard), or the page the title shows
    // wherever it has nothing (thousands of views): each view of that one
    // is guarded by itself.
    if shown.len() < 2 || shown.len() > MANY_VIEWS {
        return;
    }
    #[cfg(feature = "diagnostics")]
    ALIASES[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    for alias in shown {
        let alias = alias / BLOCK;
        g.aliased.insert(alias);
        if alias != block {
            protect_alias(g, alias);
        }
    }
}

/// Views of one page past which its views are not guarded together.
pub(crate) const MANY_VIEWS: usize = 64;

/// Whether all the views of a guarded page are guarded together
/// (`VICTORY_VIEW_GUARD`).
fn view_guard() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_VIEW_GUARD").is_none_or(|v| v != "0"))
}

/// Diagnostics: blocks guarded together with their other views, blocks
/// noted written because another view of theirs was written, and blocks
/// opened because a view of theirs was mapped; since the start.
#[cfg(feature = "diagnostics")]
pub(crate) static ALIASES: [std::sync::atomic::AtomicU64; 3] = [const { std::sync::atomic::AtomicU64::new(0) }; 3];

/// Makes `block` read-only: another view of pages a copy has just taken
/// through its own view, so that a write through this one faults too. It is
/// not noted clean. The copies of title memory at this view have not taken
/// the pages: noted clean, the block would not be reported written at their
/// next check and they would keep the bytes from before the write (`guard`
/// reports a block that is read-only and not clean as written, then notes it
/// clean). A block that is clean stays so: had the pages been written since
/// its copies took them, `open` would have noted it written.
fn protect_alias(g: &mut Guards, block: usize) {
    use windows_sys::Win32::System::Memory::PAGE_READONLY;
    if g.protected.contains(&block) || is_busy(g, block) {
        return;
    }
    if set_protection(block, block + 1, PAGE_READONLY, ALIAS) {
        g.protected.insert(block);
        if view_known() {
            g.viewed.insert(block);
        }
        // Guarded with other views: no longer an open block.
        forget_volatile(g, block, block + 1);
    }
}

/// [`protect`] for one range, without the other views of its pages; the
/// blocks a write is landing in ([`writing`]) are left out.
fn protect_here(g: &mut Guards, from: usize, to: usize) {
    use windows_sys::Win32::System::Memory::PAGE_READONLY;
    if !g.busy.is_empty() && g.busy.iter().any(|&(a, b)| a < to && b > from) {
        let mut at = from;
        while at < to {
            if is_busy(g, at) {
                at += 1;
                continue;
            }
            let mut end = at + 1;
            while end < to && !is_busy(g, end) {
                end += 1;
            }
            protect_here(g, at, end);
            at = end;
        }
        return;
    }
    // With `VICTORY_TRACK_REARM=consume`: cut at the runtime's views first,
    // so that no protection is asked across two of them (refused, then
    // retried block by block).
    if rearm_runs() && to - from > 1 {
        let pieces = view_pieces(from, to);
        if pieces.len() > 1 {
            for (a, b) in pieces {
                protect_here(g, a, b);
            }
            return;
        }
    }
    let set = |from: usize, to: usize| set_protection(from, to, PAGE_READONLY, ARM);
    let note = |g: &mut Guards, from: usize, to: usize| {
        g.protected.extend(from..to);
        g.clean.extend(from..to);
        if view_known() {
            g.viewed.extend(from..to);
        }
        for block in from..to {
            g.unclean.remove(&block);
        }
        #[cfg(feature = "diagnostics")]
        {
            track::add(track::ARMS, (to - from) as u64);
            g.seen.armed.extend(from..to);
        }
    };
    if set(from, to) {
        note(g, from, to);
        return;
    }
    #[cfg(feature = "diagnostics")]
    if to - from > 1 {
        track::add(track::REFUSED, 1);
    }
    let mut block = from;
    while block < to {
        if set(block, block + 1) {
            note(g, block, block + 1);
            block += 1;
            continue;
        }
        let end = uncommitted_end(block);
        if block >= end {
            block += 1;
            continue;
        }
        // With `VICTORY_TRACK_HYGIENE=1` a block found uncommitted answers
        // for the whole uncommitted run it begins (one question, and no
        // protection asked for the rest of it); else for itself.
        let end = if hygiene() { end.min(to) } else { block + 1 };
        for at in block..end {
            g.absent.insert(at);
            // Out of the unclean set while it stays so ([`Guards::parked`]).
            if g.unclean.contains(&at) {
                park(g, at);
            }
        }
        block = end;
    }
}

/// The end (exclusive) of the whole uncommitted blocks starting at `block`.
fn uncommitted_end(block: usize) -> usize {
    use windows_sys::Win32::System::Memory::{MEM_FREE, MEM_RESERVE};
    match query(block * BLOCK) {
        Some(info) if matches!(info.State, MEM_RESERVE | MEM_FREE) => {
            (info.BaseAddress as usize).saturating_add(info.RegionSize) / BLOCK
        }
        _ => block,
    }
}

/// Gives write access back to the guarded blocks of `[start, end)` and
/// notes them written; true if any was guarded. The same physical pages at
/// every other view of them are noted written too: the write that faulted
/// (or is ours, `crate::host::copy_mapped`) goes through one view, and
/// every guard over the pages must copy them again.
///
/// Those other views stay read-only. `guard` reports a block that is
/// read-only and not clean as written, and a write through one of them
/// still faults and opens it. Opening them all cost two changes of
/// protection for every other view of every page written (one here, one
/// when the next check guarded it again), most of them for views the
/// title did not write through before that check. `cause` names why, for
/// the counters ([`count_unclean`]).
fn open(g: &mut Guards, start: usize, end: usize, cause: usize) -> bool {
    let any = open_here(g, start, end, cause);
    if start >= end || g.aliased.is_empty() {
        return any;
    }
    let aliased: Vec<usize> = g.aliased.range(start / BLOCK..end.div_ceil(BLOCK)).copied().collect();
    for block in aliased {
        // Guarded with the others again when its own check, or the next
        // check of one of them, finds the pages written.
        g.aliased.remove(&block);
        for alias in crate::inplace::pages_of(block * BLOCK) {
            let alias = alias / BLOCK;
            if alias != block && g.protected.contains(&alias) && g.clean.remove(&alias) {
                // A view block: not clean ([`Guards::unclean`]), or the
                // asking answered from that set would not see it written.
                if covered(&g.views, alias, alias + 1) {
                    g.unclean.insert(alias);
                }
                count_unclean(BY_ALIAS, 1);
                #[cfg(feature = "diagnostics")]
                ALIASES[1].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }
    any
}

/// [`open`] for one range, without the other views of its pages.
fn open_here(g: &mut Guards, start: usize, end: usize, cause: usize) -> bool {
    use windows_sys::Win32::System::Memory::PAGE_READWRITE;
    let blocks: Vec<usize> = g
        .protected
        .range(start / BLOCK..end.div_ceil(BLOCK))
        .copied()
        .collect();
    for &block in &blocks {
        g.protected.remove(&block);
        g.clean.remove(&block);
        // A block we made read-only.
        set_protection(block, block + 1, PAGE_READWRITE, OPEN);
        // A view block: not clean ([`Guards::unclean`]).
        if covered(&g.views, block, block + 1) {
            g.unclean.insert(block);
        }
    }
    count_unclean(cause, blocks.len());
    !blocks.is_empty()
}

/// Before our own write into title memory `[at, at + len)` through the
/// kernel (which does not fault into our handler): opens its guarded
/// blocks, and keeps them open while the returned [`Writing`] lives (hold
/// it until the write has landed). The runtime calls it (as
/// `kernel_will_write`) around the title's kernel reads into its own
/// memory. Upstream c75fdba's busy window, on these guards.
pub fn writing(at: usize, len: usize) -> Writing {
    // The one-shot guards of `crate::coherence` there come off first: the
    // kernel's write would fail on one.
    crate::coherence::written_over(at, len);
    // The length is the title's (`fread`'s size times count, saturated).
    let end = at.saturating_add(len);
    let (first, last) = (at / BLOCK, end.div_ceil(BLOCK));
    {
        let mut g = guards();
        g.busy.push((first, last));
        open(&mut g, at, end, BY_TOLD);
    }
    // A placed texture there changes with no descriptor written.
    crate::descriptor::host::remapped(at, len);
    Writing { at, len, first, last }
}

/// A write landing in title memory ([`writing`]): its blocks stay open,
/// and are reported written at every check, until this is dropped; then
/// they (and their other views) are noted written once more, and the
/// placed textures there are due again, now that the bytes are there.
#[must_use = "the blocks stay open only while this lives: hold it until the write has landed"]
pub struct Writing {
    at: usize,
    len: usize,
    first: usize,
    last: usize,
}

impl Drop for Writing {
    fn drop(&mut self) {
        {
            let mut g = guards();
            if let Some(i) = g.busy.iter().position(|&r| r == (self.first, self.last)) {
                g.busy.swap_remove(i);
            }
            open(&mut g, self.at, self.at.saturating_add(self.len), BY_TOLD);
        }
        crate::descriptor::host::remapped(self.at, self.len);
    }
}

/// What `VICTORY_PAGE_WRITES` asks to be done with a write the guards do
/// not see once its bytes are there ([`title_memory_written`]), as bits:
/// 1, the guarded blocks under it are opened and count as written (host
/// copies of buffers and textures read through tables take them again);
/// 2, the placed textures picked by index that lie under it are due again;
/// 4, both also for reads to plain addresses when they have finished.
/// Unset or `1`: bits 1 and 2 (bit 2 also for finished reads to plain
/// addresses). `guards`: bit 1 alone (the default until fxceless #995).
/// `0`: nothing, counted only (the default until run 199). `slots`: bit 2
/// alone. `all`: everything.
/// (Bit 1: in every run 10,000 to 50,000 guarded blocks of host copies lay
/// under such writes and kept the bytes from before; runs 197 and 198
/// opened them, 9,766 and 10,796 blocks, with nothing worse for it.
/// Bit 2 by default since #995 (tunczzz, runs n15-n23): 5,500 to 7,300
/// placed textures a run had been uploaded before such a write landed in
/// their memory (a quarter of all the slots under such writes), and kept
/// the bytes from before it, what the title's pages held for another
/// texture, for the rest of the run. Which ones depended on timing, so one
/// cutscene shot looked different from run to run. Only those slots are
/// made due, each once a write: some 25 a second while the world streams
/// in; their host copies exist, no room needed.)
pub(crate) fn page_writes() -> u8 {
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| match std::env::var("VICTORY_PAGE_WRITES").as_deref().map(str::trim) {
        Ok("0") => 0,
        Ok("guards") => 1,
        Ok("slots") => 2,
        Ok("all") => 7,
        _ => 3,
    })
}

/// Since the start: writes into physical pages through the runtime's own
/// views that landed at addresses the title has mapped (one for each run
/// of addresses); their bytes; guarded blocks that lay under them (host
/// copies that would otherwise keep the bytes from before); reads to
/// plain addresses that finished.
pub(crate) static WRITTEN: [std::sync::atomic::AtomicU64; 4] = [const { std::sync::atomic::AtomicU64::new(0) }; 4];

/// The bytes of a write the guards do not see are there: title memory
/// `[at, at + len)`. `pages`: DirectStorage wrote them into physical pages
/// through a view of the runtime's own, and this is an address where the
/// title has those pages mapped (no title address faulted, nothing was
/// mapped or unmapped; the shortcut this file's header names). Otherwise a
/// read to a plain address has finished, of which [`writing`] was told
/// before it began.
///
/// Upstream's c75fdba does the same for its trackers (its `Writing` marks
/// the pages once more when dropped; page destinations are reported at
/// each address showing them). Here it is counted always and acted on as
/// [`page_writes`] says. Called without the runtime's page-table lock;
/// takes the guards' lock, then, alone, the lock of the ranges kept for
/// the texture pass.
pub fn title_memory_written(at: usize, len: usize, pages: bool) {
    use std::sync::atomic::Ordering::Relaxed;
    if len == 0 {
        return;
    }
    crate::uheap::page_written(at, len);
    crate::coherence::written_over(at, len);
    let mode = page_writes();
    let end = at.saturating_add(len);
    if pages {
        WRITTEN[0].fetch_add(1, Relaxed);
        WRITTEN[1].fetch_add(len as u64, Relaxed);
        // The title streams: faults open wider ([`widen_blocks`]).
        if widen_blocks() > 0 {
            DISK_WRITTEN_MS.store(since_start_ms(), Relaxed);
        }
    } else {
        WRITTEN[3].fetch_add(1, Relaxed);
        // A finished read to a plain address: the guards only with bit 4;
        // the texture slots under it with bit 2 as well (`writing` made
        // them due when the read began, a frame before its bytes may be
        // there: one uploaded meanwhile took what was there before).
        if mode & 4 == 0 {
            if mode & 2 != 0 {
                crate::descriptor::host::page_written(at, len);
            }
            return;
        }
    }
    {
        let mut g = guards();
        let guarded = g.protected.range(at / BLOCK..end.div_ceil(BLOCK)).count() as u64;
        if pages {
            WRITTEN[2].fetch_add(guarded, Relaxed);
        }
        if mode & 1 != 0 && guarded != 0 {
            open(&mut g, at, end, BY_TOLD);
        }
    }
    // A placed texture there changed with no descriptor written.
    crate::descriptor::host::page_written(at, len);
}

/// The runtime mapped a view of the physical bytes `[offset, offset + len)`
/// of `section` (`crate::inplace::title_memory_view`): the view is
/// writable, so the same bytes guarded at another view must be checked
/// again — and are, the pages noted written, which also has the next check
/// guard the new view (`protect`).
pub(crate) fn views_mapped(section: usize, offset: u64, len: usize) {
    if !view_guard() {
        return;
    }
    let ranges = crate::inplace::views_of_section(section, offset, len);
    if ranges.is_empty() {
        return;
    }
    let mut g = guards();
    for &(at, n) in &ranges {
        // Their pages have another view now: no block of them stays open
        // ([`keep_open`]), a write through it would not be seen at the new one.
        forget_volatile(&mut g, at / BLOCK, at.saturating_add(n).div_ceil(BLOCK));
        if open_here(&mut g, at, at.saturating_add(n), BY_MAPPING) {
            #[cfg(feature = "diagnostics")]
            ALIASES[2].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// The ranges of title memory read in place, and those whose mapping the
/// runtime is changing ([`title_memory_reading_range`],
/// [`title_memory_remapping_range`]).
///
/// The runtime does not change a mapping in one step. To unmap one page of
/// a view it unmaps the whole view and maps what stays again
/// (`xmem.rs` `unmap`), and a view it maps is a reservation until the next
/// call commits it (`map`, then `commit`). For that moment pages the title
/// never gave up are "reserved, never committed", and a thread reading
/// them in place takes an access violation: run 71 in `Layout::detile`,
/// run 74 in the hash before it, both under `Staging::refresh_now`, which
/// had asked whether the texture was all mapped and been told yes. Asking
/// cannot help; not changing the mapping while such a read is under way
/// does.
///
/// That was one lock over all of title memory, shared by the reads and
/// taken alone by each change, which let no read begin while a change
/// waited. While the world streams in, the title's threads change mappings
/// without pause, and the submitting thread's texture refreshes waited
/// 1.0-6.7 s a frame for the lock (the first run on an RTX 5090: 95-99 %
/// of the frame line's "textures", frames of 2-8 s). A read and a change
/// only have to wait for each other where their ranges meet, and they
/// seldom do: the texture read is one the title draws with, the pages
/// changed are others streaming in. Each now holds its own range here.
struct Holders {
    /// `[start, end)` of each read in place under way.
    reading: Vec<(usize, usize)>,
    /// `[start, end)` of each change of mapping under way, or waiting for
    /// the reads that meet it: a read that meets one waits, so reads that
    /// keep beginning do not hold a change off.
    changing: Vec<(usize, usize)>,
    /// Threads waiting for a holder to go (no wake-up when there are none).
    waiting: usize,
}

static HOLDERS: Mutex<Holders> = Mutex::new(Holders {
    reading: Vec::new(),
    changing: Vec::new(),
    waiting: 0,
});
static HOLDER_GONE: std::sync::Condvar = std::sync::Condvar::new();

fn holders() -> std::sync::MutexGuard<'static, Holders> {
    HOLDERS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether a held range meets `[start, end)`; an empty range meets none.
fn meets(held: &[(usize, usize)], start: usize, end: usize) -> bool {
    start < end && held.iter().any(|&(s, e)| s < end && start < e)
}

/// Waits while `busy` holds of the holders.
fn wait_while<'a>(
    mut h: std::sync::MutexGuard<'a, Holders>,
    busy: impl Fn(&Holders) -> bool,
) -> std::sync::MutexGuard<'a, Holders> {
    while busy(&h) {
        h.waiting += 1;
        h = HOLDER_GONE.wait(h).unwrap_or_else(|e| e.into_inner());
        h.waiting -= 1;
    }
    h
}

/// A range held, let go when dropped.
struct Held {
    start: usize,
    end: usize,
    change: bool,
}

impl Drop for Held {
    fn drop(&mut self) {
        let mut h = holders();
        let list = if self.change { &mut h.changing } else { &mut h.reading };
        if let Some(i) = list.iter().position(|&r| r == (self.start, self.end)) {
            list.swap_remove(i);
        }
        if h.waiting > 0 {
            HOLDER_GONE.notify_all();
        }
    }
}

/// `[at, at + len)`, cut at the end of the address space.
fn span(at: usize, len: usize) -> (usize, usize) {
    (at, at.saturating_add(len))
}

/// What the runtime holds while it changes what title memory shows
/// ([`title_memory_remapping_range`]), and when it asked for it.
pub struct Remapping(#[allow(dead_code)] Held, std::time::Instant);

impl Drop for Remapping {
    fn drop(&mut self) {
        // From asking for the range until it is let go (the frame log).
        crate::framelog::add(crate::framelog::T::RemapUs, self.1.elapsed().as_micros() as u64);
    }
}

/// The runtime is about to map, unmap or move physical pages of title
/// memory, anywhere: [`title_memory_remapping_range`] over all of it.
pub fn title_memory_remapping() -> Remapping {
    title_memory_remapping_range(0, usize::MAX)
}

/// The runtime is about to map, unmap or move physical pages of title
/// memory within `[at, at + len)`: until what it returns is dropped, no
/// read in place of that range is under way and none begins
/// ([`title_memory_reading_range`]). Taken around the whole change, the
/// unmapping and the mapping again, over every byte that goes away for a
/// moment (the whole of each view the change takes away and maps again).
pub fn title_memory_remapping_range(at: usize, len: usize) -> Remapping {
    let asked = std::time::Instant::now();
    let (start, end) = span(at, len);
    // Changes are one at a time where they meet (the runtime makes them
    // under its own lock anyway); then this one keeps new reads of its
    // range out, and waits for those under way.
    let mut h = wait_while(holders(), |h| meets(&h.changing, start, end));
    if start < end {
        h.changing.push((start, end));
    }
    let h = wait_while(h, |h| meets(&h.reading, start, end));
    drop(h);
    // The frame log's remaps, and their waits for the range.
    crate::framelog::add(crate::framelog::T::Remaps, 1);
    crate::framelog::add(crate::framelog::T::RemapWaitUs, asked.elapsed().as_micros() as u64);
    Remapping(Held { start, end, change: true }, asked)
}

/// What a read of title memory in place holds ([`title_memory_reading_range`]).
pub struct Reading(#[allow(dead_code)] Held);

/// For reading any of title memory in place:
/// [`title_memory_reading_range`] over all of it.
pub fn title_memory_reading() -> Reading {
    title_memory_reading_range(0, usize::MAX)
}

/// [`title_memory_reading_range_now`] over all of title memory: `None`
/// while the runtime changes any mapping (upstream's try of its one lock,
/// for `crate::inplace::write_own`, whose view lookups span the pages a
/// physical page shows at).
pub fn title_memory_reading_now() -> Option<Reading> {
    title_memory_reading_range_now(0, usize::MAX)
}

/// For reading title memory `[at, at + len)` in place: until what it
/// returns is dropped, the runtime changes no mapping there, so memory
/// found mapped stays mapped. Not to be taken twice by one thread, nor
/// held across anything that maps title memory.
pub fn title_memory_reading_range(at: usize, len: usize) -> Reading {
    let (start, end) = span(at, len);
    let mut h = wait_while(holders(), |h| meets(&h.changing, start, end));
    h.reading.push((start, end));
    drop(h);
    Reading(Held { start, end, change: false })
}

/// [`title_memory_reading_range`] if it can be had at once: `None` while
/// the runtime changes a mapping in the range (or waits to), for a caller
/// that has another way to read and would rather not wait.
pub fn title_memory_reading_range_now(at: usize, len: usize) -> Option<Reading> {
    let (start, end) = span(at, len);
    let mut h = holders();
    if meets(&h.changing, start, end) {
        return None;
    }
    h.reading.push((start, end));
    drop(h);
    Some(Reading(Held { start, end, change: false }))
}

/// Diagnostics: our own work inside the runtime's changes of what title
/// memory shows, since the last frame line (calls, then microseconds):
/// [`title_memory_remapped`] calls and its parts (descriptor slots,
/// textures, views and memory in place, guards), then
/// `crate::inplace::title_memory_view` calls and their time.
///
/// The frame log's "remapping: N calls, X ms" is the whole time the
/// runtime holds a change (wave 3's gameplay: 1 to 3 ms a call, a long
/// frame of 12.9 s with 7,018 calls holding 12.1 s of it, the disk's
/// workers waiting on the runtime's page table meanwhile); these say how
/// much of it is ours.
#[cfg(feature = "diagnostics")]
pub(crate) static REMAP_HOOKS: [std::sync::atomic::AtomicU64; 7] = [const { std::sync::atomic::AtomicU64::new(0) }; 7];

/// [`REMAP_HOOKS`]: the microseconds since `from` to place `i`; the next
/// part starts now.
#[cfg(feature = "diagnostics")]
pub(crate) fn remap_hook_part(i: usize, from: &mut std::time::Instant) {
    let now = std::time::Instant::now();
    REMAP_HOOKS[i].fetch_add((now - *from).as_micros() as u64, std::sync::atomic::Ordering::Relaxed);
    *from = now;
}

/// With the frame line: [`REMAP_HOOKS`] per frame over `frames`.
#[cfg(feature = "diagnostics")]
pub(crate) fn remap_hooks_line(frames: f64) {
    let v: Vec<f64> = REMAP_HOOKS.iter().map(|c| c.swap(0, std::sync::atomic::Ordering::Relaxed) as f64 / frames).collect();
    if v[0] == 0.0 && v[5] == 0.0 {
        return;
    }
    tracing::info!(
        "our work inside the runtime's changes of title memory, per frame (the frame log's \"remapping\" is the whole \
         time they hold): {:.0} remaps told {:.1} ms (descriptor slots {:.1}, textures {:.1}, views and memory in place \
         {:.1}, guards {:.1}); {:.0} views mapped told {:.1} ms (the guards of their other views opened in it)",
        v[0],
        (v[1] + v[2] + v[3] + v[4]) / 1e3,
        v[1] / 1e3,
        v[2] / 1e3,
        v[3] / 1e3,
        v[4] / 1e3,
        v[5],
        v[6] / 1e3
    );
}

/// The runtime mapped or unmapped title memory `[at, at + len)`: what it
/// shows is new and read-write, so it is no longer guarded (nor known
/// unmapped).
pub fn title_memory_remapped(at: usize, len: usize) {
    crate::uheap::remapped(at, len);
    // Texture read-backs whose title memory this was are dropped.
    crate::texback::remapped(at, len);
    #[cfg(feature = "diagnostics")]
    let mut part = std::time::Instant::now();
    // A placed texture there arrives, goes or changes with no descriptor
    // written.
    crate::descriptor::host::remapped(at, len);
    #[cfg(feature = "diagnostics")]
    remap_hook_part(1, &mut part);
    // Textures already uploaded from there must be uploaded again, or the
    // next list that samples them reads their old contents.
    crate::texture::remapped(at, len);
    #[cfg(feature = "diagnostics")]
    remap_hook_part(2, &mut part);
    // The views the runtime told us of there are no longer what it shows.
    crate::inplace::views_gone(at, len);
    // And nothing stays in place over it.
    crate::inplace::title_memory_freeing(at, len);
    #[cfg(feature = "diagnostics")]
    remap_hook_part(3, &mut part);
    crate::coherence::remapped(at, len);
    remapped_guards(at, len);
    #[cfg(feature = "diagnostics")]
    {
        remap_hook_part(4, &mut part);
        REMAP_HOOKS[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// [`title_memory_remapped`]'s part in the guards.
fn remapped_guards(at: usize, len: usize) {
    let mut g = guards();
    let range = at / BLOCK..at.saturating_add(len).div_ceil(BLOCK);
    let absent: Vec<usize> = g.absent.range(range.clone()).copied().collect();
    for block in absent {
        g.absent.remove(&block);
    }
    let blocks: Vec<usize> = g.protected.range(range.clone()).copied().collect();
    for block in blocks {
        g.protected.remove(&block);
        g.clean.remove(&block);
    }
    let aliased: Vec<usize> = g.aliased.range(at / BLOCK..at.saturating_add(len).div_ceil(BLOCK)).copied().collect();
    for block in aliased {
        g.aliased.remove(&block);
    }
    // What the range shows is new: no longer known to be a view.
    let viewed: Vec<usize> = g.viewed.range(at / BLOCK..at.saturating_add(len).div_ceil(BLOCK)).copied().collect();
    for block in viewed {
        g.viewed.remove(&block);
    }
    // Nor parked: the views below go back to the unclean set.
    let parked: Vec<usize> = g.parked.range(range.clone()).copied().collect();
    for block in parked {
        g.parked.remove(&block);
        g.parked_ordinary.remove(&block);
    }
    // Nor open ([`keep_open`]): what it shows is new.
    forget_volatile(&mut g, range.start, range.end);
    // View blocks there are not clean any more.
    let from = g.views.partition_point(|r| r.1 <= range.start);
    let views: Vec<usize> = g.views[from..]
        .iter()
        .take_while(|r| r.0 < range.end)
        .flat_map(|&(a, b)| a.max(range.start)..b.min(range.end))
        .collect();
    count_unclean(BY_MAPPING, views.len());
    g.unclean.extend(views);
}

/// The title committed `[at, at + len)` through the runtime
/// (`XMemVirtualAlloc`): blocks of it found uncommitted before are not
/// known so any more, and the next asking that covers them finds them
/// written. A commit no one tells of is found at the next look at the
/// parked blocks that are no placeholder ([`Guards::parked`]).
pub fn title_memory_committed(at: usize, len: usize) {
    let mut g = guards();
    let range = at / BLOCK..at.saturating_add(len).div_ceil(BLOCK);
    let parked: Vec<usize> = g.parked.range(range.clone()).copied().collect();
    count_unclean(BY_MAPPING, parked.len());
    for block in parked {
        g.parked.remove(&block);
        g.parked_ordinary.remove(&block);
        g.unclean.insert(block);
    }
    let absent: Vec<usize> = g.absent.range(range).copied().collect();
    for block in absent {
        g.absent.remove(&block);
    }
}

/// Registers the handler that takes writes to guarded blocks, once, ahead
/// of the crash reporter's.
fn install() {
    use windows_sys::Win32::Foundation::{EXCEPTION_ACCESS_VIOLATION, NTSTATUS};
    use windows_sys::Win32::System::Diagnostics::Debug::{
        AddVectoredExceptionHandler, EXCEPTION_CONTINUE_EXECUTION, EXCEPTION_CONTINUE_SEARCH,
        EXCEPTION_POINTERS,
    };
    unsafe extern "system" fn on_write(info: *mut EXCEPTION_POINTERS) -> i32 {
        // SAFETY: the system passes the exception being dispatched.
        let r = unsafe { &*(*info).ExceptionRecord };
        const WRITE: usize = 1;
        if r.ExceptionCode != EXCEPTION_ACCESS_VIOLATION as NTSTATUS
            || r.NumberParameters < 2
            || r.ExceptionInformation[0] != WRITE
        {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        let at = r.ExceptionInformation[1];
        #[cfg(feature = "diagnostics")]
        let started = std::time::Instant::now();
        // Held across both checks: another thread may have opened the block
        // since this one faulted, and a copy may have guarded it again since
        // (then it is ours again, not read-only for some other reason).
        let mut g = guards();
        #[cfg(feature = "diagnostics")]
        let (locked, aliased) = (started.elapsed(), g.aliased.contains(&(at / BLOCK)));
        if open(&mut g, at, at + 1, BY_FAULT) {
            widen(&mut g, at);
            #[cfg(feature = "diagnostics")]
            fault_taken(&mut g, at / BLOCK, aliased, started, locked);
            #[cfg(feature = "diagnostics")]
            crate::copystat::add(crate::copystat::C::GuardFaults, 1);
            crate::framelog::add(crate::framelog::T::GuardFaults, 1);
            return EXCEPTION_CONTINUE_EXECUTION;
        }
        if writable(at) || reopen_unknown(at) {
            EXCEPTION_CONTINUE_EXECUTION
        } else {
            EXCEPTION_CONTINUE_SEARCH
        }
    }
    static ONCE: std::sync::Once = std::sync::Once::new();
    // SAFETY: the handler only touches our guard state and protections.
    ONCE.call_once(|| unsafe {
        AddVectoredExceptionHandler(1, Some(on_write));
    });
}

/// With [`widen_blocks`], in a fault the handler took at `at`: while
/// DirectStorage wrote in the last half second, the other guarded blocks of
/// the aligned window around it are opened too (noted written, as if each
/// had faulted).
fn widen(g: &mut Guards, at: usize) {
    use std::sync::atomic::Ordering::Relaxed;
    let n = widen_blocks();
    if n == 0 {
        return;
    }
    let last = DISK_WRITTEN_MS.load(Relaxed);
    if last == 0 || since_start_ms().saturating_sub(last) > 500 {
        return;
    }
    let first = at / BLOCK / n * n;
    #[cfg(feature = "diagnostics")]
    track::add(track::WIDENED, g.protected.range(first..first + n).count() as u64);
    open(g, first * BLOCK, (first + n) * BLOCK, BY_FAULT);
}

/// Diagnostics: a fault the handler took on `block` (`aliased`: guarded
/// with other views of its page), the handler begun at `started`, the
/// guards' lock had after `locked`.
#[cfg(feature = "diagnostics")]
fn fault_taken(g: &mut Guards, block: usize, aliased: bool, started: std::time::Instant, locked: std::time::Duration) {
    track::add(track::FAULTS, 1);
    g.seen.faulted.insert(block);
    if aliased {
        track::add(track::FAULTS_ALIASED, 1);
        // The page, by its lowest view: faults on its views count once.
        let lowest = crate::inplace::pages_of(block * BLOCK).into_iter().min().unwrap_or(block * BLOCK);
        g.seen.pages.insert(lowest / BLOCK);
    }
    if track::on_submitter() {
        track::add(track::FAULTS_SUBMITTER, 1);
    }
    track::add(track::HANDLER_LOCK_NS, locked.as_nanos() as u64);
    track::ns(track::HANDLER_NS, started);
}

/// A write faulted at `at`, in a view of physical pages the runtime told
/// us of, read-only, and no guard holds its block: only we make title views
/// read-only, so it is given back (logged once) rather than ending the run.
/// Belobug's fork met such blocks (its 1c7231d9: a view made read-only as
/// another view of a guarded page before the runtime's notice of it, which
/// then forgot the guard); the order here (the notice of a mapping, then
/// its view) leaves none known, so this only stands between such a slip
/// and a crash. Not with `VICTORY_VIEW_GUARD=0`, which guards no other view.
fn reopen_unknown(at: usize) -> bool {
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_READONLY, PAGE_READWRITE, VirtualProtect, VirtualQuery,
    };
    let block = at - at % BLOCK;
    if !view_guard() || crate::inplace::pages_of(block).is_empty() {
        return false;
    }
    // SAFETY: plain data filled in by the query.
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a valid out pointer of the right size.
    let n = unsafe { VirtualQuery(at as *const _, &mut info, size_of_val(&info)) };
    if n == 0 || info.State != MEM_COMMIT || info.Protect != PAGE_READONLY {
        return false;
    }
    let mut old = 0;
    // SAFETY: a block of a title view; only its protection changes.
    let opened = unsafe { VirtualProtect(block as *const _, BLOCK, PAGE_READWRITE, &mut old) } != 0;
    if opened {
        crate::coherence::protection_set(block, block + BLOCK, PAGE_READWRITE);
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| tracing::warn!(at = format_args!("{at:#x}"), "a read-only view block no guard knew was opened"));
    }
    opened
}

/// Whether `at` is committed read-write memory now.
fn writable(at: usize) -> bool {
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_READWRITE, VirtualQuery,
    };
    // SAFETY: plain data filled in by the query.
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a valid out pointer of the right size.
    let n = unsafe { VirtualQuery(at as *const _, &mut info, size_of_val(&info)) };
    n != 0 && info.State == MEM_COMMIT && info.Protect == PAGE_READWRITE
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc, VirtualFree,
    };
    const MEM_WRITE_WATCH: u32 = 0x20_0000;

    #[test]
    fn written_pages_are_reported_to_every_asker() {
        // SAFETY: a fresh write-watched allocation, freed at the end.
        let p = unsafe {
            VirtualAlloc(
                std::ptr::null(),
                4 * PAGE,
                MEM_RESERVE | MEM_COMMIT | MEM_WRITE_WATCH,
                PAGE_READWRITE,
            )
        } as usize;
        assert_ne!(p, 0);
        let (start, end) = (p, p + 4 * PAGE);
        let (_, before) = writes(start, end, false).expect("watched");
        assert!(before.is_empty());
        // SAFETY: committed pages of ours.
        unsafe { ((p + PAGE + 8) as *mut u8).write(1) };
        let (now, after) = writes(start, end, false).expect("watched");
        assert_eq!(after, [(p / PAGE + 1, now)]);
        // Another asker after the kernel's bit was cleared still sees it.
        let (later, again) = writes(p + PAGE, p + 2 * PAGE, false).expect("watched");
        assert!(later > now);
        assert_eq!(again, [(p / PAGE + 1, now)]);
        let mut plain = vec![0u8; PAGE];
        assert!(
            writes(
                plain.as_mut_ptr() as usize,
                plain.as_ptr() as usize + PAGE,
                true
            )
            .is_none()
        );
        // SAFETY: our allocation.
        unsafe { VirtualFree(p as *mut _, 0, MEM_RELEASE) };
    }

    /// A kernel read into a guarded view block fails; opened first, it
    /// lands and the block counts as written.
    #[test]
    fn kernel_reads_into_guarded_blocks_after_opening() {
        use std::io::{Read, Seek, Write};
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, FILE_MAP_WRITE, MapViewOfFile, UnmapViewOfFile,
        };
        // SAFETY: a pagefile section of two blocks, mapped read-write.
        let view = unsafe {
            let section = CreateFileMappingW(
                -1isize as _,
                std::ptr::null(),
                PAGE_READWRITE,
                0,
                (2 * BLOCK) as u32,
                std::ptr::null(),
            );
            assert!(!section.is_null());
            MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, 2 * BLOCK).Value as usize
        };
        assert_ne!(view, 0);
        assert!(guard(view, view + 2 * BLOCK).is_some()); // now read-only
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"from the disk").unwrap();
        // `ReadFile` inside: the kernel writes the view.
        let read = |file: &mut std::fs::File| {
            file.rewind().unwrap();
            // SAFETY: 13 bytes of our view, nothing else refers to them.
            let into = unsafe { std::slice::from_raw_parts_mut((view + 100) as *mut u8, 13) };
            file.read(into).is_ok_and(|n| n == 13)
        };
        assert!(
            !read(&mut file),
            "the kernel does not fault into our handler"
        );
        drop(writing(view + 100, 13));
        assert!(read(&mut file));
        // SAFETY: the bytes just read.
        let landed = unsafe { std::slice::from_raw_parts((view + 100) as *const u8, 13) };
        assert_eq!(landed, b"from the disk");
        assert_eq!(
            guard(view, view + 2 * BLOCK),
            Some((view / PAGE..view / PAGE + 1).collect())
        );
        // Forgotten before the view goes: once it is unmapped, a test on
        // another thread may map memory at the same address and find these
        // blocks still noted guarded and clean.
        title_memory_remapped(view, 2 * BLOCK);
        // SAFETY: our view.
        unsafe {
            UnmapViewOfFile(
                windows_sys::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS {
                    Value: view as *mut _,
                },
            )
        };
    }

    /// A block a write through the kernel is landing in ([`writing`]) is
    /// reported written at every check meanwhile and is not guarded again
    /// under the write; once the write is done, it is reported written once
    /// more, then guarded again.
    #[test]
    fn a_landing_write_keeps_its_blocks_open() {
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, FILE_MAP_WRITE, MapViewOfFile, UnmapViewOfFile,
        };
        // SAFETY: a pagefile section of two blocks, mapped read-write.
        let view = unsafe {
            let section = CreateFileMappingW(
                -1isize as _,
                std::ptr::null(),
                PAGE_READWRITE,
                0,
                (2 * BLOCK) as u32,
                std::ptr::null(),
            );
            assert!(!section.is_null());
            MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, 2 * BLOCK).Value as usize
        };
        assert_ne!(view, 0);
        let block = view / BLOCK;
        let first_page: Vec<usize> = (view / PAGE..view / PAGE + 1).collect();
        assert!(guard(view, view + 2 * BLOCK).is_some()); // now read-only
        assert!(guards().protected.contains(&block));
        let landing = writing(view + 100, 13);
        for _ in 0..2 {
            // A check while the write lands (another thread's, say).
            assert_eq!(guard(view, view + 2 * BLOCK), Some(first_page.clone()));
            assert!(!guards().protected.contains(&block), "not guarded under the write");
        }
        drop(landing);
        assert_eq!(guard(view, view + 2 * BLOCK), Some(first_page.clone()));
        assert!(guards().protected.contains(&block), "guarded again once it landed");
        assert_eq!(guard(view, view + 2 * BLOCK), Some(Vec::new()));
        title_memory_remapped(view, 2 * BLOCK);
        // SAFETY: our view.
        unsafe {
            UnmapViewOfFile(
                windows_sys::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS {
                    Value: view as *mut _,
                },
            )
        };
    }

    #[test]
    fn writes_in_partly_committed_view_blocks_are_reported() {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
            SEC_RESERVE, UnmapViewOfFile,
        };

        const SIZE: usize = 3 * BLOCK;
        struct View(MEMORY_MAPPED_VIEW_ADDRESS);
        impl Drop for View {
            fn drop(&mut self) {
                title_memory_remapped(self.0.Value as usize, SIZE);
                // SAFETY: our view is still mapped.
                unsafe { UnmapViewOfFile(self.0) };
            }
        }
        // SAFETY: a new reserved pagefile section. The view holds its own
        // reference after we close the handle, including on assertion failure.
        let view = unsafe {
            let section = CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                std::ptr::null(),
                PAGE_READWRITE | SEC_RESERVE,
                0,
                SIZE as u32,
                std::ptr::null(),
            );
            assert!(!section.is_null());
            let view = MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, SIZE);
            CloseHandle(section);
            assert!(!view.Value.is_null());
            View(view)
        };
        let start = view.0.Value as usize;
        let commit = |at: usize, len| {
            // SAFETY: these pages belong to our reserved view.
            let committed = unsafe { VirtualAlloc(at as _, len, MEM_COMMIT, PAGE_READWRITE) };
            assert_eq!(committed as usize, at);
        };
        let early = start + BLOCK + 3 * KERNEL_PAGE;
        let late = start + 2 * BLOCK + 3 * KERNEL_PAGE;
        commit(start, BLOCK);
        commit(early, KERNEL_PAGE);
        let refresh = || guard(start, start + SIZE).unwrap();
        assert_eq!(refresh(), [start / PAGE, early / PAGE, late / PAGE]);
        // The last block is still entirely reserved.
        assert!(!refresh().contains(&(late / PAGE)));

        commit(late, KERNEL_PAGE); // Ordinary commit, without a runtime mapping notification.
        for at in [early, late] {
            assert!(writable(at));
            // SAFETY: each address is inside its committed read-write page.
            unsafe { (at as *mut u8).write_volatile(1) };
        }
        // With the view set: found at the next look at the blocks parked as
        // uncommitted that are no placeholder.
        SUBMISSIONS.fetch_add(park_epoch(), std::sync::atomic::Ordering::Relaxed);
        assert_eq!(refresh(), [early / PAGE, late / PAGE]);
    }

    /// Blocks found uncommitted are parked: not asked about again until the
    /// title commits them through the runtime (found at once) or the next
    /// look at the parked blocks that are no placeholder (a commit no one
    /// tells of).
    #[test]
    fn parked_blocks_come_back_when_committed() {
        if !view_set() {
            return;
        }
        // SAFETY: a fresh reservation of two blocks, nothing committed.
        let p = unsafe { VirtualAlloc(std::ptr::null(), 2 * BLOCK, MEM_RESERVE, PAGE_READWRITE) } as usize;
        assert_ne!(p, 0);
        let pages = |block: usize| (p + block * BLOCK) / PAGE..(p + (block + 1) * BLOCK) / PAGE;
        let all: Vec<usize> = (0..2).flat_map(pages).collect();
        // Nothing there: a view, its blocks reported once, then parked.
        assert_eq!(guard(p, p + 2 * BLOCK), Some(all));
        assert_eq!(guard(p, p + 2 * BLOCK), Some(Vec::new()));
        {
            // No placeholder: looked at again each epoch.
            let g = guards();
            assert!(g.parked.contains(&(p / BLOCK)) && g.parked_ordinary.contains(&(p / BLOCK)));
        }
        // SAFETY: the first block of our reservation.
        assert_eq!(unsafe { VirtualAlloc(p as _, BLOCK, MEM_COMMIT, PAGE_READWRITE) } as usize, p);
        title_memory_committed(p, BLOCK);
        assert_eq!(guard(p, p + 2 * BLOCK), Some(pages(0).collect()));
        assert_eq!(guard(p, p + 2 * BLOCK), Some(Vec::new()));
        // SAFETY: the second block of our reservation.
        assert_eq!(unsafe { VirtualAlloc((p + BLOCK) as _, BLOCK, MEM_COMMIT, PAGE_READWRITE) } as usize, p + BLOCK);
        SUBMISSIONS.fetch_add(park_epoch(), std::sync::atomic::Ordering::Relaxed);
        assert_eq!(guard(p, p + 2 * BLOCK), Some(pages(1).collect()));
        assert_eq!(guard(p, p + 2 * BLOCK), Some(Vec::new()));
        title_memory_remapped(p, 2 * BLOCK);
        // SAFETY: our reservation, no longer known to the guards.
        unsafe { VirtualFree(p as _, 0, MEM_RELEASE) };
    }

    /// A range of view memory whose first block is not mapped is a view
    /// all the same: its blocks are reported once, then the ones written
    /// or mapped since.
    #[test]
    fn a_range_that_begins_in_an_unmapped_block_is_guarded() {
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, MEM_PRESERVE_PLACEHOLDER, MEM_REPLACE_PLACEHOLDER,
            MEM_RESERVE_PLACEHOLDER, MapViewOfFile3, PAGE_NOACCESS, UnmapViewOfFile2,
            VirtualAlloc2,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        // SAFETY: a fresh placeholder of three blocks and a pagefile
        // section of three; the middle block is mapped first.
        let (p, section) = unsafe {
            let p = VirtualAlloc2(
                GetCurrentProcess(),
                std::ptr::null(),
                3 * BLOCK,
                MEM_RESERVE | MEM_RESERVE_PLACEHOLDER,
                PAGE_NOACCESS,
                std::ptr::null_mut(),
                0,
            ) as usize;
            assert_ne!(p, 0);
            let section = CreateFileMappingW(
                -1isize as _,
                std::ptr::null(),
                PAGE_READWRITE,
                0,
                (3 * BLOCK) as u32,
                std::ptr::null(),
            );
            assert!(!section.is_null());
            (p, section)
        };
        let map = |block: usize| {
            // SAFETY: splits the placeholder at the block and maps the
            // section's block there.
            let view = unsafe {
                VirtualFree(
                    (p + block * BLOCK) as *mut _,
                    BLOCK,
                    MEM_RELEASE | MEM_PRESERVE_PLACEHOLDER,
                );
                MapViewOfFile3(
                    section,
                    GetCurrentProcess(),
                    (p + block * BLOCK) as *const _,
                    (block * BLOCK) as u64,
                    BLOCK,
                    MEM_REPLACE_PLACEHOLDER,
                    PAGE_READWRITE,
                    std::ptr::null_mut(),
                    0,
                )
            };
            assert_eq!(view.Value as usize, p + block * BLOCK);
            view
        };
        let pages = |block: usize| (p + block * BLOCK) / PAGE..(p + (block + 1) * BLOCK) / PAGE;
        let all: Vec<usize> = (0..3).flat_map(pages).collect();
        // Nothing mapped at all: a view with every block absent.
        assert!(is_view(p, p + 3 * BLOCK));
        let middle = map(1);
        title_memory_remapped(p + BLOCK, BLOCK);
        // The range begins in the placeholder, its middle block is a view.
        assert!(is_view(p, p + 3 * BLOCK));
        assert_eq!(guard(p, p + 3 * BLOCK), Some(all));
        assert_eq!(guard(p, p + 3 * BLOCK), Some(Vec::new()), "nothing written since");
        // SAFETY: inside the mapped middle block.
        unsafe { ((p + BLOCK + 8) as *mut u8).write_volatile(1) };
        assert_eq!(guard(p, p + 3 * BLOCK), Some(pages(1).collect()));
        let first = map(0);
        title_memory_remapped(p, BLOCK);
        assert_eq!(guard(p, p + 3 * BLOCK), Some(pages(0).collect()));
        assert_eq!(guard(p, p + 3 * BLOCK), Some(Vec::new()));
        // Memory of ours that is no view stays none.
        let mut local = [0u8; 16];
        let at = local.as_mut_ptr() as usize;
        assert!(!is_view(at, at + local.len()));
        // Forgotten before the memory goes (see
        // `kernel_reads_into_guarded_blocks_after_opening`).
        title_memory_remapped(p, 3 * BLOCK);
        // SAFETY: our views and placeholder.
        unsafe {
            UnmapViewOfFile2(GetCurrentProcess(), first, MEM_PRESERVE_PLACEHOLDER);
            UnmapViewOfFile2(GetCurrentProcess(), middle, MEM_PRESERVE_PLACEHOLDER);
            for block in 0..3 {
                VirtualFree((p + block * BLOCK) as *mut _, 0, MEM_RELEASE);
            }
            windows_sys::Win32::Foundation::CloseHandle(section);
        }
    }

    /// Blocks of a placeholder between views are reported once, then not
    /// again (they cannot be guarded) until the runtime maps them.
    #[test]
    fn unmapped_blocks_are_reported_once_until_mapped() {
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, MEM_PRESERVE_PLACEHOLDER, MEM_REPLACE_PLACEHOLDER,
            MEM_RESERVE_PLACEHOLDER, MapViewOfFile3, PAGE_NOACCESS, UnmapViewOfFile2,
            VirtualAlloc2,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        // SAFETY: a fresh placeholder of three blocks and a pagefile
        // section of two; block 0 shows the section's first block, as the
        // runtime maps physical pages.
        let (p, section) = unsafe {
            let p = VirtualAlloc2(
                GetCurrentProcess(),
                std::ptr::null(),
                3 * BLOCK,
                MEM_RESERVE | MEM_RESERVE_PLACEHOLDER,
                PAGE_NOACCESS,
                std::ptr::null_mut(),
                0,
            ) as usize;
            assert_ne!(p, 0);
            let section = CreateFileMappingW(
                -1isize as _,
                std::ptr::null(),
                PAGE_READWRITE,
                0,
                (2 * BLOCK) as u32,
                std::ptr::null(),
            );
            assert!(!section.is_null());
            (p, section)
        };
        let map = |block: usize| {
            // SAFETY: splits the placeholder at the block and maps the
            // section's block there.
            let view = unsafe {
                VirtualFree(
                    (p + block * BLOCK) as *mut _,
                    BLOCK,
                    MEM_RELEASE | MEM_PRESERVE_PLACEHOLDER,
                );
                MapViewOfFile3(
                    section,
                    GetCurrentProcess(),
                    (p + block * BLOCK) as *const _,
                    (block * BLOCK) as u64,
                    BLOCK,
                    MEM_REPLACE_PLACEHOLDER,
                    PAGE_READWRITE,
                    std::ptr::null_mut(),
                    0,
                )
            };
            assert_eq!(view.Value as usize, p + block * BLOCK);
            view
        };
        let first = map(0);
        let pages = |block: usize| (p + block * BLOCK) / PAGE..(p + (block + 1) * BLOCK) / PAGE;
        let all: Vec<usize> = (0..3).flat_map(pages).collect();
        assert_eq!(guard(p, p + 3 * BLOCK), Some(all));
        assert_eq!(
            guard(p, p + 3 * BLOCK),
            Some(Vec::new()),
            "nothing written since"
        );
        if view_set() {
            // Placeholders: parked until the runtime maps them, never asked
            // about again meanwhile.
            let g = guards();
            for block in [1, 2].map(|k| p / BLOCK + k) {
                assert!(g.parked.contains(&block) && !g.parked_ordinary.contains(&block));
            }
        }
        let second = map(1);
        title_memory_remapped(p + BLOCK, BLOCK);
        assert_eq!(guard(p, p + 3 * BLOCK), Some(pages(1).collect()));
        assert_eq!(guard(p, p + 3 * BLOCK), Some(Vec::new()));
        // Forgotten before the memory goes (see
        // `kernel_reads_into_guarded_blocks_after_opening`).
        title_memory_remapped(p, 3 * BLOCK);
        // SAFETY: our views and placeholder.
        unsafe {
            UnmapViewOfFile2(GetCurrentProcess(), first, MEM_PRESERVE_PLACEHOLDER);
            UnmapViewOfFile2(GetCurrentProcess(), second, MEM_PRESERVE_PLACEHOLDER);
            for block in 0..3 {
                VirtualFree((p + block * BLOCK) as *mut _, 0, MEM_RELEASE);
            }
            windows_sys::Win32::Foundation::CloseHandle(section);
        }
    }

    /// A pagefile section mapped twice: two views of the same physical
    /// pages, the second left for the test; the section handle, the two
    /// addresses.
    fn two_views(bytes: usize) -> (usize, usize, usize) {
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, FILE_MAP_WRITE, MapViewOfFile,
        };
        // SAFETY: a fresh pagefile section, mapped twice; the views keep
        // the section alive and the handle is closed by the caller.
        unsafe {
            let section = CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                std::ptr::null(),
                PAGE_READWRITE,
                0,
                bytes as u32,
                std::ptr::null(),
            );
            assert!(!section.is_null());
            let one = MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, bytes).Value as usize;
            let two = MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, bytes).Value as usize;
            assert_ne!(one, 0);
            assert_ne!(two, 0);
            crate::inplace::title_memory_view(one, bytes, section as usize, 0);
            crate::inplace::title_memory_view(two, bytes, section as usize, 0);
            (section as usize, one, two)
        }
    }

    /// The section of [`two_views`] is closed and its views unmapped; the
    /// guard is told they are gone first.
    fn drop_views(section: usize, views: &[usize], bytes: usize) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Memory::{MEMORY_MAPPED_VIEW_ADDRESS, UnmapViewOfFile};
        for &view in views {
            title_memory_remapped(view, bytes);
            // SAFETY: a view of the section mapped by `two_views`.
            unsafe {
                UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                    Value: view as *mut _,
                })
            };
        }
        // SAFETY: the section's last reference.
        unsafe { CloseHandle(section as *mut std::ffi::c_void) };
    }

    /// The title wrote the pages of a guarded texture through another view
    /// of them: the fault at that view is the guard's to see, and the
    /// guarded view's next check reports the pages written.
    #[test]
    fn a_write_through_another_view_is_reported_at_the_guarded_one() {
        const SIZE: usize = 2 * BLOCK;
        let (section, guarded, other) = two_views(SIZE);
        let pages = |block: usize| (guarded + block * BLOCK) / PAGE;
        // The first check takes it in (and every view of its pages).
        assert_eq!(guard(guarded, guarded + SIZE), Some(vec![pages(0), pages(1)]));
        assert_eq!(guard(guarded, guarded + SIZE), Some(Vec::new()), "nothing written yet");
        // Writing through the other view faults into the handler, which
        // opens that view and the guarded one: the next check sees it.
        // SAFETY: the second view, read-write once the handler has opened it.
        unsafe { ((other + BLOCK + 8) as *mut u8).write(1) };
        assert_eq!(guard(guarded, guarded + SIZE), Some(vec![pages(1)]));
        assert_eq!(guard(guarded, guarded + SIZE), Some(Vec::new()));
        drop_views(section, &[guarded, other], SIZE);
    }

    /// A view of guarded pages is mapped anew: it is writable, so the
    /// guarded view checks its pages again (and guards the new view).
    #[test]
    fn a_view_mapped_beside_a_guarded_one_makes_it_check_again() {
        const SIZE: usize = 2 * BLOCK;
        let (section, guarded, other) = two_views(SIZE);
        let pages = |block: usize| (guarded + block * BLOCK) / PAGE;
        assert_eq!(guard(guarded, guarded + SIZE), Some(vec![pages(0), pages(1)]));
        assert_eq!(guard(guarded, guarded + SIZE), Some(Vec::new()), "nothing written yet");
        title_memory_remapped(other, SIZE);
        crate::inplace::title_memory_view(other, SIZE, section, 0);
        assert_eq!(guard(guarded, guarded + SIZE), Some(vec![pages(0), pages(1)]));
        assert_eq!(guard(guarded, guarded + SIZE), Some(Vec::new()));
        drop_views(section, &[guarded, other], SIZE);
    }

    /// A write of the runtime's own, told at one view of the pages
    /// (`writing`): the other views' next checks report the pages written.
    #[test]
    fn a_write_told_at_one_view_is_reported_at_the_others() {
        const SIZE: usize = 2 * BLOCK;
        let (section, guarded, other) = two_views(SIZE);
        let pages = |block: usize| (guarded + block * BLOCK) / PAGE;
        assert_eq!(guard(guarded, guarded + SIZE), Some(vec![pages(0), pages(1)]));
        assert_eq!(guard(guarded, guarded + SIZE), Some(Vec::new()), "nothing written yet");
        drop(writing(other + BLOCK, BLOCK));
        assert_eq!(guard(guarded, guarded + SIZE), Some(vec![pages(1)]));
        assert_eq!(guard(guarded, guarded + SIZE), Some(Vec::new()));
        // Half a page and the next: both blocks, not just the first's.
        drop(writing(other + BLOCK / 2, BLOCK));
        assert_eq!(guard(guarded, guarded + SIZE), Some(vec![pages(0), pages(1)]));
        drop_views(section, &[guarded, other], SIZE);
    }

    /// Both views of the pages are checked, each for a copy of its own
    /// (the title shows one physical page at several addresses, and each
    /// address has its host copy): a write through either is reported at
    /// both, whichever is checked first. Guarding the pages for the first
    /// view's check must not note the second view clean, or its copy would
    /// keep the bytes from before the write.
    #[test]
    fn a_write_is_reported_at_every_checked_view() {
        const SIZE: usize = 2 * BLOCK;
        let (section, one, two) = two_views(SIZE);
        let pages = |view: usize, block: usize| (view + block * BLOCK) / PAGE;
        assert_eq!(guard(one, one + SIZE), Some(vec![pages(one, 0), pages(one, 1)]));
        assert_eq!(guard(two, two + SIZE), Some(vec![pages(two, 0), pages(two, 1)]), "the second view's first check");
        assert_eq!(guard(one, one + SIZE), Some(Vec::new()), "nothing written yet");
        assert_eq!(guard(two, two + SIZE), Some(Vec::new()), "nothing written yet");
        // SAFETY: the second view, read-write once the handler has opened it.
        unsafe { ((two + BLOCK + 8) as *mut u8).write(1) };
        assert_eq!(guard(one, one + SIZE), Some(vec![pages(one, 1)]));
        assert_eq!(guard(two, two + SIZE), Some(vec![pages(two, 1)]), "written through this view, checked second");
        // SAFETY: the first view, as above.
        unsafe { ((one + 8) as *mut u8).write(1) };
        assert_eq!(guard(two, two + SIZE), Some(vec![pages(two, 0)]));
        assert_eq!(guard(one, one + SIZE), Some(vec![pages(one, 0)]), "written through this view, checked second");
        assert_eq!(guard(one, one + SIZE), Some(Vec::new()));
        assert_eq!(guard(two, two + SIZE), Some(Vec::new()));
        drop_views(section, &[one, two], SIZE);
    }

    /// A write through a view that no copy checks: the checked view is
    /// noted written and left read-only, its check reports the pages and
    /// guards the written view again, so a second write through it is seen
    /// as well.
    #[test]
    fn a_view_no_copy_checks_is_guarded_again_by_the_checked_one() {
        let (section, checked, other) = two_views(BLOCK);
        assert_eq!(guard(checked, checked + BLOCK), Some(vec![checked / PAGE]));
        assert!(!writable(other), "guarded with the checked view");
        // SAFETY: the other view, read-write once the handler has opened it.
        unsafe { (other as *mut u8).write(1) };
        assert!(writable(other), "opened by the handler");
        assert!(!writable(checked), "noted written, left read-only");
        assert_eq!(guard(checked, checked + BLOCK), Some(vec![checked / PAGE]));
        assert!(!writable(other), "guarded again by the check");
        // SAFETY: as above.
        unsafe { (other as *mut u8).write(2) };
        assert_eq!(guard(checked, checked + BLOCK), Some(vec![checked / PAGE]));
        assert_eq!(guard(checked, checked + BLOCK), Some(Vec::new()));
        // SAFETY: the checked view, opened by the handler.
        unsafe { (checked as *mut u8).write(3) };
        assert_eq!(guard(checked, checked + BLOCK), Some(vec![checked / PAGE]));
        drop_views(section, &[checked, other], BLOCK);
    }

    /// A check of view memory as [`guard`] makes it with the view set, with
    /// the write tracking's switches given (`volatile`, `joined`).
    fn check(start: usize, end: usize, volatile: u32, joined: bool) -> Vec<usize> {
        assert!(is_view(start, end));
        install();
        let mut g = guards();
        guard_unclean_as(&mut g, start / BLOCK, end.div_ceil(BLOCK), volatile, joined)
    }

    /// With the re-arms of a check joined (`VICTORY_TRACK_REARM=consume`):
    /// the same blocks end guarded as block by block. A write through a
    /// view no copy checks is reported at the checked view, and the check
    /// guards that view again, so a second write through it is seen too.
    #[test]
    fn joined_rearms_guard_the_other_views_again() {
        if !view_set() {
            return;
        }
        const SIZE: usize = 2 * BLOCK;
        let (section, checked, other) = two_views(SIZE);
        let pages = |block: usize| (checked + block * BLOCK) / PAGE;
        assert_eq!(check(checked, checked + SIZE, 0, true), vec![pages(0), pages(1)]);
        assert!(!writable(other) && !writable(other + BLOCK), "both blocks of the other view guarded with the checked one");
        assert_eq!(check(checked, checked + SIZE, 0, true), Vec::<usize>::new());
        // SAFETY: the other view, read-write once the handler has opened it.
        unsafe { ((other + BLOCK + 8) as *mut u8).write(1) };
        assert!(writable(other + BLOCK) && !writable(other), "the handler opened the written block only");
        assert_eq!(check(checked, checked + SIZE, 0, true), vec![pages(1)]);
        assert!(!writable(other + BLOCK), "guarded again by the joined re-arm");
        // SAFETY: as above.
        unsafe { ((other + BLOCK + 16) as *mut u8).write(2) };
        assert_eq!(check(checked, checked + SIZE, 0, true), vec![pages(1)]);
        assert_eq!(check(checked, checked + SIZE, 0, true), Vec::<usize>::new());
        drop_views(section, &[checked, other], SIZE);
    }

    /// A block written at every check that covers it is left open
    /// (`VICTORY_TRACK_VOLATILE`): reported written at each check with no
    /// fault and no guard, until the probe guards it again; a check that
    /// finds it clean then ends its streak, and it is tracked as before.
    #[test]
    fn a_block_written_at_every_check_is_left_open_then_probed() {
        use windows_sys::Win32::System::Memory::{CreateFileMappingW, FILE_MAP_WRITE, MapViewOfFile};
        if !view_set() {
            return;
        }
        const N: u32 = 2;
        // SAFETY: a pagefile section of one block, mapped once (no other
        // view: a block with other views is never left open).
        let (section, view) = unsafe {
            let section = CreateFileMappingW(-1isize as _, std::ptr::null(), PAGE_READWRITE, 0, BLOCK as u32, std::ptr::null());
            assert!(!section.is_null());
            (section as usize, MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, BLOCK).Value as usize)
        };
        assert_ne!(view, 0);
        let page = vec![view / PAGE];
        // SAFETY: our view; written through the guard's handler while guarded.
        let write = |n: u8| unsafe { ((view + 8) as *mut u8).write_volatile(n) };
        // First seen: taken, a streak of one, guarded.
        assert_eq!(check(view, view + BLOCK, N, false), page);
        assert!(!writable(view));
        write(1);
        // Written again by the second check: left open.
        assert_eq!(check(view, view + BLOCK, N, false), page);
        assert!(writable(view), "left open");
        for k in 1..VOLATILE_PROBE {
            write(k as u8);
            assert_eq!(check(view, view + BLOCK, N, false), page, "reported written at every check while open");
            assert!(writable(view));
        }
        // The probe (the check after as many while open): reported, and
        // guarded again once.
        assert_eq!(check(view, view + BLOCK, N, false), page);
        assert!(!writable(view), "guarded by the probe");
        // Not written since: clean, and the streak ends.
        assert_eq!(check(view, view + BLOCK, N, false), Vec::<usize>::new());
        assert!(!guards().streak.contains_key(&(view / BLOCK)));
        write(0x55);
        // Tracked as before: one written check is a streak of one, guarded.
        assert_eq!(check(view, view + BLOCK, N, false), page);
        assert!(!writable(view));
        drop_views(section, &[view], BLOCK);
    }

    /// A page shown in more than `MANY_VIEWS` views is guarded at each by
    /// itself: the others stay writable, and nothing looks through them all.
    #[test]
    fn a_page_of_many_views_is_guarded_one_view_at_a_time() {
        let (section, one, two) = two_views(BLOCK);
        let filler = one + (1 << 45);
        for n in 0..MANY_VIEWS {
            crate::inplace::title_memory_view(filler + n * BLOCK, BLOCK, section, 0);
        }
        assert_eq!(guard(one, one + BLOCK), Some(vec![one / PAGE]));
        assert!(writable(two), "another view of it is left alone");
        // SAFETY: the guarded view, opened by the handler.
        unsafe { ((one + 8) as *mut u8).write(1) };
        assert_eq!(guard(one, one + BLOCK), Some(vec![one / PAGE]));
        title_memory_remapped(filler, MANY_VIEWS * BLOCK);
        drop_views(section, &[one, two], BLOCK);
    }

    /// A write or mapping that runs past the end of the address space (a
    /// title's `fread` with a huge count) covers up to the end; the sum used
    /// to overflow. Up there no test has guarded blocks.
    #[test]
    fn ranges_past_the_address_space_end_do_not_overflow() {
        let top = usize::MAX - 16;
        drop(writing(top, PAGE));
        title_memory_remapped(top, usize::MAX);
    }
}

#[cfg(test)]
mod mapping_tests {
    use super::{
        title_memory_reading, title_memory_reading_range, title_memory_reading_range_now,
        title_memory_remapping, title_memory_remapping_range,
    };
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::Duration;

    /// The holders are the process's: these tests take turns.
    static TURN: Mutex<()> = Mutex::new(());

    const MIB: usize = 1 << 20;

    /// Runs `f` on another thread; true if it returned within five seconds.
    fn done_soon(f: impl FnOnce() + Send + 'static) -> bool {
        let (done, wait) = mpsc::channel();
        std::thread::spawn(move || {
            f();
            let _ = done.send(());
        });
        wait.recv_timeout(Duration::from_secs(5)).is_ok()
    }

    /// A change of mapping waits for a read in place that is under way, and
    /// a read in place waits for a change of mapping.
    #[test]
    fn reads_in_place_and_remappings_wait_for_each_other() {
        let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        // A reader holds; the change does not begin until it lets go.
        let changed = Arc::new(AtomicBool::new(false));
        let reading = title_memory_reading();
        let (began, wait) = mpsc::channel();
        let change = {
            let changed = changed.clone();
            std::thread::spawn(move || {
                began.send(()).unwrap();
                let _changing = title_memory_remapping();
                changed.store(true, Ordering::SeqCst);
            })
        };
        wait.recv().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(!changed.load(Ordering::SeqCst), "the mapping changed under a read in place");
        drop(reading);
        change.join().unwrap();
        assert!(changed.load(Ordering::SeqCst));
        // A change holds; the read does not begin until it is done.
        let read = Arc::new(AtomicBool::new(false));
        let changing = title_memory_remapping();
        let (began, wait) = mpsc::channel();
        let reader = {
            let read = read.clone();
            std::thread::spawn(move || {
                began.send(()).unwrap();
                let _reading = title_memory_reading();
                read.store(true, Ordering::SeqCst);
            })
        };
        wait.recv().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(!read.load(Ordering::SeqCst), "a read in place began during a change of mapping");
        drop(changing);
        reader.join().unwrap();
        assert!(read.load(Ordering::SeqCst));
    }

    /// Reads in place and changes of mapping wait for each other only where
    /// their ranges meet: a change elsewhere neither waits for a read nor
    /// holds one off (one lock for all of title memory kept the submitting
    /// thread waiting seconds a frame while the world streamed in).
    #[test]
    fn only_ranges_that_meet_wait_for_each_other() {
        let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        // A read under way: a change elsewhere goes ahead.
        let reading = title_memory_reading_range(16 * MIB, MIB);
        assert!(done_soon(|| drop(title_memory_remapping_range(32 * MIB, MIB))));
        // Ranges are half open: one that ends where the read begins does
        // not meet it.
        assert!(done_soon(|| drop(title_memory_remapping_range(15 * MIB, MIB))));
        drop(reading);
        // A change under way: reads elsewhere begin, reads of its range do
        // not.
        let changing = title_memory_remapping_range(32 * MIB, MIB);
        assert!(done_soon(|| drop(title_memory_reading_range(16 * MIB, MIB))));
        assert!(title_memory_reading_range_now(16 * MIB, MIB).is_some());
        assert!(title_memory_reading_range_now(32 * MIB + 4096, 1).is_none());
        assert!(title_memory_reading_range_now(31 * MIB, 2 * MIB).is_none());
        drop(changing);
        assert!(title_memory_reading_range_now(32 * MIB, MIB).is_some());
        // Nothing to change changes nothing, and waits for no read.
        let all = title_memory_reading();
        assert!(done_soon(|| drop(title_memory_remapping_range(32 * MIB, 0))));
        drop(all);
    }

    /// A change that waits for a read of its range keeps new reads of that
    /// range out, so reads that keep beginning cannot hold it off; reads
    /// elsewhere still begin.
    #[test]
    fn a_waiting_change_is_not_held_off() {
        let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        let reading = title_memory_reading_range(0, MIB);
        let changed = Arc::new(AtomicBool::new(false));
        let change = {
            let changed = changed.clone();
            std::thread::spawn(move || {
                let _changing = title_memory_remapping_range(0, 2 * MIB);
                changed.store(true, Ordering::SeqCst);
            })
        };
        // Until the change is waiting, a read of its second half begins.
        let mut tries = 0;
        while let Some(read) = title_memory_reading_range_now(MIB, MIB) {
            drop(read);
            tries += 1;
            assert!(tries < 5_000, "the change never began waiting");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!changed.load(Ordering::SeqCst), "the change went ahead of a read under way");
        assert!(title_memory_reading_range_now(4 * MIB, MIB).is_some());
        drop(reading);
        change.join().unwrap();
        assert!(changed.load(Ordering::SeqCst));
        assert!(title_memory_reading_range_now(MIB, MIB).is_some());
    }
}
