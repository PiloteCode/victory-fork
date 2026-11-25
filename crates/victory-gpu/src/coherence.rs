//! What becomes of what shaders write (the research report's stages 0, 3
//! and 4, "LA solution"): how much of what is copied back into title memory
//! the CPU reads before the card writes it again, and the copy-back's
//! modes. Two switches, both off by default; with both off nothing here
//! runs and nothing else changes.
//!
//! Every range a submission's shaders may write is copied back into title
//! memory once the card has run it (`crate::host::relay`: the card hashes
//! and compares the declared ranges, the pieces that changed are fetched
//! and merged byte by byte), 65 to 95 ms a frame of the thread that lands
//! flights in wave 5's gameplay, and reads of title memory those flights may
//! write wait for it (two to four waits a frame). A copy-back made only when
//! the CPU first touches a range (stage 3, "lazy") pays if most of those
//! bytes are not read before the card writes them again. The census found 97
//! of the 100 MB fetched back a frame in memory the title reserved
//! write-combining, which a console title seldom reads.
//!
//! `VICTORY_COPYBACK_STATS=1` ([`observing`]) measures it. One page of title
//! memory in [`every`] of those a merge writes (at most one per 64 KiB block,
//! at most [`MOST_ARMED`] at once) gets a guard (`PAGE_GUARD`, on top of its
//! protection) right after the merge's write, and what happens to it next
//! is counted, with the bytes the merge wrote into it:
//!
//! - the title touches it first: the guard fires in our handler ([`install`])
//!   at an instruction outside this executable (code of another module, a
//!   system library or the graphics driver, is looked through to its
//!   caller), read or write, and how long after the merge;
//! - the card writes it again first: the next merge into the page takes the
//!   guard off before it reads title memory ([`landing`]);
//! - the runtime or one of our copies writes over it first (a file or
//!   DirectStorage read into title memory, a title buffer copy):
//!   [`written_over`], from `crate::watch`'s notices of such writes;
//! - nothing touches it for [`expiry_ms`];
//! - its guard went without our handler seeing it (a system call touched
//!   the page, or someone changed its protection), or its memory was mapped
//!   again.
//!
//! Our own reads of the page (the refreshes that take it up to the card
//! again, the merges' reads, other code of this executable, the runtime's
//! included) are counted and do not end the watch: the guard is put back
//! after them (in place: when the copy is done, [`after_own_copy`]; through
//! the kernel, whose read fails on a guard and leaves it: [`read_again`]
//! takes it off in place and reads again). The
//! guards' changes of protection keep the page's guard
//! ([`protection_set`]), and every change of ours is made under the guards'
//! lock (`crate::watch::hold_guards`). The answer is in the per-second line
//! ([`line`]): the share of the bytes copied back that the title read
//! first.
//!
//! What it costs and risks, with the switch on only: a fault and a change
//! of protection per own read of a watched page (some microseconds, at most
//! [`MOST_ARMED`] pages); a system call of the title that reads or writes a
//! watched page fails (the kernel leaves the guard) until the page is let go
//! (its next touch in place, [`expiry_ms`] at most), as one that writes into
//! a guarded block does today. The runtime's own reads and writes into title
//! memory take the guards off first (`crate::watch::writing`).
//!
//! `VICTORY_COPYBACK=off` ([`off`]) is a MEASUREMENT mode, never a default:
//! what shaders write is not copied back into title memory at all, but for
//! declared ranges of at most [`off_keep`] bytes (`VICTORY_COPYBACK_OFF_KEEP`,
//! 256; their addresses are logged: the small values a title may poll), and
//! reads of title memory that flights' shaders may write no longer wait for
//! the flights (`crate::host`'s `blocked_by`; they still do with the
//! textures shaders write copied back, `VICTORY_TEXTURE_BACK`: see
//! [`reads_wait`]). Copies in the card's memory are neither hashed nor
//! compared nor fetched for the ranges left out. Fences, labels and
//! bottom-of-pipe writes, the title's read-backs (texture backs among
//! them), presents and the flights' landing are as before. It shows the
//! ceiling of stages 3 and 4, and what breaks when the CPU never sees the
//! card's results; expect wrong pictures
//! where the title's CPU writes a 64 KiB piece the card also wrote (the next
//! upload sends the title's stale bytes over the card's).
//!
//! `VICTORY_COPYBACK=lazy` ([`lazy`], stage 3; off by default) keeps what
//! shaders wrote on the card until the CPU touches it: see the section
//! "Stage 3" below. `=predict` (stage 4) is not in this build: it copies
//! back as before, and says so.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock};

/// The kernel's page: the unit a guard covers.
const KERNEL_PAGE: usize = 4096;
/// At most one watched page per such block: a kernel read of title memory
/// that fails is tried again by 64 KiB pieces, then by 4 KiB pages
/// (`crate::host`'s `read_pieces`), so one guard a block is always got past.
const BLOCK: usize = 64 << 10;
/// The most pages watched at once.
pub(crate) const MOST_ARMED: usize = 256;

/// What shaders write is copied back as before ([`Mode::Eager`]), not
/// ([`Mode::Off`], a measurement mode), or at the CPU's first touch
/// ([`Mode::Lazy`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Mode {
    Eager,
    Off,
    Lazy,
}

/// `VICTORY_COPYBACK`: unset (or `eager`) copies back as before; `off`, a
/// measurement mode ([`off`]); `lazy`, stage 3 ([`lazy`]). Read once.
pub(crate) fn mode() -> Mode {
    static MODE: OnceLock<Mode> = OnceLock::new();
    *MODE.get_or_init(|| {
        let asked = std::env::var("VICTORY_COPYBACK").unwrap_or_default();
        match asked.trim() {
            "off" => {
                tracing::warn!(
                    keep = off_keep(),
                    "VICTORY_COPYBACK=off, a MEASUREMENT mode: what shaders write is not copied back into title memory \
                     (but for declared ranges of at most `keep` bytes, VICTORY_COPYBACK_OFF_KEEP, logged by address), and \
                     reads of title memory that flights' shaders write do not wait for them: expect wrong pictures"
                );
                Mode::Off
            }
            "lazy" => {
                tracing::info!(
                    window_kb = WINDOW >> 10,
                    keep = off_keep(),
                    write_combining = lazy_wc(),
                    demote_after = demote_after(),
                    "VICTORY_COPYBACK=lazy: the 64 KiB pieces shaders wrote to a title buffer's copy in the card's memory stay \
                     there, their title memory closed, until the CPU first touches them (write-watched memory held by one \
                     copy; the rest is copied back as before)"
                );
                Mode::Lazy
            }
            "" | "eager" => Mode::Eager,
            other => {
                tracing::warn!("VICTORY_COPYBACK={other} is not in this build (predict is planned): copied back as before");
                Mode::Eager
            }
        }
    })
}

/// [`Mode::Off`]: nothing shaders write is copied back but small ranges.
pub(crate) fn off() -> bool {
    mode() == Mode::Off
}

/// Whether our reads of title memory that flights' shaders write wait for
/// the flights to land (`crate::host`'s `blocked_by`): always, but with
/// [`off`], where nothing of those writes comes back. Unless the textures
/// shaders write are copied back too (`VICTORY_TEXTURE_BACK`, fxceless#999,
/// wave 8): their ranges are joined with the declared writes in the line of
/// flights, they are real reads of the card's results (the ocean's heights)
/// and they still land, so the waits stay with it; `off` then saves the
/// fetches and merges only.
pub(crate) fn reads_wait() -> bool {
    static WAIT: OnceLock<bool> = OnceLock::new();
    *WAIT.get_or_init(|| !off() || crate::texback::on())
}

/// With [`off`], the most bytes of a declared range that is still copied
/// back (`VICTORY_COPYBACK_OFF_KEEP`, 256; 0 keeps none): a counter or a
/// flag the title may wait on. Those it keeps are logged by address.
pub(crate) fn off_keep() -> usize {
    static KEEP: OnceLock<usize> = OnceLock::new();
    *KEEP.get_or_init(|| {
        std::env::var("VICTORY_COPYBACK_OFF_KEEP").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(256)
    })
}

/// Whether a declared range of `len` bytes is copied back: always but with
/// `off`, and then only up to `keep` bytes.
fn keeps(off: bool, keep: usize, len: usize) -> bool {
    !off || len <= keep
}

/// Since the last line, with [`off`]: declared ranges left out and their
/// bytes, ranges kept and their bytes.
static OFF_COUNT: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

/// The starts of the ranges [`off`] kept, to log each once (the first 64).
static OFF_KEPT: Mutex<Option<HashSet<usize>>> = Mutex::new(None);

/// Whether what shaders may write over title memory `[from, to)` (one
/// declared range) is copied back. Always true but with [`off`]; then
/// counted, and a range kept is logged the first time.
pub(crate) fn copied_back(from: usize, to: usize) -> bool {
    if !off() || to <= from {
        return true;
    }
    let len = to - from;
    if !keeps(true, off_keep(), len) {
        OFF_COUNT[0].fetch_add(1, Relaxed);
        OFF_COUNT[1].fetch_add(len as u64, Relaxed);
        return false;
    }
    OFF_COUNT[2].fetch_add(1, Relaxed);
    OFF_COUNT[3].fetch_add(len as u64, Relaxed);
    let mut kept = OFF_KEPT.lock().unwrap_or_else(|e| e.into_inner());
    let kept = kept.get_or_insert_with(HashSet::new);
    if kept.len() < 4096 && kept.insert(from) && kept.len() <= 64 {
        tracing::info!(
            "VICTORY_COPYBACK=off still copies back a small declared range: {from:#x}+{len:#x} (a value the title may wait on), frame {}",
            crate::frame::current()
        );
    }
    true
}

/// `VICTORY_COPYBACK_STATS=1`: what becomes of the bytes copied back is
/// measured (the module's header).
pub(crate) fn observing() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let on = crate::switch::on("VICTORY_COPYBACK_STATS");
        if on {
            tracing::info!(
                every = every(),
                most = MOST_ARMED,
                expiry_ms = expiry_ms(),
                "VICTORY_COPYBACK_STATS=1: one page in `every` that merges write into title memory is watched for what touches \
                 it first (a one-shot guard)"
            );
        }
        on
    })
}

/// One page in how many merged pages is watched (`VICTORY_COPYBACK_STATS_EVERY`, 8).
fn every() -> u64 {
    static N: OnceLock<u64> = OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("VICTORY_COPYBACK_STATS_EVERY").ok().and_then(|v| v.trim().parse().ok()).filter(|&n| n > 0).unwrap_or(8)
    })
}

/// How long a watched page nothing touched stays watched
/// (`VICTORY_COPYBACK_STATS_MS`, 4000).
fn expiry_ms() -> u64 {
    static MS: OnceLock<u64> = OnceLock::new();
    *MS.get_or_init(|| {
        std::env::var("VICTORY_COPYBACK_STATS_MS").ok().and_then(|v| v.trim().parse().ok()).filter(|&n| n > 0).unwrap_or(4000)
    })
}

fn now_ms() -> u64 {
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64
}

/// Where a watched page is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    /// Its guard is on.
    Armed,
    /// Our own code read it (the guard went): put back at the next
    /// [`rearm`].
    Rearm,
    /// Being let go by a read of ours ([`let_go`]): a fault on it is ours.
    Touching,
}

/// A watched page.
#[derive(Clone, Copy, Debug)]
struct Watched {
    state: State,
    /// When the merge wrote it ([`now_ms`]).
    since: u64,
    /// The bytes that merge wrote into the page.
    bytes: u32,
    /// In memory the title reserved write-combining (`crate::census`).
    wc: bool,
    /// Whether our own code read it since.
    own: bool,
}

/// What became of a watched page first: the places of [`Tally`].
const TITLE_READ: usize = 0;
const TITLE_WRITE: usize = 1;
const CARD_AGAIN: usize = 2;
const WRITTEN_OVER: usize = 3;
const UNTOUCHED: usize = 4;
const LOST: usize = 5;
const REMAPPED: usize = 6;
const OUTCOMES: usize = 7;

/// Counts of what became of watched pages: per outcome, pages, the bytes
/// the merges wrote into them, those in write-combining memory, those our
/// own code read before; the title's first touches by how long after the
/// merge (under 1, 10, 100, 1000 ms, later).
#[derive(Clone, Copy, Default)]
struct Tally {
    pages: [u64; OUTCOMES],
    bytes: [u64; OUTCOMES],
    wc: [u64; OUTCOMES],
    own: [u64; OUTCOMES],
    after: [u64; 5],
}

impl Tally {
    fn add(&mut self, w: &Watched, outcome: usize, now: u64) {
        self.pages[outcome] += 1;
        self.bytes[outcome] += u64::from(w.bytes);
        self.wc[outcome] += u64::from(w.wc);
        self.own[outcome] += u64::from(w.own);
        if outcome == TITLE_READ || outcome == TITLE_WRITE {
            let ms = now.saturating_sub(w.since);
            let bucket = [1, 10, 100, 1000].iter().position(|&b| ms < b).unwrap_or(4);
            self.after[bucket] += 1;
        }
    }
}

struct Observed {
    /// Watched pages by address.
    pages: BTreeMap<usize, Watched>,
    /// Since the last line, and since the start.
    now: Tally,
    total: Tally,
    /// Since the last line, the first pages the title touched first: the
    /// address, whether it wrote, the milliseconds since the merge (the
    /// ranges a pre-copy, stage 4, would learn).
    said: Vec<(usize, bool, u64)>,
}

static OBSERVED: Mutex<Observed> =
    Mutex::new(Observed { pages: BTreeMap::new(), now: TALLY, total: TALLY, said: Vec::new() });
const TALLY: Tally = Tally { pages: [0; OUTCOMES], bytes: [0; OUTCOMES], wc: [0; OUTCOMES], own: [0; OUTCOMES], after: [0; 5] };

fn observed() -> std::sync::MutexGuard<'static, Observed> {
    OBSERVED.lock().unwrap_or_else(|e| e.into_inner())
}

impl Observed {
    /// `w`, let go, came to `outcome`.
    fn resolve(&mut self, w: &Watched, outcome: usize) {
        let now = now_ms();
        self.now.add(w, outcome, now);
        self.total.add(w, outcome, now);
        WATCHED.store(self.pages.len(), Relaxed);
    }
}

/// How many pages are watched ([`Observed::pages`]' length as last
/// changed): no lock is taken to pick more while it is at [`MOST_ARMED`].
static WATCHED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Since the last line: merges' writes into title memory and their bytes;
/// pages watched, pages that could not be (not committed read-write or
/// read-only, or the limit); our own reads of watched pages in place,
/// through the kernel, by other code of ours.
static COUNT: [AtomicU64; 7] = [const { AtomicU64::new(0) }; 7];
/// Merged pages seen, for picking one in [`every`].
static SEEN: AtomicU64 = AtomicU64::new(0);
/// A page waits for its guard to be put back ([`State::Rearm`]).
static REARM_DUE: AtomicBool = AtomicBool::new(false);

/// The pages of `[at, at + len)` (`len > 0`), by address.
fn pages_of(at: usize, len: usize) -> std::ops::RangeInclusive<usize> {
    at / KERNEL_PAGE..=(at + len - 1) / KERNEL_PAGE
}

/// The bytes of `[at, at + len)` in the page at `page`.
fn overlap(at: usize, len: usize, page: usize) -> usize {
    (at + len).min(page + KERNEL_PAGE).saturating_sub(at.max(page))
}

/// A merge wrote title memory `[at, at + len)` (what shaders wrote): one
/// page in [`every`] of them gets a guard. Called right after the write,
/// while its `crate::watch::Writing` holds the blocks open, so the guards
/// do not make them read-only meanwhile.
pub(crate) fn merged(at: usize, len: usize) {
    if len == 0 || !observing() {
        return;
    }
    COUNT[0].fetch_add(1, Relaxed);
    COUNT[1].fetch_add(len as u64, Relaxed);
    let every = every();
    let picked: Vec<usize> = pages_of(at, len).filter(|_| SEEN.fetch_add(1, Relaxed).is_multiple_of(every)).collect();
    if !picked.is_empty() {
        if WATCHED.load(Relaxed) >= MOST_ARMED {
            COUNT[3].fetch_add(picked.len() as u64, Relaxed);
        } else {
            let picked: Vec<(usize, u32, bool)> = picked
                .into_iter()
                .map(|page| {
                    let page = page * KERNEL_PAGE;
                    (page, overlap(at, len, page) as u32, crate::census::is_wc(page as u64))
                })
                .collect();
            arm(&picked);
        }
    }
    rearm();
}

/// Guards `pages` (address, bytes the merge wrote there, write-combining),
/// those that can be.
fn arm(pages: &[(usize, u32, bool)]) {
    install();
    crate::watch::hold_guards(|| {
        let mut o = observed();
        for &(page, bytes, wc) in pages {
            let block = page - page % BLOCK;
            if o.pages.len() >= MOST_ARMED || o.pages.range(block..block + BLOCK).next().is_some() {
                COUNT[3].fetch_add(1, Relaxed);
                continue;
            }
            if guard(page) {
                o.pages.insert(page, Watched { state: State::Armed, since: now_ms(), bytes, wc, own: false });
                COUNT[2].fetch_add(1, Relaxed);
            } else {
                COUNT[3].fetch_add(1, Relaxed);
            }
        }
        WATCHED.store(o.pages.len(), Relaxed);
    });
}

/// The page's protection, if it is committed: (protection, guard on).
fn protection(page: usize) -> Option<(u32, bool)> {
    use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_GUARD, VirtualQuery};
    // SAFETY: plain data filled in by the query.
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a valid out pointer of the right size.
    let n = unsafe { VirtualQuery(page as *const _, &mut info, size_of_val(&info)) };
    (n != 0 && info.State == MEM_COMMIT).then_some((info.Protect & !PAGE_GUARD, info.Protect & PAGE_GUARD != 0))
}

/// Puts a guard on the page at `page` over its protection, if that is
/// read-write or read-only; true if the guard is on (now or already).
/// Under the guards' lock: no protection of ours changes meanwhile.
fn guard(page: usize) -> bool {
    use windows_sys::Win32::System::Memory::{PAGE_GUARD, PAGE_READONLY, PAGE_READWRITE, VirtualProtect};
    let Some((protect, on)) = protection(page) else {
        return false;
    };
    if on {
        return true;
    }
    if protect != PAGE_READWRITE && protect != PAGE_READONLY {
        return false;
    }
    let mut old = 0;
    // SAFETY: a committed page of title memory; only its protection changes.
    if unsafe { VirtualProtect(page as *const _, KERNEL_PAGE, protect | PAGE_GUARD, &mut old) } == 0 {
        return false;
    }
    if old != protect {
        // Changed between the question and the change (the runtime): put
        // back what it had.
        // SAFETY: as above.
        unsafe { VirtualProtect(page as *const _, KERNEL_PAGE, old, &mut old) };
        return false;
    }
    true
}

/// The guards of `crate::watch` set the protection of title memory
/// `[start, end)` to `protect` (read-only or read-write), which took the
/// guards of the watched pages there: they are put back. The caller holds
/// the guards' lock.
pub(crate) fn protection_set(start: usize, end: usize, protect: u32) {
    if observing() {
        guards_again(start, end, protect);
    }
}

/// [`protection_set`], whatever the switch says.
fn guards_again(start: usize, end: usize, protect: u32) {
    use windows_sys::Win32::System::Memory::{PAGE_GUARD, VirtualProtect};
    let mut o = observed();
    for (&page, w) in o.pages.range_mut(start..end) {
        if w.state == State::Touching {
            continue;
        }
        let mut old = 0;
        // SAFETY: a page of title memory whose protection was just set to
        // this; only its protection changes.
        if unsafe { VirtualProtect(page as *const _, KERNEL_PAGE, protect | PAGE_GUARD, &mut old) } != 0 {
            w.state = State::Armed;
        }
    }
}

/// Puts the guards back on the pages our own code read ([`State::Rearm`]);
/// a page whose memory is no longer read-write or read-only is let go.
fn rearm() {
    if !REARM_DUE.swap(false, Relaxed) {
        return;
    }
    crate::watch::hold_guards(|| {
        let mut o = observed();
        let due: Vec<usize> = o.pages.iter().filter(|(_, w)| w.state == State::Rearm).map(|(&p, _)| p).collect();
        for page in due {
            if guard(page) {
                if let Some(w) = o.pages.get_mut(&page) {
                    w.state = State::Armed;
                }
            } else if let Some(w) = o.pages.remove(&page) {
                o.resolve(&w, LOST);
            }
        }
    });
}

/// A copy of title memory made in place by us is done
/// (`crate::plaincopy::copy`): the guards it took are put back.
pub(crate) fn after_own_copy() {
    if REARM_DUE.load(Relaxed) {
        rearm();
    }
}

/// Lets go of the watched pages in `[at, at + len)` (the guard taken off by
/// a read of ours, which our handler lets through), counted as `outcome`.
fn let_go(at: usize, len: usize, outcome: usize) {
    if len == 0 {
        return;
    }
    let first = at - at % KERNEL_PAGE;
    let pages: Vec<usize> = {
        let mut o = observed();
        if o.pages.is_empty() {
            return;
        }
        let found: Vec<usize> = o.pages.range(first..at.saturating_add(len)).map(|(&p, _)| p).collect();
        for page in &found {
            if let Some(w) = o.pages.get_mut(page) {
                w.state = State::Touching;
            }
        }
        found
    };
    if pages.is_empty() {
        return;
    }
    for &page in &pages {
        // A page that is not there any more ends the read without a fault.
        crate::plaincopy::touch(page);
    }
    let mut o = observed();
    for page in pages {
        if let Some(w) = o.pages.remove(&page) {
            o.resolve(&w, outcome);
        }
    }
}

/// A merge is about to read title memory `[at, at + len)` and write what
/// the card wrote there: the card wrote the watched pages there again
/// before anything else touched them.
pub(crate) fn landing(at: usize, len: usize) {
    if observing() {
        let_go(at, len, CARD_AGAIN);
    }
}

/// The runtime (a file or DirectStorage read) or a copy of ours is about to
/// write title memory `[at, at + len)`, or has written it through another
/// view (`crate::watch`): the watched pages there were written over.
pub(crate) fn written_over(at: usize, len: usize) {
    if observing() {
        let_go(at, len, WRITTEN_OVER);
    }
    // What the card keeps there comes back first: the kernel's write would
    // fail on a closed page, and a write of part of a piece must land on
    // the card's bytes, not on the stale ones.
    if lazy() {
        release(at, len, BY_WRITING);
    }
}

/// The runtime mapped or unmapped title memory `[at, at + len)`: the
/// watched pages there show other memory now.
pub(crate) fn remapped(at: usize, len: usize) {
    if observing() {
        let_go(at, len, REMAPPED);
    }
    // Pieces kept on the card there no longer name what the addresses show.
    if lazy() {
        forget(at, len);
    }
}

/// The kernel's read of title memory `[src, src + len)` failed. A guard of
/// ours there fails every such read, and stays (the kernel does not take
/// it): the watched pages there are read in place first, which takes their
/// guards (our handler lets that through), counted as read by our own code
/// through the kernel and watched on (the guards come back at the next
/// [`rearm`]); then the read is tried again with `again`. Whether it read.
pub(crate) fn read_again(src: usize, len: usize, mut again: impl FnMut() -> bool) -> bool {
    // A page the lazy copy-back keeps closed fails it too: what the card
    // keeps there comes back, and the read is tried again.
    if lazy() && release(src, len, BY_KERNEL) && again() {
        return true;
    }
    observing() && read_through(src, len, again)
}

/// [`read_again`], whatever the switch says.
fn read_through(src: usize, len: usize, mut again: impl FnMut() -> bool) -> bool {
    if len == 0 {
        return false;
    }
    let first = src - src % KERNEL_PAGE;
    // Twice at most: a guard put back by another thread meanwhile fails the
    // read once more.
    for _ in 0..2 {
        let watched: Vec<usize> = {
            let mut o = observed();
            let found: Vec<usize> = o
                .pages
                .range(first..src.saturating_add(len))
                .filter(|(_, w)| w.state != State::Touching)
                .map(|(&p, _)| p)
                .collect();
            for page in &found {
                if let Some(w) = o.pages.get_mut(page)
                    && w.state == State::Armed
                {
                    // Our handler lets the touch below through without
                    // counting it: it is counted here.
                    w.state = State::Rearm;
                    w.own = true;
                    COUNT[5].fetch_add(1, Relaxed);
                }
            }
            found
        };
        if watched.is_empty() {
            return false;
        }
        REARM_DUE.store(true, Relaxed);
        for &page in &watched {
            crate::plaincopy::touch(page);
        }
        if again() {
            return true;
        }
    }
    false
}

/// Who touched a watched page, by the faulting instruction.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Who {
    /// Our copy in place (`crate::plaincopy`).
    OwnCopy,
    /// Other code of this executable (the runtime's included).
    Own,
    /// The title (code in no module, or another module's code called from
    /// code outside this executable).
    Title,
}

/// `[start, end)` of the module holding `address`.
fn module_of(address: usize) -> Option<(usize, usize)> {
    use windows_sys::Win32::System::LibraryLoader::{
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, GetModuleHandleExW,
    };
    let mut module = std::ptr::null_mut();
    // SAFETY: asks which loaded module holds an address; the handle is not
    // counted (the module stays loaded with the process).
    let found = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            address as *const u16,
            &mut module,
        )
    } != 0;
    if !found || module.is_null() {
        return None;
    }
    let base = module as usize;
    // SAFETY: a loaded module's headers are mapped and readable: the PE
    // header's offset at 0x3c, its optional header's SizeOfImage 80 bytes
    // in.
    let size = unsafe {
        let header = (base as *const u8).add(0x3c).cast::<u32>().read_unaligned() as usize;
        (base as *const u8).add(header + 80).cast::<u32>().read_unaligned() as usize
    };
    Some((base, base + size))
}

/// This executable's image, and the other modules loaded when the first
/// page was watched: system libraries whose code both the title and we call
/// (memory copies among them), the graphics driver. Code in those is told
/// by its caller ([`who`]).
fn images() -> &'static ((usize, usize), Vec<(usize, usize)>) {
    static IMAGES: OnceLock<((usize, usize), Vec<(usize, usize)>)> = OnceLock::new();
    IMAGES.get_or_init(|| {
        use windows_sys::Win32::Foundation::HMODULE;
        use windows_sys::Win32::System::ProcessStatus::K32EnumProcessModules;
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        let own = module_of(images as *const () as usize).unwrap_or((0, 0));
        let mut modules: Vec<HMODULE> = vec![std::ptr::null_mut(); 2048];
        let mut needed = 0u32;
        // SAFETY: room for `modules.len()` handles, its size in bytes given;
        // the bytes needed come back.
        let listed = unsafe {
            K32EnumProcessModules(GetCurrentProcess(), modules.as_mut_ptr(), size_of_val(&modules[..]) as u32, &mut needed)
        } != 0;
        let n = if listed { (needed as usize / size_of::<HMODULE>()).min(modules.len()) } else { 0 };
        let foreign = modules[..n].iter().map(|&m| m as usize).filter(|&m| m != 0 && m != own.0).filter_map(module_of).collect();
        (own, foreign)
    })
}

/// Who ran the instruction at `rip`, given this executable's image, the
/// other modules' ([`images`]) and our copy's instruction; `caller` looks
/// through another module's code to the code that called it (`None`: not
/// known, then the title). Code in no module is the title's.
fn who(rip: usize, own: (usize, usize), foreign: &[(usize, usize)], copy: usize, caller: impl FnOnce() -> Option<usize>) -> Who {
    let inside = |at: usize, (a, b): (usize, usize)| a <= at && at < b;
    if rip == copy && copy != 0 {
        return Who::OwnCopy;
    }
    if inside(rip, own) {
        return Who::Own;
    }
    if foreign.iter().any(|&r| inside(rip, r)) && caller().is_some_and(|c| inside(c, own)) {
        return Who::Own;
    }
    Who::Title
}

/// The first return address outside the other modules above `context`'s
/// frame (four frames at most), as the crash reporter walks a stack.
///
/// # Safety
/// `context` is a faulting thread's context, on that thread.
unsafe fn caller(context: &windows_sys::Win32::System::Diagnostics::Debug::CONTEXT, foreign: &[(usize, usize)]) -> Option<usize> {
    use windows_sys::Win32::System::Diagnostics::Debug::{CONTEXT, RtlLookupFunctionEntry, RtlVirtualUnwind, UNW_FLAG_NHANDLER};
    use windows_sys::Win32::System::Threading::GetCurrentThreadStackLimits;
    // `RtlVirtualUnwind` stores the context 16 bytes aligned.
    #[repr(C, align(16))]
    struct Aligned(CONTEXT);
    let (mut low, mut high) = (0usize, 0usize);
    // SAFETY: two out pointers.
    unsafe { GetCurrentThreadStackLimits(&mut low, &mut high) };
    let mut copy = Aligned(*context);
    let c = &mut copy.0;
    for _ in 0..4 {
        let rsp = c.Rsp as usize;
        if rsp < low || rsp.checked_add(8).is_none_or(|end| end > high) {
            return None;
        }
        let mut image = 0u64;
        // SAFETY: a lookup of an address in the unwind tables.
        let entry = unsafe { RtlLookupFunctionEntry(c.Rip, &mut image, std::ptr::null_mut()) };
        if entry.is_null() {
            // A leaf: its return address is at the top of the stack.
            // SAFETY: inside this thread's stack (checked above).
            c.Rip = unsafe { (rsp as *const u64).read_unaligned() };
            c.Rsp = c.Rsp.wrapping_add(8);
        } else {
            let (mut data, mut frame) = (std::ptr::null_mut(), 0);
            // SAFETY: `entry` belongs to `image`; the context is our copy.
            unsafe { RtlVirtualUnwind(UNW_FLAG_NHANDLER, image, c.Rip, entry, c, &mut data, &mut frame, std::ptr::null_mut()) };
        }
        let at = c.Rip as usize;
        if at == 0 {
            return None;
        }
        if !foreign.iter().any(|&(a, b)| a <= at && at < b) {
            return Some(at);
        }
    }
    None
}

/// Registers the handler of the watched pages' guards, first in the line
/// (ahead of the copies in place, whose own handler would take a guard at
/// their instruction for a page not there), once.
fn install() {
    use windows_sys::Win32::Foundation::{EXCEPTION_GUARD_PAGE, NTSTATUS};
    use windows_sys::Win32::System::Diagnostics::Debug::{
        AddVectoredExceptionHandler, EXCEPTION_CONTINUE_EXECUTION, EXCEPTION_CONTINUE_SEARCH, EXCEPTION_POINTERS,
    };
    unsafe extern "system" fn on_guard(info: *mut EXCEPTION_POINTERS) -> i32 {
        // SAFETY: the system passes the exception being dispatched and the
        // context it will continue with.
        let (r, context) = unsafe { (&*(*info).ExceptionRecord, &*(*info).ContextRecord) };
        if r.ExceptionCode != EXCEPTION_GUARD_PAGE as NTSTATUS || r.NumberParameters < 2 {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        let page = r.ExceptionInformation[1] - r.ExceptionInformation[1] % KERNEL_PAGE;
        const WRITE: usize = 1;
        let write = r.ExceptionInformation[0] == WRITE;
        // Not ours: someone else's guard (a stack's is the kernel's own).
        if !observed().pages.contains_key(&page) {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        let (own, foreign) = images();
        // SAFETY: the faulting thread's context, on that thread.
        let who = who(context.Rip as usize, *own, foreign, crate::plaincopy::copy_instruction(), || unsafe { caller(context, foreign) });
        let mut o = observed();
        let Some(w) = o.pages.get_mut(&page) else {
            // Let go meanwhile: the guard was ours all the same.
            return EXCEPTION_CONTINUE_EXECUTION;
        };
        if w.state != State::Armed {
            return EXCEPTION_CONTINUE_EXECUTION;
        }
        match who {
            Who::OwnCopy | Who::Own => {
                w.state = State::Rearm;
                w.own = true;
                COUNT[if who == Who::OwnCopy { 4 } else { 6 }].fetch_add(1, Relaxed);
                REARM_DUE.store(true, Relaxed);
            }
            Who::Title => {
                if let Some(w) = o.pages.remove(&page) {
                    o.resolve(&w, if write { TITLE_WRITE } else { TITLE_READ });
                    if o.said.len() < 16 {
                        let after = now_ms().saturating_sub(w.since);
                        o.said.push((page, write, after));
                    }
                }
            }
        }
        EXCEPTION_CONTINUE_EXECUTION
    }
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // Looked up here, not in the handler.
        let _ = images();
        // The copies in place register their handler at their first copy:
        // made now, so that this one, registered after it, runs before it.
        // A guard at their instruction is then counted here and let
        // through, not made a failed copy (which would go to the kernel's
        // read, take the guard unseen, and leave it counted as gone).
        static PROBE: u8 = 0;
        crate::plaincopy::touch(&raw const PROBE as usize);
        // SAFETY: the handler reads the record and context it is given, and
        // touches only the watched pages' state.
        unsafe { AddVectoredExceptionHandler(1, Some(on_guard)) };
    });
}

/// Lets go of the pages watched longer than [`expiry_ms`]: those whose
/// guard is still on were touched by nothing, the others lost it without our
/// handler (a system call, a change of protection).
fn expire() {
    expire_at(now_ms());
}

/// [`expire`] with the clock at `now` ([`now_ms`]).
fn expire_at(now: u64) {
    let old = |w: &Watched| now.saturating_sub(w.since) >= expiry_ms();
    if !observed().pages.values().any(old) {
        return;
    }
    let touch: Vec<usize> = crate::watch::hold_guards(|| {
        let mut o = observed();
        let due: Vec<(usize, Watched)> = o.pages.iter().filter(|(_, w)| old(w) && w.state != State::Touching).map(|(&p, &w)| (p, w)).collect();
        let mut touch = Vec::new();
        for (page, w) in due {
            if w.state == State::Armed && protection(page).is_none_or(|(_, on)| !on) {
                o.pages.remove(&page);
                o.resolve(&w, LOST);
            } else if let Some(w) = o.pages.get_mut(&page) {
                // Still guarded, or waiting for its guard after our own
                // read: nothing else touched it.
                w.state = State::Touching;
                touch.push(page);
            }
        }
        touch
    });
    for &page in &touch {
        crate::plaincopy::touch(page);
    }
    let mut o = observed();
    for page in touch {
        if let Some(w) = o.pages.remove(&page) {
            o.resolve(&w, UNTOUCHED);
        }
    }
}

/// The per-second lines (with the frame line), over `frames`.
pub(crate) fn line(frames: f64) {
    if off() {
        let [left, left_bytes, kept, kept_bytes] = [0, 1, 2, 3].map(|i| OFF_COUNT[i].swap(0, Relaxed) as f64 / frames);
        let seen = OFF_KEPT.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map_or(0, HashSet::len);
        tracing::info!(
            "copy-back off (VICTORY_COPYBACK=off, a measurement mode) per frame: {left:.0} declared ranges ({:.1} MB) not copied \
             back, {kept:.1} kept and copied back ({:.1} KB; ranges of at most {} bytes, VICTORY_COPYBACK_OFF_KEEP; {seen} kept \
             ranges since the start)",
            left_bytes / 1e6,
            kept_bytes / 1e3,
            off_keep()
        );
    }
    if lazy() {
        lazy_line(frames);
    }
    if !observing() {
        return;
    }
    rearm();
    expire();
    let c: Vec<f64> = COUNT.iter().map(|c| c.swap(0, Relaxed) as f64).collect();
    let (now, total, watched, said) = {
        let mut o = observed();
        let now = std::mem::take(&mut o.now);
        let said = std::mem::take(&mut o.said);
        (now, o.total, o.pages.len(), said)
    };
    if !said.is_empty() {
        let pages: Vec<String> = said
            .iter()
            .map(|&(page, write, ms)| format!("{page:#x} {} after {ms} ms", if write { "written" } else { "read" }))
            .collect();
        tracing::info!(?pages, "copy-back watched: pages the title touched first, since the last line (VICTORY_COPYBACK_STATS=1)");
    }
    let read_share = |t: &Tally| {
        // Of the bytes of the pages whose fate is known (not lost, not
        // mapped again), those the title read or wrote first.
        let known: u64 = [TITLE_READ, TITLE_WRITE, CARD_AGAIN, WRITTEN_OVER, UNTOUCHED].iter().map(|&i| t.bytes[i]).sum();
        let pct = |v: u64| if known > 0 { 100.0 * v as f64 / known as f64 } else { 0.0 };
        (pct(t.bytes[TITLE_READ]), pct(t.bytes[TITLE_WRITE]), known)
    };
    let (read_now, write_now, _) = read_share(&now);
    let (read_all, write_all, known_all) = read_share(&total);
    let p = &now.pages;
    tracing::info!(
        "copy-back watched (VICTORY_COPYBACK_STATS=1, a page in {} of those merges write): per frame {:.1} MB copied back in \
         {:.0} writes; since the last line {:.0} pages watched ({:.0} could not be), {} let go: the title read first {}, \
         wrote first {}, the card wrote again first {}, the runtime or our copies wrote over {}, untouched for {} ms {}, \
         guard gone without our handler {}, mapped again {}; of the bytes copied back into those whose fate is known the \
         title read {read_now:.1}% first and wrote {write_now:.1}% first; our own reads of watched pages: {:.0} in place, \
         {:.0} through the kernel, {:.0} by other code of ours ({} of the pages let go); the title's first touches after \
         under 1 ms {}, 10 ms {}, 100 ms {}, 1 s {}, later {}; in write-combining memory {} let go, the title read {}; \
         since the start {} let go, the title read {} first (bytes {read_all:.1}%) and wrote {} first (bytes \
         {write_all:.1}%), the card wrote again first {}, the runtime or our copies {}, untouched {}, gone {}, mapped \
         again {} ({} KB of known fate); {watched} watched now",
        every(),
        c[1] / 1e6 / frames,
        c[0] / frames,
        c[2],
        c[3],
        p.iter().sum::<u64>(),
        p[TITLE_READ],
        p[TITLE_WRITE],
        p[CARD_AGAIN],
        p[WRITTEN_OVER],
        expiry_ms(),
        p[UNTOUCHED],
        p[LOST],
        p[REMAPPED],
        c[4],
        c[5],
        c[6],
        now.own.iter().sum::<u64>(),
        now.after[0],
        now.after[1],
        now.after[2],
        now.after[3],
        now.after[4],
        now.wc.iter().sum::<u64>(),
        now.wc[TITLE_READ],
        total.pages.iter().sum::<u64>(),
        total.pages[TITLE_READ],
        total.pages[TITLE_WRITE],
        total.pages[CARD_AGAIN],
        total.pages[WRITTEN_OVER],
        total.pages[UNTOUCHED],
        total.pages[LOST],
        total.pages[REMAPPED],
        known_all >> 10,
    );
}

// Stage 3: `VICTORY_COPYBACK=lazy`.
//
// At a flight's landing the card's compare still flags the 64 KiB pieces
// its shaders changed (`crate::host`'s `relay_card_back`). A flagged piece
// that may be left on the card ([`take`]) is not fetched: it is "owned by
// the card", noted here with the copy that holds it and the flight that
// wrote it last, and the 4 KiB pages of title memory under it are closed
// (no access). The title's first touch of a closed page faults into our
// handler ([`install`], first in the line, ahead of the copies in place and
// the guards), which fetches the owned pieces of the 512 KiB window around
// it through the relay's copy queue (no title list), puts what shaders
// wrote into title memory and the copy's CPU side, opens the pages and lets
// the instruction run again ([`resolve_range`]). Our own reads and writes
// of title memory bring the pieces back the same way first: in place they
// fault like the title's; through the kernel they fail, and are tried again
// after ([`read_again`]); the runtime's writes ([`written_over`], through
// `crate::watch::writing`) and system calls that read title buffers
// ([`kernel_will_read`]) bring them back before the kernel is asked.
//
// Which pieces may be left on the card (the rest is copied back as before,
// counted by reason in the per-second line):
// - a title buffer's copy in the card's memory, not a window outside
//   buffers, and no other host copy, staged texture or fill read ahead of a
//   flight holds any of the piece: their bytes would be older than the
//   card's with nothing to say so (the write watch sees no write);
// - a declared range longer than `VICTORY_COPYBACK_OFF_KEEP` (256 bytes),
//   not a texture read back (`VICTORY_TEXTURE_BACK`): small values the
//   title may poll;
// - memory the runtime reserved write-watched (`XMemVirtualAlloc`, never a
//   view of physical pages: no other address shows those pages, so one
//   change of protection closes every way in), committed read-write; with
//   `VICTORY_COPYBACK_LAZY_WC=0` not write-combining memory;
// - no title bytes still on their way to the card there (the copy's CPU
//   side holds bytes not sent, or sent after the flight), and no write of
//   the title since the last asking of the write watch (looked at once the
//   pages are closed);
// - not a piece the title touched first after
//   `VICTORY_COPYBACK_LAZY_DEMOTE` (4) ownerships in a row: the title reads
//   it, and fetching it at each touch costs more than fetching it at once.
//
// The invariants, and where each is kept:
// 1. protect before copy: a piece is noted, then closed, then the write
//    watch asked ([`own`]); a write that came first leaves it eager.
// 2. ownership only after pending CPU bytes are sent: `Shadow::cpu_pending`.
// 3. no upload covers an owned range: a refresh reads title memory before
//    it sends anything up, and that read brings the piece back first (in
//    place: the handler; through the kernel: [`read_again`]; `read_sparse`
//    never takes a closed page for a hole); only one copy holds an owned
//    piece, so no other copy's refresh can skip it as unwritten.
// 4. a CPU write wins: the merge puts only the bytes the card changed
//    (against the copy's CPU side) into title memory; a title write after
//    the fault lands on top, and goes up at the next refresh.
// 5. our code and the runtime see current bytes: the same three ways in.
// 6. remove write, then read, under one lock: a piece is closed with one
//    change (no access) and opened with one, both under [`LAZY`]'s lock,
//    held by the handler's look too.
// 7. no stack or code shares a closed page: only write-watched GPU
//    reservations of the title are closed.
//
// Known limits: a thread that touches an owned page between the moment
// another thread's fault opens it and the end of that fault's write sees
// the bytes from before (there is no second view of private memory to
// write through while it stays closed); the merge of the copy's CPU side
// takes the card's bytes where they differ from it, as the eager merge
// does. With the switch on, the in-flight fetch (`VICTORY_CARD_FETCH_INFLIGHT`)
// is off: lazy needs the relay's own fetch.

/// A 64 KiB piece of title memory, as the card's compare cuts the copies
/// (at multiples of 64 KiB of title memory).
const PIECE: usize = 64 << 10;
/// What a first touch brings back: the owned pieces of the aligned window
/// around it.
const WINDOW: usize = 512 << 10;

/// [`Mode::Lazy`].
pub(crate) fn lazy() -> bool {
    mode() == Mode::Lazy
}

/// Whether memory the title reserved write-combining may be left on the
/// card (`VICTORY_COPYBACK_LAZY_WC=0`: no).
fn lazy_wc() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_COPYBACK_LAZY_WC").is_none_or(|v| v != "0"))
}

/// After how many ownerships in a row that the title touched first a piece
/// stays eager (`VICTORY_COPYBACK_LAZY_DEMOTE`, 4; 0: never).
fn demote_after() -> u32 {
    static N: OnceLock<u32> = OnceLock::new();
    *N.get_or_init(|| std::env::var("VICTORY_COPYBACK_LAZY_DEMOTE").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(4))
}

/// The title's reservations the runtime made (`XMemVirtualAlloc` at an
/// address of its choice), by start: end, whether they are write-watched
/// private memory (not a placeholder for views of physical pages), whether
/// the title asked for write-combining.
static RESERVED: Mutex<BTreeMap<usize, (usize, bool, bool)>> = Mutex::new(BTreeMap::new());

/// `crate::census::title_memory_reserved`: the runtime reserved `[at, at +
/// len)` with the title's `kind` and console `protect`.
pub(crate) fn reserved(at: usize, len: usize, kind: u32, protect: u32) {
    if !lazy() || at == 0 || len == 0 {
        return;
    }
    // As `victory-hle`'s xmem: 64 KiB pages reserved without a commit are
    // a placeholder (physical pages mapped later, views); the rest is
    // reserved with write watching.
    const PAGES_64K: u32 = 0x2040_0000;
    const MEM_COMMIT: u32 = 0x1000;
    const WRITE_COMBINE: u32 = 0x400;
    let placeholder = kind & PAGES_64K == PAGES_64K && kind & MEM_COMMIT == 0;
    RESERVED.lock().unwrap_or_else(|e| e.into_inner()).insert(at, (at + len, !placeholder, protect & WRITE_COMBINE != 0));
}

/// Of title memory `[from, to)`: whether one write-watched reservation
/// holds it, and whether it is write-combining.
fn reservation(from: usize, to: usize) -> Option<bool> {
    let reserved = RESERVED.lock().unwrap_or_else(|e| e.into_inner());
    let (_, &(end, watched, wc)) = reserved.range(..=from).next_back()?;
    (watched && to <= end).then_some(wc)
}

/// With [`lazy`]: how many host copies hold each 64 KiB piece of title
/// memory, by piece number.
static COVER: Mutex<BTreeMap<usize, u32>> = Mutex::new(BTreeMap::new());

/// A host copy of title memory `[start, start + len)` was made
/// (`crate::shadow`).
pub(crate) fn copy_made(start: usize, len: usize) {
    if !lazy() || len == 0 {
        return;
    }
    let mut cover = COVER.lock().unwrap_or_else(|e| e.into_inner());
    for piece in start / PIECE..=(start + len - 1) / PIECE {
        *cover.entry(piece).or_insert(0) += 1;
    }
}

/// A host copy of `[start, start + len)` is gone (or was title memory in
/// place, not a copy).
pub(crate) fn copy_gone(start: usize, len: usize) {
    if !lazy() || len == 0 {
        return;
    }
    let mut cover = COVER.lock().unwrap_or_else(|e| e.into_inner());
    for piece in start / PIECE..=(start + len - 1) / PIECE {
        if let Some(n) = cover.get_mut(&piece) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                cover.remove(&piece);
            }
        }
    }
}

/// Whether exactly one host copy holds the piece at `key`.
fn alone(key: usize) -> bool {
    COVER.lock().unwrap_or_else(|e| e.into_inner()).get(&(key / PIECE)).copied() == Some(1)
}

/// A piece left on the card.
struct Owned {
    /// The copy whose card side holds what shaders wrote; kept alive while
    /// it owns. `None` only in the tests.
    copy: Option<Arc<crate::shadow::Shadow>>,
    /// The offsets of the copy the card may have written there (the
    /// flagged parts of declared ranges, joined over landings).
    lo: usize,
    hi: usize,
    /// Its number among the copy's pieces (`crate::host`'s `BESIDE`).
    number: u32,
    /// The 4 KiB pages of title memory kept closed, `[start, end)`.
    pages: (usize, usize),
    /// The last flight that flagged it, and the host fence value behind it
    /// (reached: it has landed).
    flight: u64,
    fence: u64,
    /// The tests' "card": the bytes the fetch brings for `[lo, hi)`.
    #[cfg(test)]
    fake: Option<Arc<Vec<u8>>>,
}

struct Lazy {
    /// By the title address of the piece (a multiple of [`PIECE`]).
    owned: BTreeMap<usize, Owned>,
    /// Pieces ever closed: a fault there once they are open again is
    /// retried, not passed on.
    ever: BTreeSet<usize>,
    /// Per piece: ownerships in a row that the title touched first; and
    /// the pieces that stay eager.
    streak: BTreeMap<usize, u32>,
    demoted: BTreeSet<usize>,
}

static LAZY: Mutex<Lazy> = Mutex::new(Lazy {
    owned: BTreeMap::new(),
    ever: BTreeSet::new(),
    streak: BTreeMap::new(),
    demoted: BTreeSet::new(),
});
/// Pieces owned now, and whether any ever was: looked at without the lock.
static OWNED_N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static EVER: AtomicBool = AtomicBool::new(false);

fn lazy_state() -> std::sync::MutexGuard<'static, Lazy> {
    LAZY.lock().unwrap_or_else(|e| e.into_inner())
}

/// Who brought owned pieces back (the [`LAZY_COUNT`] places from
/// [`BY_FIRST`]): the title reading, the title writing, a copy of ours in
/// place, other code of ours in place, a read through the kernel, a write
/// into title memory (ours or the runtime's), a system call of the title,
/// a merge or landing of ours, a copy let go.
pub(crate) const BY_READ: usize = 0;
pub(crate) const BY_WRITE: usize = 1;
pub(crate) const BY_OWN_COPY: usize = 2;
pub(crate) const BY_OWN: usize = 3;
pub(crate) const BY_KERNEL: usize = 4;
pub(crate) const BY_WRITING: usize = 5;
pub(crate) const BY_SYSTEM: usize = 6;
pub(crate) const BY_MERGE: usize = 7;
pub(crate) const BY_RELEASED: usize = 8;
const BY_KINDS: usize = 9;

/// Why a flagged piece was copied back as before (the [`LAZY_COUNT`]
/// places from [`EAGER_FIRST`]).
const EAGER_READY: usize = 0;
const EAGER_CLASS: usize = 1;
const EAGER_SMALL: usize = 2;
const EAGER_BACK: usize = 3;
const EAGER_RESERVATION: usize = 4;
const EAGER_WC: usize = 5;
const EAGER_SHARED: usize = 6;
const EAGER_FILL: usize = 7;
const EAGER_DEMOTED: usize = 8;
const EAGER_PENDING: usize = 9;
const EAGER_PROTECT: usize = 10;
const EAGER_WRITTEN: usize = 11;
const EAGER_KINDS: usize = 12;

/// Since the last line: pieces owned and their bytes (left on the card,
/// not fetched), pieces flagged again while owned and their bytes; then
/// first touches (fault or ask that found an owned piece) by who, the
/// pieces they brought back and their bytes, the fetches, their
/// microseconds (fetch and merge) and the longest, the bytes written into
/// title memory, pieces given up without a fetch; then eager pieces by
/// reason.
const OWNED_PIECES: usize = 0;
const OWNED_BYTES: usize = 1;
const AGAIN_PIECES: usize = 2;
const AGAIN_BYTES: usize = 3;
const BY_FIRST: usize = 4;
const BACK_PIECES: usize = BY_FIRST + BY_KINDS;
const BACK_BYTES: usize = BACK_PIECES + 1;
const FETCHES: usize = BACK_BYTES + 1;
const FETCH_US: usize = FETCHES + 1;
const FETCH_MOST_US: usize = FETCH_US + 1;
const MERGED_BYTES: usize = FETCH_MOST_US + 1;
const FORGOTTEN: usize = MERGED_BYTES + 1;
const EAGER_FIRST: usize = FORGOTTEN + 1;
const LAZY_COUNTS: usize = EAGER_FIRST + EAGER_KINDS;
static LAZY_COUNT: [AtomicU64; LAZY_COUNTS] = [const { AtomicU64::new(0) }; LAZY_COUNTS];

fn add(at: usize, n: u64) {
    LAZY_COUNT[at].fetch_add(n, Relaxed);
}

/// The landing a [`take`] is part of.
pub(crate) struct Landing<'a> {
    /// The number of the flight whose compare flagged the piece, and the
    /// host fence value behind the landing's last flight.
    pub(crate) flight: u64,
    pub(crate) fence: u64,
    /// The flight's send of title bytes to the card's copies (its plan's
    /// `sent_as`): bytes sent after it had not reached the card when it ran.
    pub(crate) sent_as: u64,
    /// The title memory texture read-backs of the landing write (in order,
    /// joined): left eager.
    pub(crate) backs: &'a [(usize, usize)],
}

/// At a landing (`crate::host`'s `relay_card_back`), for a piece the card's
/// compare flagged: offsets `[lo, hi)` of `copy` (inside piece `number`),
/// in the declared range `declared` (title addresses). Whether it stays on
/// the card (owned: the caller neither fetches nor merges it); otherwise it
/// is copied back as before.
pub(crate) fn take(
    copy: &Arc<crate::shadow::Shadow>,
    lo: usize,
    hi: usize,
    number: u32,
    declared: (usize, usize),
    landing: &Landing,
) -> bool {
    if !lazy() || lo >= hi {
        return false;
    }
    let start = copy.title_start() as usize;
    let (t0, t1) = (start + lo, start + hi);
    let key = t0 / PIECE * PIECE;
    let facts = Facts {
        ready: crate::host::lazy_ready(),
        card_buffer: copy.carded() && !copy.windowed(),
        // A piece is cut at 64 KiB of title memory: one key.
        one_piece: (t1 - 1) / PIECE * PIECE == key,
        declared: declared.1.saturating_sub(declared.0),
        back: touches(landing.backs, t0, t1),
        reservation: reservation(t0, t1),
        alone: alone(key),
        texture: crate::texture::staged_over(t0, t1 - t0),
        fill: crate::host::fills_read_ahead(t0, t1 - t0),
        demoted: lazy_state().demoted.contains(&key),
    };
    if let Some(why) = refusal(&facts, off_keep(), lazy_wc()) {
        add(EAGER_FIRST + why, 1);
        return false;
    }
    install_lazy();
    copy.with_side(|| own(copy, lo, hi, number, (t0, t1), landing))
}

/// What [`take`] decides on, looked up beforehand.
#[derive(Clone, Copy, Debug)]
struct Facts {
    /// The relay lands flights and fetches on its copy queue.
    ready: bool,
    /// A title buffer's copy in the card's memory, not a window.
    card_buffer: bool,
    one_piece: bool,
    /// The declared range's bytes.
    declared: usize,
    /// A texture read-back of the landing writes there.
    back: bool,
    /// One write-watched reservation holds it: whether write-combining.
    reservation: Option<bool>,
    /// No other host copy holds the piece.
    alone: bool,
    /// A staged texture's memory meets it.
    texture: bool,
    /// A fill read it ahead of flights lately.
    fill: bool,
    demoted: bool,
}

/// Why a piece with `facts` is copied back as before, if it is; `keep` is
/// [`off_keep`], `wc` [`lazy_wc`].
fn refusal(facts: &Facts, keep: usize, wc: bool) -> Option<usize> {
    if !facts.ready {
        return Some(EAGER_READY);
    }
    if !facts.card_buffer || !facts.one_piece {
        return Some(EAGER_CLASS);
    }
    if facts.declared <= keep {
        return Some(EAGER_SMALL);
    }
    if facts.back {
        return Some(EAGER_BACK);
    }
    let Some(write_combining) = facts.reservation else {
        return Some(EAGER_RESERVATION);
    };
    if write_combining && !wc {
        return Some(EAGER_WC);
    }
    if !facts.alone || facts.texture {
        return Some(EAGER_SHARED);
    }
    if facts.fill {
        return Some(EAGER_FILL);
    }
    if facts.demoted {
        return Some(EAGER_DEMOTED);
    }
    None
}

/// Whether `[from, to)` meets one of `ranges` (in order, joined).
fn touches(ranges: &[(usize, usize)], from: usize, to: usize) -> bool {
    let at = ranges.partition_point(|r| r.1 <= from);
    ranges.get(at).is_some_and(|r| r.0 < to)
}

/// The 4 KiB pages that hold title memory `[t0, t1)`.
fn pages_over(t0: usize, t1: usize) -> (usize, usize) {
    (t0 - t0 % KERNEL_PAGE, t1.div_ceil(KERNEL_PAGE) * KERNEL_PAGE)
}

/// [`take`], under the copy's CPU side lock (`Shadow::with_side`), which
/// every merge into the copy holds too.
fn own(copy: &Arc<crate::shadow::Shadow>, lo: usize, hi: usize, number: u32, (t0, t1): (usize, usize), landing: &Landing) -> bool {
    // Invariant 2: title bytes still on their way to the card there.
    if copy.cpu_pending(lo, hi, landing.sent_as) {
        add(EAGER_FIRST + EAGER_PENDING, 1);
        return false;
    }
    let key = t0 / PIECE * PIECE;
    let pages = pages_over(t0, t1);
    let mut l = lazy_state();
    if let Some(o) = l.owned.get_mut(&key) {
        if !o.copy.as_ref().is_some_and(|c| Arc::ptr_eq(c, copy)) {
            add(EAGER_FIRST + EAGER_SHARED, 1);
            return false;
        }
        // Flagged again while owned: the card holds newer bytes, title
        // memory stays closed. Pages of the piece not closed yet are.
        let wider = (o.pages.0.min(pages.0), o.pages.1.max(pages.1));
        if wider != o.pages && !close(wider) {
            add(EAGER_FIRST + EAGER_PROTECT, 1);
            return false;
        }
        o.pages = wider;
        o.lo = o.lo.min(lo);
        o.hi = o.hi.max(hi);
        o.flight = landing.flight;
        o.fence = landing.fence;
        // Owned through a landing untouched: the title did not read it.
        l.streak.remove(&key);
        add(AGAIN_PIECES, 1);
        add(AGAIN_BYTES, (hi - lo) as u64);
        return true;
    }
    // Noted first: a fault from the moment the pages are closed is ours.
    l.ever.insert(key);
    EVER.store(true, Relaxed);
    l.owned.insert(
        key,
        Owned {
            copy: Some(copy.clone()),
            lo,
            hi,
            number,
            pages,
            flight: landing.flight,
            fence: landing.fence,
            #[cfg(test)]
            fake: None,
        },
    );
    if !close(pages) {
        l.owned.remove(&key);
        add(EAGER_FIRST + EAGER_PROTECT, 1);
        return false;
    }
    // Closed: a write of the title from here on faults. One before it may
    // not have reached the copy yet: then the piece stays eager.
    if written_since(pages) {
        open(pages);
        l.owned.remove(&key);
        add(EAGER_FIRST + EAGER_WRITTEN, 1);
        return false;
    }
    OWNED_N.store(l.owned.len(), Relaxed);
    add(OWNED_PIECES, 1);
    add(OWNED_BYTES, (hi - lo) as u64);
    true
}

/// Closes title pages `[pages.0, pages.1)` (no access), if they are
/// committed read-write; whether they are closed now.
fn close(pages: (usize, usize)) -> bool {
    use windows_sys::Win32::System::Memory::{PAGE_NOACCESS, PAGE_READWRITE, VirtualProtect};
    let mut old = 0;
    // SAFETY: committed title memory (the call fails otherwise); only its
    // protection changes, and every way in is ours to open again.
    if unsafe { VirtualProtect(pages.0 as *const _, pages.1 - pages.0, PAGE_NOACCESS, &mut old) } == 0 {
        return false;
    }
    if old != PAGE_READWRITE {
        // Not plain read-write memory (the title's own protection): put
        // back.
        // SAFETY: as above.
        unsafe { VirtualProtect(pages.0 as *const _, pages.1 - pages.0, old, &mut old) };
        return false;
    }
    true
}

/// Opens title pages `[pages.0, pages.1)` again (read-write).
fn open(pages: (usize, usize)) {
    use windows_sys::Win32::System::Memory::{PAGE_READWRITE, VirtualProtect};
    let mut old = 0;
    // SAFETY: pages [`close`] closed; only their protection changes.
    unsafe { VirtualProtect(pages.0 as *const _, pages.1 - pages.0, PAGE_READWRITE, &mut old) };
}

/// Whether the title wrote a page of `[pages.0, pages.1)` since the write
/// watch was last asked (asked without resetting it: the journal's askings
/// still find it). True where the memory is not write-watched.
fn written_since(pages: (usize, usize)) -> bool {
    use windows_sys::Win32::System::Memory::GetWriteWatch;
    let mut found = [std::ptr::null_mut::<std::ffi::c_void>(); PIECE / KERNEL_PAGE];
    let mut count = found.len().min((pages.1 - pages.0) / KERNEL_PAGE);
    let mut granularity = 0;
    // SAFETY: room for `count` answers; nothing is reset.
    let failed = unsafe { GetWriteWatch(0, pages.0 as *const _, pages.1 - pages.0, found.as_mut_ptr(), &mut count, &mut granularity) } != 0;
    failed || count > 0
}

/// Whether a piece of title memory `[at, at + len)` is owned now.
pub(crate) fn owns(at: usize, len: usize) -> bool {
    if OWNED_N.load(Relaxed) == 0 || len == 0 {
        return false;
    }
    let first = at / PIECE * PIECE;
    lazy_state().owned.range(first..at.saturating_add(len)).next().is_some()
}

/// Brings back what the card keeps in title memory `[at, at + len)`
/// before it is read or written, for `why` (a `BY_` place). Whether
/// anything was.
pub(crate) fn release(at: usize, len: usize, why: usize) -> bool {
    if !owns(at, len) {
        return false;
    }
    let first = at / PIECE * PIECE;
    resolve_range(first, at.saturating_add(len), why) > 0
}

/// `crate::kernel_will_read`: a system call is about to read title memory
/// `[at, at + len)` (a file write, a send).
pub(crate) fn kernel_will_read(at: usize, len: usize) {
    if lazy() {
        release(at, len, BY_SYSTEM);
    }
}

/// The title let go of the buffer `copy` is of (`crate::shadow::unregister`):
/// what the card keeps of it comes back now, while its card side is there
/// (the memory may be read as something else).
pub(crate) fn copy_released(copy: &Arc<crate::shadow::Shadow>) {
    if OWNED_N.load(Relaxed) == 0 {
        return;
    }
    let keys: Vec<usize> = lazy_state()
        .owned
        .iter()
        .filter(|(_, o)| o.copy.as_ref().is_some_and(|c| Arc::ptr_eq(c, copy)))
        .map(|(&k, _)| k)
        .collect();
    for key in keys {
        resolve_range(key, key + PIECE, BY_RELEASED);
    }
}

/// Pieces owned in `[at, at + len)` are given up without a fetch (the
/// memory shows something else now: a mapping changed).
fn forget(at: usize, len: usize) {
    if OWNED_N.load(Relaxed) == 0 || len == 0 {
        return;
    }
    let first = at / PIECE * PIECE;
    let mut l = lazy_state();
    let keys: Vec<usize> = l.owned.range(first..at.saturating_add(len)).map(|(&k, _)| k).collect();
    for key in keys {
        // (What the addresses show now is read-write, the runtime says.)
        if let Some(o) = l.owned.remove(&key) {
            open(o.pages);
        }
        add(FORGOTTEN, 1);
    }
    OWNED_N.store(l.owned.len(), Relaxed);
}

/// The tests' landing: title memory `[t0, t1)` (inside one piece) is left
/// on a "card" that holds `card` there, as [`own`] leaves a piece.
#[cfg(test)]
fn own_fake(t0: usize, t1: usize, card: Vec<u8>, flight: u64) -> bool {
    assert_eq!(card.len(), t1 - t0);
    install_lazy();
    let key = t0 / PIECE * PIECE;
    let pages = pages_over(t0, t1);
    let mut l = lazy_state();
    l.ever.insert(key);
    EVER.store(true, Relaxed);
    l.owned.insert(key, Owned { copy: None, lo: t0, hi: t1, number: 0, pages, flight, fence: 0, fake: Some(Arc::new(card)) });
    if !close(pages) {
        l.owned.remove(&key);
        return false;
    }
    OWNED_N.store(l.owned.len(), Relaxed);
    true
}

thread_local! {
    /// This thread is bringing pieces back: a fault inside that is not one
    /// to take again (it would wait for itself).
    static RESOLVING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The last address this thread faulted at on an open piece that was
    /// once owned, and how many times in a row.
    static RETRIED: std::cell::Cell<(usize, u32)> = const { std::cell::Cell::new((0, 0)) };
}

/// One bring-back at a time: the fetch buffer is one.
static RESOLVER: Mutex<()> = Mutex::new(());

/// A piece being brought back: its key, copy, offsets, number, flight and
/// fence.
type Back = (usize, Option<Arc<crate::shadow::Shadow>>, usize, usize, u32, u64, u64);

/// Brings back every owned piece whose key lies in `[from, to)`: their
/// bytes are fetched from the card's sides through the relay's copy queue
/// (behind their producers' fences, reached already, and nothing else),
/// what shaders wrote goes into the copies' CPU sides and title memory,
/// and the pages are opened, each piece under its copy's side lock and
/// [`LAZY`]'s. A piece flagged again by a landing meanwhile is fetched
/// again. Returns the pieces brought back.
fn resolve_range(from: usize, to: usize, why: usize) -> usize {
    if RESOLVING.with(std::cell::Cell::get) {
        return 0;
    }
    RESOLVING.with(|r| r.set(true));
    let started = std::time::Instant::now();
    let one = RESOLVER.lock().unwrap_or_else(|e| e.into_inner());
    let mut done = 0;
    for _ in 0..4 {
        let backs: Vec<Back> = {
            let l = lazy_state();
            l.owned.range(from / PIECE * PIECE..to).map(|(&k, o)| (k, o.copy.clone(), o.lo, o.hi, o.number, o.flight, o.fence)).collect()
        };
        if backs.is_empty() {
            break;
        }
        let Some((bytes, after)) = fetch(&backs) else {
            // The card cannot be read (removed): the pages open on what
            // title memory holds, said once.
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| tracing::error!("VICTORY_COPYBACK=lazy: the card's bytes could not be fetched; title memory keeps older bytes"));
            let mut l = lazy_state();
            for b in &backs {
                if let Some(o) = l.owned.remove(&b.0) {
                    open(o.pages);
                }
            }
            OWNED_N.store(l.owned.len(), Relaxed);
            break;
        };
        add(FETCHES, 1);
        let mut at = 0;
        let mut again = false;
        for (key, copy, lo, hi, _, flight, _) in &backs {
            let gpu = &bytes[at..at + (hi - lo)];
            at += hi - lo;
            let merged = match copy {
                Some(copy) => copy.with_side(|| merge(*key, Some(copy), *lo, gpu, after, *flight)),
                None => merge(*key, None, *lo, gpu, after, *flight),
            };
            match merged {
                Some(written) => {
                    done += 1;
                    add(BACK_PIECES, 1);
                    add(BACK_BYTES, (hi - lo) as u64);
                    add(MERGED_BYTES, written as u64);
                }
                None => again = true,
            }
        }
        if !again {
            break;
        }
    }
    drop(one);
    RESOLVING.with(|r| r.set(false));
    if done > 0 {
        add(BY_FIRST + why, 1);
        let us = started.elapsed().as_micros() as u64;
        add(FETCH_US, us);
        LAZY_COUNT[FETCH_MOST_US].fetch_max(us, Relaxed);
    }
    done
}

/// The fetch of [`resolve_range`]: the pieces' bytes one after the other,
/// and the last send of title bytes the card had finished before.
fn fetch(backs: &[Back]) -> Option<(Vec<u8>, u64)> {
    #[cfg(test)]
    if backs.iter().all(|b| b.1.is_none()) {
        let l = lazy_state();
        let bytes = backs.iter().flat_map(|b| l.owned.get(&b.0).and_then(|o| o.fake.clone()).map_or_else(|| vec![0; b.3 - b.2], |f| f.to_vec())).collect();
        return Some((bytes, 0));
    }
    let pieces: Vec<(Arc<crate::shadow::Shadow>, usize, usize, u32)> =
        backs.iter().filter_map(|b| b.1.clone().map(|c| (c, b.2, b.3, b.4))).collect();
    if pieces.len() != backs.len() {
        return None;
    }
    let behind = backs.iter().map(|b| b.6).max().unwrap_or(0);
    crate::host::lazy_fetch(&pieces, behind)
}

/// The merge of one piece brought back (`gpu`: the card side's bytes at
/// offset `lo` of `copy`, fetched after send `after`), under the copy's
/// side lock: unless a landing flagged it again since `flight` (then
/// `None`: fetched again), what shaders wrote goes into the CPU side and
/// title memory, and the pages open. The bytes written into title memory.
fn merge(key: usize, copy: Option<&Arc<crate::shadow::Shadow>>, lo: usize, gpu: &[u8], after: u64, flight: u64) -> Option<usize> {
    let mut l = lazy_state();
    let Some(o) = l.owned.get(&key) else {
        // Brought back by another way meanwhile.
        return Some(0);
    };
    if o.flight != flight || o.lo != lo || o.hi != lo + gpu.len() {
        return None;
    }
    let pages = o.pages;
    // (The tests' pieces have no copy: their offsets are title addresses.)
    let start = copy.map_or(0, |c| c.title_start() as usize);
    // The bytes shaders wrote: those the card changed against the CPU side
    // (which takes them), outside title bytes the card had not received.
    let runs = match copy {
        Some(copy) => copy.lazy_runs(lo, gpu, after),
        None => vec![(0, gpu.len())],
    };
    // Invariant 6: one change opens the pages, under the lock the handler's
    // look takes.
    open(pages);
    let mut written = 0;
    for (off, n) in runs {
        crate::host::write_mapped((start + lo + off) as *mut u8, gpu[off..].as_ptr(), n);
        written += n;
    }
    if written > 0 {
        // A placed texture there changed with no descriptor written.
        crate::descriptor::host::remapped(start + lo, gpu.len());
    }
    l.owned.remove(&key);
    OWNED_N.store(l.owned.len(), Relaxed);
    Some(written)
}

/// The title touched an owned piece first: its streak grows, and past
/// [`demote_after`] it stays eager.
fn touched_by_title(key: usize) {
    let n = demote_after();
    if n == 0 {
        return;
    }
    let mut l = lazy_state();
    let streak = l.streak.entry(key).or_insert(0);
    *streak += 1;
    if *streak >= n {
        l.streak.remove(&key);
        l.demoted.insert(key);
    }
}

/// Registers the handler of the closed pages, first in the line (ahead of
/// the copies in place, whose handler would end their copy at a closed
/// page, and the guards'), once.
fn install_lazy() {
    use windows_sys::Win32::System::Diagnostics::Debug::AddVectoredExceptionHandler;
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // Looked up here, not in the handler.
        let _ = images();
        // The copies in place register at their first copy: made now, so
        // that this handler, registered after, runs first.
        static PROBE: u8 = 0;
        crate::plaincopy::touch(&raw const PROBE as usize);
        // SAFETY: the handler reads the record and context it is given and
        // touches only the owned pieces' state and protections.
        unsafe { AddVectoredExceptionHandler(1, Some(on_access)) };
    });
}

/// The handler of the closed pages: an access violation in an owned piece
/// brings back the owned pieces of the window around it, then lets the
/// instruction run again.
unsafe extern "system" fn on_access(info: *mut windows_sys::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS) -> i32 {
    use windows_sys::Win32::Foundation::{EXCEPTION_ACCESS_VIOLATION, NTSTATUS};
    use windows_sys::Win32::System::Diagnostics::Debug::{EXCEPTION_CONTINUE_EXECUTION, EXCEPTION_CONTINUE_SEARCH};
    // SAFETY: the system passes the exception being dispatched and the
    // context it will continue with.
    let (r, context) = unsafe { (&*(*info).ExceptionRecord, &*(*info).ContextRecord) };
    if r.ExceptionCode != EXCEPTION_ACCESS_VIOLATION as NTSTATUS || r.NumberParameters < 2 || !EVER.load(Relaxed) {
        return EXCEPTION_CONTINUE_SEARCH;
    }
    const WRITE: usize = 1;
    const EXECUTE: usize = 8;
    let kind = r.ExceptionInformation[0];
    let at = r.ExceptionInformation[1];
    if kind == EXECUTE || RESOLVING.with(std::cell::Cell::get) {
        return EXCEPTION_CONTINUE_SEARCH;
    }
    let key = at / PIECE * PIECE;
    let (owned, ever) = {
        let l = lazy_state();
        (l.owned.contains_key(&key), l.ever.contains(&key))
    };
    if !owned {
        // Opened since the fault (another thread brought it back): run the
        // instruction again, once or twice; a page closed for another
        // reason is not ours.
        if !ever {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        let (last, n) = RETRIED.with(std::cell::Cell::get);
        let n = if last == at { n + 1 } else { 1 };
        RETRIED.with(|c| c.set((at, n)));
        return if n <= 2 { EXCEPTION_CONTINUE_EXECUTION } else { EXCEPTION_CONTINUE_SEARCH };
    }
    let (own, foreign) = images();
    // SAFETY: the faulting thread's context, on that thread.
    let who = who(context.Rip as usize, *own, foreign, crate::plaincopy::copy_instruction(), || unsafe { caller(context, foreign) });
    let why = match who {
        Who::OwnCopy => BY_OWN_COPY,
        Who::Own => BY_OWN,
        Who::Title if kind == WRITE => BY_WRITE,
        Who::Title => BY_READ,
    };
    let w0 = at / WINDOW * WINDOW;
    resolve_range(w0, w0 + WINDOW, why);
    if who == Who::Title {
        touched_by_title(key);
    }
    RETRIED.with(|c| c.set((0, 0)));
    EXCEPTION_CONTINUE_EXECUTION
}

/// The per-second line of [`lazy`], over `frames`.
fn lazy_line(frames: f64) {
    let c: Vec<u64> = LAZY_COUNT.iter().map(|c| c.swap(0, Relaxed)).collect();
    let f = |i: usize| c[i] as f64 / frames;
    let mb = |i: usize| c[i] as f64 / 1e6 / frames;
    let (owned_now, owned_bytes, demoted) = {
        let l = lazy_state();
        (l.owned.len(), l.owned.values().map(|o| (o.hi - o.lo) as u64).sum::<u64>(), l.demoted.len())
    };
    let touches: u64 = c[BY_FIRST..BY_FIRST + BY_KINDS].iter().sum();
    tracing::info!(
        "copy-back lazy (VICTORY_COPYBACK=lazy) per frame: {:.1} pieces left on the card ({:.2} MB not fetched), {:.1} flagged \
         again while owned ({:.2} MB); first touches {:.2} (title reads {:.2}, title writes {:.2}, our copies in place {:.2}, \
         our code in place {:.2}, kernel reads {:.2}, writes into title memory {:.2}, system calls {:.2}, our merges {:.2}, \
         copies let go {:.2}) bringing back {:.1} pieces ({:.2} MB fetched, {:.2} MB written) in {:.2} fetches, {:.3} ms \
         (longest {:.3} ms since the last line); {:.1} given up unfetched; eager instead: not ready {:.1}, not a card buffer \
         {:.1}, small {:.1}, texture back {:.1}, not write-watched {:.1}, write-combining {:.1}, other copies or textures \
         {:.1}, fills ahead {:.1}, read by the title {:.1}, title bytes on their way {:.1}, protection refused {:.1}, \
         written since asked {:.1}; {owned_now} pieces owned now ({:.1} MB), {demoted} pieces demoted; {:.0} first touches a \
         second",
        f(OWNED_PIECES),
        mb(OWNED_BYTES),
        f(AGAIN_PIECES),
        mb(AGAIN_BYTES),
        touches as f64 / frames,
        f(BY_FIRST + BY_READ),
        f(BY_FIRST + BY_WRITE),
        f(BY_FIRST + BY_OWN_COPY),
        f(BY_FIRST + BY_OWN),
        f(BY_FIRST + BY_KERNEL),
        f(BY_FIRST + BY_WRITING),
        f(BY_FIRST + BY_SYSTEM),
        f(BY_FIRST + BY_MERGE),
        f(BY_FIRST + BY_RELEASED),
        f(BACK_PIECES),
        mb(BACK_BYTES),
        mb(MERGED_BYTES),
        f(FETCHES),
        c[FETCH_US] as f64 / 1e3 / frames,
        c[FETCH_MOST_US] as f64 / 1e3,
        f(FORGOTTEN),
        f(EAGER_FIRST + EAGER_READY),
        f(EAGER_FIRST + EAGER_CLASS),
        f(EAGER_FIRST + EAGER_SMALL),
        f(EAGER_FIRST + EAGER_BACK),
        f(EAGER_FIRST + EAGER_RESERVATION),
        f(EAGER_FIRST + EAGER_WC),
        f(EAGER_FIRST + EAGER_SHARED),
        f(EAGER_FIRST + EAGER_FILL),
        f(EAGER_FIRST + EAGER_DEMOTED),
        f(EAGER_FIRST + EAGER_PENDING),
        f(EAGER_FIRST + EAGER_PROTECT),
        f(EAGER_FIRST + EAGER_WRITTEN),
        owned_bytes as f64 / 1e6,
        touches as f64,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The facts a landing looks up, and which of them leaves a piece
    /// eager: the first that applies, in the order the line names them.
    #[test]
    fn a_piece_stays_eager_for_the_first_reason_that_applies() {
        let ok = Facts {
            ready: true,
            card_buffer: true,
            one_piece: true,
            declared: 64 << 10,
            back: false,
            reservation: Some(false),
            alone: true,
            texture: false,
            fill: false,
            demoted: false,
        };
        assert_eq!(refusal(&ok, 256, true), None);
        let with = |f: fn(&mut Facts)| {
            let mut facts = ok;
            f(&mut facts);
            refusal(&facts, 256, true)
        };
        assert_eq!(with(|f| f.ready = false), Some(EAGER_READY));
        assert_eq!(with(|f| f.card_buffer = false), Some(EAGER_CLASS));
        assert_eq!(with(|f| f.one_piece = false), Some(EAGER_CLASS));
        assert_eq!(with(|f| f.declared = 256), Some(EAGER_SMALL));
        assert_eq!(with(|f| f.declared = 257), None);
        assert_eq!(with(|f| f.back = true), Some(EAGER_BACK));
        assert_eq!(with(|f| f.reservation = None), Some(EAGER_RESERVATION));
        assert_eq!(with(|f| f.alone = false), Some(EAGER_SHARED));
        assert_eq!(with(|f| f.texture = true), Some(EAGER_SHARED));
        assert_eq!(with(|f| f.fill = true), Some(EAGER_FILL));
        assert_eq!(with(|f| f.demoted = true), Some(EAGER_DEMOTED));
        // Write-combining memory: lazy unless VICTORY_COPYBACK_LAZY_WC=0.
        let wc = Facts { reservation: Some(true), ..ok };
        assert_eq!(refusal(&wc, 256, true), None);
        assert_eq!(refusal(&wc, 256, false), Some(EAGER_WC));
        // Not ready comes first, whatever else holds.
        let none = Facts { ready: false, card_buffer: false, declared: 0, ..ok };
        assert_eq!(refusal(&none, 256, true), Some(EAGER_READY));
    }

    /// The pages closed for a piece's bytes, and ranges that meet.
    #[test]
    fn closed_pages_cover_the_bytes_and_ranges_meet() {
        assert_eq!(pages_over(0x1_0010, 0x1_0020), (0x1_0000, 0x1_1000));
        assert_eq!(pages_over(0x1_0000, 0x1_1000), (0x1_0000, 0x1_1000));
        assert_eq!(pages_over(0x1_0fff, 0x1_1001), (0x1_0000, 0x1_2000));
        let ranges = [(0x100, 0x200), (0x400, 0x500)];
        assert!(touches(&ranges, 0x1ff, 0x300));
        assert!(!touches(&ranges, 0x200, 0x400));
        assert!(touches(&ranges, 0x0, 0x1000));
        assert!(!touches(&[], 0, 10));
    }

    /// Reservations: write-watched private memory may be closed, a
    /// placeholder for views may not; write-combining is told apart.
    #[test]
    fn only_write_watched_reservations_hold_pieces() {
        {
            let mut r = RESERVED.lock().unwrap_or_else(|e| e.into_inner());
            r.insert(0x7_0000_0000, (0x7_1000_0000, true, false));
            r.insert(0x7_2000_0000, (0x7_3000_0000, true, true));
            r.insert(0x7_4000_0000, (0x7_5000_0000, false, false));
        }
        assert_eq!(reservation(0x7_0000_1000, 0x7_0001_0000), Some(false));
        assert_eq!(reservation(0x7_2000_1000, 0x7_2001_0000), Some(true));
        assert_eq!(reservation(0x7_4000_1000, 0x7_4001_0000), None);
        // Past a reservation's end, or below the first.
        assert_eq!(reservation(0x7_0fff_ff00, 0x7_1000_0100), None);
        assert_eq!(reservation(0x6_ffff_0000, 0x6_ffff_1000), None);
    }

    /// One test for the closed pages (they share the owned pieces): a piece
    /// left on the "card" is brought back by the first read in place (the
    /// whole window), a write of the title lands on the card's bytes and
    /// wins, a kernel read fails until the piece is brought back, a write
    /// told before it lands brings it back first, a mapping change gives it
    /// up, and a piece the title keeps touching first stays eager.
    #[test]
    fn owned_pieces_through_their_lives() {
        use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
        use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc, VirtualFree};
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        const MEM_WRITE_WATCH: u32 = 0x20_0000;
        // Two windows of our own write-watched memory (one more to align
        // them), a pattern in it.
        let len = 2 * WINDOW;
        // SAFETY: memory of the test's own.
        let reserved = unsafe { VirtualAlloc(std::ptr::null(), len + WINDOW, MEM_RESERVE | MEM_COMMIT | MEM_WRITE_WATCH, PAGE_READWRITE) } as usize;
        assert_ne!(reserved, 0);
        let base = reserved.next_multiple_of(WINDOW);
        let title = |at: usize| (at as u32).wrapping_mul(0x9E37_79B1);
        for at in (0..len).step_by(4) {
            // SAFETY: inside the allocation.
            unsafe { ((base + at) as *mut u32).write_volatile(title(at)) };
        }
        let gpu = |at: usize| title(at) ^ 0xFFFF_0000;
        let card = |t0: usize, t1: usize| -> Vec<u8> { (t0..t1).step_by(4).flat_map(|at| gpu(at - base).to_le_bytes()).collect() };
        // SAFETY: inside the allocation.
        let read = |at: usize| unsafe { ((base + at) as *const u32).read_volatile() };
        // Two pieces of the first window and one of the second.
        let (a, b, c) = (base + PIECE + 0x100, base + 3 * PIECE, base + WINDOW + 2 * PIECE);
        assert!(own_fake(a, a + 0x2000, card(a, a + 0x2000), 1));
        assert!(own_fake(b, b + PIECE, card(b, b + PIECE), 1));
        assert!(own_fake(c, c + 0x1000, card(c, c + 0x1000), 1));
        assert!(owns(a, 1) && owns(b + 100, 4) && owns(c, 0x1000));
        assert!(!owns(base, PIECE));
        // A read in place on another thread faults: the window comes back,
        // the card's bytes in it, the title's around them.
        let got = std::thread::spawn(move || {
            // SAFETY: inside the allocation.
            unsafe { ((base + PIECE + 0x200) as *const u32).read_volatile() }
        })
        .join()
        .unwrap();
        assert_eq!(got, gpu(PIECE + 0x200));
        assert!(!owns(a, 1) && !owns(b, PIECE), "the whole window came back");
        assert!(owns(c, 1), "not the next window");
        assert_eq!(read(PIECE + 0x100), gpu(PIECE + 0x100));
        assert_eq!(read(PIECE + 0x2100 - 4), gpu(PIECE + 0x2100 - 4));
        assert_eq!(read(PIECE + 0x2100), title(PIECE + 0x2100), "past the piece's bytes: the title's");
        assert_eq!(read(PIECE), title(PIECE));
        assert_eq!(read(3 * PIECE + 0x8000), gpu(3 * PIECE + 0x8000));
        // A write of the title: the piece comes back first, the write
        // lands on top and wins.
        // SAFETY: inside the allocation.
        unsafe { ((c + 8) as *mut u32).write_volatile(0x1234_5678) };
        assert!(!owns(c, 1));
        assert_eq!(read(c + 8 - base), 0x1234_5678);
        assert_eq!(read(c + 4 - base), gpu(c + 4 - base));
        // A kernel read of a closed page fails, and reads once the piece
        // is brought back (as `read_again` does).
        assert!(own_fake(c, c + 0x1000, card(c, c + 0x1000), 2));
        let mut out = [0u8; 16];
        let mut done = 0;
        // SAFETY: our own memory, read into a buffer of ours of that size.
        let mut kernel = || unsafe { ReadProcessMemory(GetCurrentProcess(), (c + 16) as *const _, out.as_mut_ptr().cast(), 16, &mut done) != 0 };
        assert!(!kernel(), "a kernel read of a closed page");
        assert!(release(c + 16, 16, BY_KERNEL) && kernel());
        assert_eq!(u32::from_le_bytes([out[0], out[1], out[2], out[3]]), gpu(c + 16 - base));
        // A write told before it lands (`crate::watch::writing`) brings it
        // back first; a second release finds nothing.
        assert!(own_fake(b, b + PIECE, card(b, b + PIECE), 3));
        assert!(release(b + 0x10, 0x20, BY_WRITING));
        assert!(!release(b, PIECE, BY_WRITING));
        assert_eq!(read(3 * PIECE), gpu(3 * PIECE));
        // A mapping change gives the piece up, opened, unfetched.
        for at in (0..PIECE).step_by(4) {
            // SAFETY: inside the allocation.
            unsafe { ((b + at) as *mut u32).write_volatile(title(3 * PIECE + at)) };
        }
        assert!(own_fake(b, b + PIECE, card(b, b + PIECE), 4));
        forget(b, PIECE);
        assert!(!owns(b, PIECE));
        assert_eq!(read(3 * PIECE + 4), title(3 * PIECE + 4), "given up: nothing fetched");
        // A piece the title touches first after each ownership stays eager.
        let key = 0x7_7700_0000;
        for _ in 1..demote_after() {
            touched_by_title(key);
        }
        assert!(demote_after() == 0 || !lazy_state().demoted.contains(&key));
        touched_by_title(key);
        assert!(demote_after() == 0 || lazy_state().demoted.contains(&key));
        // SAFETY: the test's own reservation.
        unsafe { VirtualFree(reserved as *mut _, 0, MEM_RELEASE) };
    }

    /// Off keeps only small ranges; otherwise everything is copied back.
    #[test]
    fn off_keeps_only_small_ranges() {
        assert!(keeps(false, 0, 1 << 30));
        assert!(keeps(true, 256, 256));
        assert!(keeps(true, 256, 4));
        assert!(!keeps(true, 256, 257));
        assert!(!keeps(true, 0, 1));
        // Unset, the mode copies back as before.
        assert!(std::env::var_os("VICTORY_COPYBACK").is_some() || (!off() && copied_back(0x1000, 0x9000)));
    }

    /// A page is in one block, its bytes counted from the range.
    #[test]
    fn pages_and_overlaps() {
        assert_eq!(pages_of(4096, 1), 1..=1);
        assert_eq!(pages_of(4095, 2), 0..=1);
        assert_eq!(pages_of(0, 3 * 4096), 0..=2);
        assert_eq!(overlap(100, 50, 0), 50);
        assert_eq!(overlap(4000, 200, 0), 96);
        assert_eq!(overlap(4000, 200, 4096), 104);
        assert_eq!(overlap(4000, 200, 8192), 0);
    }

    /// Who touched a page, by the faulting instruction.
    #[test]
    fn who_is_told_by_the_instruction() {
        let own = (0x1000, 0x9000);
        let system = [(0x10_0000, 0x20_0000)];
        let never = || -> Option<usize> { panic!("no system code here") };
        assert_eq!(who(0x5000, own, &system, 0x5000, never), Who::OwnCopy);
        assert_eq!(who(0x5001, own, &system, 0x5000, never), Who::Own);
        assert_eq!(who(0x50_0000, own, &system, 0x5000, never), Who::Title);
        // A system library's code: by its caller.
        assert_eq!(who(0x15_0000, own, &system, 0x5000, || Some(0x2000)), Who::Own);
        assert_eq!(who(0x15_0000, own, &system, 0x5000, || Some(0x50_0000)), Who::Title);
        assert_eq!(who(0x15_0000, own, &system, 0x5000, || None), Who::Title);
        // No copy instruction known.
        assert_eq!(who(0, own, &system, 0, || None), Who::Title);
    }

    /// This executable's image holds this code.
    #[test]
    fn our_image_holds_our_code() {
        let ((a, b), foreign) = images();
        let here = our_image_holds_our_code as *const () as usize;
        assert!(*a <= here && here < *b, "{a:#x}..{b:#x} {here:#x}");
        assert!(!foreign.is_empty());
    }

    /// One test for the guards (they share the watched pages): a page
    /// guarded, read by our own code (watched on, its guard put back), read
    /// through the kernel (tried again, watched on), let go by a merge;
    /// another written over; another expired.
    #[test]
    fn watched_pages_through_their_lives() {
        use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
        use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc, VirtualFree};
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        // Three blocks of our own, one page of each watched.
        // SAFETY: memory of the test's own.
        let base = unsafe { VirtualAlloc(std::ptr::null(), 3 * BLOCK, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE) } as usize;
        assert_ne!(base, 0);
        let (a, b, c) = (base + KERNEL_PAGE, base + BLOCK + 2 * KERNEL_PAGE, base + 2 * BLOCK);
        let state = |p: usize| observed().pages.get(&p).map(|w| w.state);
        let guarded = |p: usize| protection(p).is_some_and(|(_, on)| on);
        arm(&[(a, 4096, false), (a + KERNEL_PAGE, 4096, false), (b, 100, true), (c, 8, false)]);
        // One page a block: the second page of a's block is not watched.
        assert_eq!(state(a), Some(State::Armed));
        assert_eq!(state(a + KERNEL_PAGE), None);
        assert!(guarded(a) && guarded(b) && guarded(c) && !guarded(a + KERNEL_PAGE));
        // Our own read: let through, watched on, the guard put back.
        // SAFETY: committed memory of the test's own.
        let byte = unsafe { std::ptr::read_volatile(a as *const u8) };
        assert_eq!(byte, 0);
        assert_eq!(state(a), Some(State::Rearm));
        assert!(!guarded(a));
        after_own_copy();
        assert_eq!(state(a), Some(State::Armed));
        assert!(guarded(a));
        // The kernel's read fails on the guard once; tried again, it reads.
        let mut out = vec![0u8; 2 * KERNEL_PAGE];
        let mut done = 0;
        // SAFETY: our own memory, read into a buffer of ours of that size.
        let mut read = || unsafe { ReadProcessMemory(GetCurrentProcess(), (a - KERNEL_PAGE) as *const _, out.as_mut_ptr().cast(), out.len(), &mut done) != 0 };
        // The kernel's read fails on the guard, which stays; read again
        // (the page read in place first), it reads, and the page is
        // watched on.
        assert!(!read(), "a kernel read through a guard");
        assert!(guarded(a));
        assert!(read_through(a - KERNEL_PAGE, 2 * KERNEL_PAGE, read));
        assert_eq!(state(a), Some(State::Rearm));
        rearm();
        assert!(guarded(a));
        // The guards' own change of protection keeps the guard.
        crate::watch::hold_guards(|| {
            use windows_sys::Win32::System::Memory::{PAGE_READONLY, VirtualProtect};
            let mut old = 0;
            // SAFETY: the test's own block.
            assert_ne!(unsafe { VirtualProtect((b - b % BLOCK) as *const _, BLOCK, PAGE_READONLY, &mut old) }, 0);
            guards_again(b - b % BLOCK, b - b % BLOCK + BLOCK, PAGE_READONLY);
        });
        assert!(guarded(b));
        assert_eq!(protection(b).map(|p| p.0), Some(windows_sys::Win32::System::Memory::PAGE_READONLY));
        let before = observed().total;
        // A merge lands on a, the runtime writes over b.
        let_go(a + 10, 20, CARD_AGAIN);
        let_go(b - 5, 10, WRITTEN_OVER);
        assert_eq!(state(a), None);
        assert_eq!(state(b), None);
        assert!(!guarded(a) && !guarded(b));
        let after = observed().total;
        assert_eq!(after.pages[CARD_AGAIN], before.pages[CARD_AGAIN] + 1);
        assert_eq!(after.own[CARD_AGAIN], before.own[CARD_AGAIN] + 1, "a was read by our own code first");
        assert_eq!(after.pages[WRITTEN_OVER], before.pages[WRITTEN_OVER] + 1);
        assert_eq!(after.wc[WRITTEN_OVER], before.wc[WRITTEN_OVER] + 1);
        // c is still watched a moment later, and expires untouched.
        expire_at(now_ms());
        assert_eq!(state(c), Some(State::Armed));
        expire_at(now_ms() + expiry_ms());
        assert_eq!(state(c), None);
        assert!(!guarded(c));
        assert_eq!(observed().total.pages[UNTOUCHED], before.pages[UNTOUCHED] + 1);
        assert_eq!(WATCHED.load(Relaxed), observed().pages.len());
        // SAFETY: the test's own reservation.
        unsafe { VirtualFree(base as *mut _, 0, MEM_RELEASE) };
    }
}
