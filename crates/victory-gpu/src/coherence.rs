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
//! the kernel, whose read fails once on a guard: [`read_again`]). The
//! guards' changes of protection keep the page's guard
//! ([`protection_set`]), and every change of ours is made under the guards'
//! lock (`crate::watch::hold_guards`). The answer is in the per-second line
//! ([`line`]): the share of the bytes copied back that the title read
//! first.
//!
//! What it costs and risks, with the switch on only: a fault and a change
//! of protection per own read of a watched page (some microseconds, at most
//! [`MOST_ARMED`] pages); a system call of the title that reads or writes a
//! watched page fails once (the kernel's access takes the guard and fails),
//! as one that writes into a guarded block does today.
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
//! them), presents and the flights' landing are as before. It shows the ceiling of stages 3 and 4, and what
//! breaks when the CPU never sees the card's results; expect wrong pictures
//! where the title's CPU writes a 64 KiB piece the card also wrote (the next
//! upload sends the title's stale bytes over the card's).
//! `VICTORY_COPYBACK=lazy` and `=predict` are not in this build yet: they
//! copy back as before, and say so.

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

/// The kernel's page: the unit a guard covers.
const KERNEL_PAGE: usize = 4096;
/// At most one watched page per such block: a kernel read of title memory
/// that fails is tried again by 64 KiB pieces, then by 4 KiB pages
/// (`crate::host`'s `read_pieces`), so one guard a block is always got past.
const BLOCK: usize = 64 << 10;
/// The most pages watched at once.
pub(crate) const MOST_ARMED: usize = 256;

/// What shaders write is copied back as before ([`Mode::Eager`]), or not
/// ([`Mode::Off`], a measurement mode).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Mode {
    Eager,
    Off,
}

/// `VICTORY_COPYBACK`: unset (or `eager`) copies back as before; `off`, a
/// measurement mode ([`off`]). Read once.
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
            "" | "eager" => Mode::Eager,
            other => {
                tracing::warn!("VICTORY_COPYBACK={other} is not in this build (lazy and predict are planned): copied back as before");
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
    *WAIT.get_or_init(|| !off() || crate::switch::on("VICTORY_TEXTURE_BACK"))
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
}

/// The runtime mapped or unmapped title memory `[at, at + len)`: the
/// watched pages there show other memory now.
pub(crate) fn remapped(at: usize, len: usize) {
    if observing() {
        let_go(at, len, REMAPPED);
    }
}

/// The kernel's read of title memory `[src, src + len)` failed. A guard of
/// ours there fails every such read, and stays (the kernel does not take
/// it): the watched pages there are read in place first, which takes their
/// guards (our handler lets that through), counted as read by our own code
/// through the kernel and watched on (the guards come back at the next
/// [`rearm`]); then the read is tried again with `again`. Whether it read.
pub(crate) fn read_again(src: usize, len: usize, again: impl FnMut() -> bool) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

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
