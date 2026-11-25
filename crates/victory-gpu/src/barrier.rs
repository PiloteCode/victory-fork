//! The barrier pass (`VICTORY_BARRIER_OPT`): which of the title's enhanced
//! barrier calls record a wait into the host list, and what they declare,
//! counted per frame (diagnostics).
//!
//! Every call of the title's enhanced barriers (`crate::list`'s
//! `barrier_groups`, about 710 a frame in the gameplay) has been one wait
//! for every earlier shader write, a global unordered access barrier,
//! whatever its barriers say: on NVIDIA each can drain the card. A barrier
//! says what it waits for: `SyncBefore` none waits for nothing before it (a
//! resource's first use, "acquire"); `SyncAfter` none is waited for by
//! nothing after it (an end of use, "release": the first use that follows
//! waits for nothing, so the wait between the two belongs to this one).
//!
//! With `VICTORY_BARRIER_OPT=1` a call records its wait only when that is
//! needed:
//! - a call of first uses only records nothing, as on the console, where
//!   such a barrier does not wait either;
//! - a call of ends of use only defers its wait: the next call that records
//!   one, the next call of first uses, or the end of the host list records
//!   it, whichever comes first (each waits for all the work before it, the
//!   reads the ends of use cover among it);
//! - a call with ends of use and first uses (an alias handed over) waits;
//! - any other barrier waits (after writes, writes after reads, reads after
//!   reads, split barriers, what cannot be read), as before;
//! - a wait right after one of ours with nothing recorded into the host
//!   list between them (no title call reached it: `touch`) is left out:
//!   the first covers it.
//!
//! The title's other barriers (its legacy calls: state changes, aliasing,
//! unordered access) and the layer's own (texture state changes, clamps,
//! copies) are untouched. Off (`0`, the default) every call records its
//! wait as before; the diagnostics count what the switch would do anyway.
//!
//! The groups are read as the PC's enhanced barriers (the title's values
//! match them, see `crate::copystat::barrier_groups_seen`): 16-byte groups
//! (type 0 global, 1 texture, 2 buffer; the count; the barriers), 16-byte
//! global, 64-byte texture and 40-byte buffer barriers, each starting with
//! its sync before and after and its access before and after.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};

use windows::Win32::Graphics::Direct3D12::{
    D3D12_RESOURCE_BARRIER, D3D12_RESOURCE_BARRIER_0, D3D12_RESOURCE_BARRIER_FLAG_NONE,
    D3D12_RESOURCE_BARRIER_TYPE_UAV, D3D12_RESOURCE_UAV_BARRIER, ID3D12GraphicsCommandList,
};

use crate::host::HostList;

/// `D3D12_BARRIER_SYNC_NONE` and `D3D12_BARRIER_SYNC_SPLIT`.
const SYNC_NONE: u32 = 0;
const SYNC_SPLIT: u32 = 0x8000_0000;
/// `D3D12_BARRIER_ACCESS_NO_ACCESS`.
const NO_ACCESS: u32 = 0x8000_0000;
/// The accesses that only read: vertex, constant and index buffers, depth
/// read, shader resource, indirect arguments (and predication), copy and
/// resolve sources, acceleration structure read, shading rate source, video
/// decode, process and encode reads. Anything else (common, render target,
/// unordered access, depth write, stream output, copy and resolve
/// destinations, acceleration structure write, video writes, bits unknown)
/// counts as a write.
const READS: u32 = 0x1
    | 0x2
    | 0x4
    | 0x40
    | 0x80
    | 0x200
    | 0x800
    | 0x2000
    | 0x4000
    | 0x1_0000
    | 0x2_0000
    | 0x8_0000
    | 0x20_0000;

/// Groups and barriers per group read at most; a call past them waits.
const MOST_GROUPS: u32 = 64;
const MOST_BARRIERS: u32 = 4096;

/// Whether `access` may write.
fn writes(access: u32) -> bool {
    access != NO_ACCESS && (access == 0 || access & !READS != 0)
}

/// What one barrier waits for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Waits for nothing before it: a first use.
    First,
    /// Nothing after it waits for it: an end of use.
    Last,
    /// Waits for writes before it.
    AfterWrite,
    /// Writes after reads before it.
    WriteAfterRead,
    /// Reads after reads.
    ReadAfterRead,
    /// A split barrier's half, or a first use or an end of use with an
    /// access on the side that should have none.
    Other,
}

/// What a barrier with these syncs and accesses waits for.
pub(crate) fn kind(sync_before: u32, sync_after: u32, before: u32, after: u32) -> Kind {
    if sync_before == SYNC_SPLIT || sync_after == SYNC_SPLIT {
        return Kind::Other;
    }
    if sync_before == SYNC_NONE {
        return if before == NO_ACCESS {
            Kind::First
        } else {
            Kind::Other
        };
    }
    if sync_after == SYNC_NONE {
        return if after == NO_ACCESS {
            Kind::Last
        } else {
            Kind::Other
        };
    }
    if writes(before) {
        Kind::AfterWrite
    } else if writes(after) {
        Kind::WriteAfterRead
    } else {
        Kind::ReadAfterRead
    }
}

/// One barrier of a call: its group's type (0 global, 1 texture, 2 buffer),
/// what it waits for, and for buffers the resource, offset and size (read
/// by the counts only).
#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(feature = "diagnostics"), allow(dead_code))]
pub(crate) struct Seen {
    pub(crate) group: u32,
    pub(crate) kind: Kind,
    pub(crate) resource: usize,
    pub(crate) offset: u64,
    pub(crate) size: u64,
}

/// What a call declares: its barriers by kind. `readable` false: a group
/// of an unknown type, a null array, or more than this reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Call {
    pub(crate) readable: bool,
    pub(crate) groups: u32,
    pub(crate) barriers: u32,
    pub(crate) first: u32,
    pub(crate) last: u32,
    /// Every other kind: the call waits.
    pub(crate) wait: u32,
}

impl Call {
    fn add(&mut self, kind: Kind) {
        self.barriers += 1;
        match kind {
            Kind::First => self.first += 1,
            Kind::Last => self.last += 1,
            _ => self.wait += 1,
        }
    }
}

/// Reads the `count` groups at `groups` as the title passes them, calling
/// `each` for every barrier.
///
/// # Safety
/// `groups` points at `count` groups as the title passes them (or is null).
pub(crate) unsafe fn read(groups: *const u8, count: u32, mut each: impl FnMut(Seen)) -> Call {
    let mut call = Call {
        readable: true,
        ..Call::default()
    };
    if groups.is_null() || count > MOST_GROUPS {
        call.readable = false;
        return call;
    }
    for g in 0..count as usize {
        // SAFETY: as the caller vouches: 16-byte groups.
        let (group, n, at) = unsafe {
            let group = groups.add(g * 16);
            (
                group.cast::<u32>().read_unaligned(),
                group.add(4).cast::<u32>().read_unaligned(),
                group.add(8).cast::<*const u8>().read_unaligned(),
            )
        };
        let size = match group {
            0 => 16,
            1 => 64,
            2 => 40,
            _ => {
                call.readable = false;
                continue;
            }
        };
        call.groups += 1;
        if n == 0 {
            continue;
        }
        if at.is_null() || n > MOST_BARRIERS {
            call.readable = false;
            continue;
        }
        for b in 0..n as usize {
            // SAFETY: as the caller vouches: `n` barriers of the type's size.
            let word =
                |k: usize| unsafe { at.add(b * size + 4 * k).cast::<u32>().read_unaligned() };
            let kind = kind(word(0), word(1), word(2), word(3));
            call.add(kind);
            let (resource, offset, length) = match group {
                // SAFETY: as above: a buffer barrier's resource, offset and size.
                2 => unsafe {
                    let base = at.add(b * size);
                    (
                        base.add(16).cast::<usize>().read_unaligned(),
                        base.add(24).cast::<u64>().read_unaligned(),
                        base.add(32).cast::<u64>().read_unaligned(),
                    )
                },
                // SAFETY: as above: a texture barrier's resource.
                1 => (
                    unsafe { at.add(b * size + 24).cast::<usize>().read_unaligned() },
                    0,
                    0,
                ),
                _ => (0, 0, 0),
            };
            each(Seen {
                group,
                kind,
                resource,
                offset,
                size: length,
            });
        }
    }
    call
}

/// What a call does to the host list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    /// Records the wait.
    Wait,
    /// Records the wait an earlier call of ends of use deferred (a call of
    /// first uses only).
    Flush,
    /// Nothing: first uses only, nothing deferred.
    First,
    /// Nothing now: ends of use only; the wait is deferred.
    Defer,
    /// Nothing: a wait of ours stands right before (nothing recorded since).
    Adjacent,
    /// Nothing: ends of use only, right after a wait of ours.
    Covered,
}

impl Decision {
    pub(crate) fn records(self) -> bool {
        matches!(self, Decision::Wait | Decision::Flush)
    }
}

/// The pass's view of one host list: whether nothing was recorded since its
/// last wait (`clean`) and whether an end of use's wait is deferred.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct State {
    pub(crate) clean: bool,
    pub(crate) pending: bool,
}

/// Decides call `c` with the switch on, from state `s`, and moves `s` on.
pub(crate) fn decide(s: &mut State, c: &Call) -> Decision {
    let waits = !c.readable || c.barriers == 0 || c.wait > 0 || (c.first > 0 && c.last > 0);
    let decision = if waits {
        if s.clean {
            Decision::Adjacent
        } else {
            Decision::Wait
        }
    } else if c.last > 0 {
        if s.clean {
            Decision::Covered
        } else {
            Decision::Defer
        }
    } else if s.pending {
        // (Pending implies not clean: a wait clears it.)
        Decision::Flush
    } else {
        Decision::First
    };
    match decision {
        Decision::Wait | Decision::Flush => (s.clean, s.pending) = (true, false),
        Decision::Adjacent => s.pending = false,
        Decision::Defer => s.pending = true,
        Decision::First | Decision::Covered => {}
    }
    decision
}

/// The pass's state in one host list (`HostList::barriers`).
#[derive(Default)]
pub(crate) struct Pass {
    /// Bumped by every title call that reaches the host list (`touch`).
    touched: AtomicU32,
    /// `touched` when the pass's last wait was recorded, plus one (0: none
    /// since the list began), and whether a wait is deferred: as recorded,
    /// and as the switch on would have it (diagnostics, for the counts of
    /// a run with the switch off).
    waited: AtomicU64,
    pending: AtomicBool,
    #[cfg(feature = "diagnostics")]
    would_waited: AtomicU64,
    #[cfg(feature = "diagnostics")]
    would_pending: AtomicBool,
    /// The list's deferred steps at the switch-on view's last wait, plus
    /// one (diagnostics: what shaders declared writing since).
    #[cfg(feature = "diagnostics")]
    mark: AtomicU64,
    /// Waits recorded (the tests).
    #[cfg(test)]
    recorded: AtomicU32,
}

impl Pass {
    /// A title call reached the host list: it may record work.
    pub(crate) fn touch(&self) {
        // One thread records a list at a time: no read-modify-write needed.
        self.touched
            .store(self.touched.load(Relaxed).wrapping_add(1), Relaxed);
    }

    fn state(&self, waited: &AtomicU64, pending: &AtomicBool) -> State {
        let at = waited.load(Relaxed);
        State {
            clean: at != 0 && at == u64::from(self.touched.load(Relaxed)) + 1,
            pending: pending.load(Relaxed),
        }
    }

    fn set(&self, waited: &AtomicU64, pending: &AtomicBool, s: State, recorded: bool) {
        if recorded {
            waited.store(u64::from(self.touched.load(Relaxed)) + 1, Relaxed);
        }
        pending.store(s.pending, Relaxed);
    }

    /// The list was reset (or made): nothing recorded, nothing deferred.
    pub(crate) fn reset(&self) {
        self.waited.store(0, Relaxed);
        self.pending.store(false, Relaxed);
        #[cfg(feature = "diagnostics")]
        {
            self.would_waited.store(0, Relaxed);
            self.would_pending.store(false, Relaxed);
            self.mark.store(0, Relaxed);
        }
    }

    /// Waits recorded by the pass since the list began (the tests).
    #[cfg(test)]
    pub(crate) fn recorded(&self) -> u32 {
        self.recorded.load(Relaxed)
    }
}

/// The switch: 0 off (every call waits, as before), 1 on.
pub(crate) fn mode() -> u8 {
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| {
        let mode = match std::env::var("VICTORY_BARRIER_OPT").as_deref().map(str::trim) {
            Ok("1") => 1,
            _ => 0,
        };
        if mode != 0 {
            tracing::info!(mode, "the title's barrier calls wait only where they declare a wait (VICTORY_BARRIER_OPT)");
        }
        mode
    })
}

/// Records one wait for every earlier shader write (a global unordered
/// access barrier), as the title's calls always have.
fn record_wait(list: &ID3D12GraphicsCommandList) {
    let barrier = D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_UAV,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            UAV: std::mem::ManuallyDrop::new(D3D12_RESOURCE_UAV_BARRIER {
                pResource: std::mem::ManuallyDrop::new(None),
            }),
        },
    };
    // SAFETY: an open list; the barrier names no resource.
    unsafe { list.ResourceBarrier(&[barrier]) };
}

/// The title's enhanced barrier call (`count` groups at `groups`) in host
/// list `list`, with the switch as set.
///
/// # Safety
/// `groups` points at `count` groups as the title passes them (or is null).
pub(crate) unsafe fn groups(list: &HostList, count: u32, groups: *const u8) {
    // SAFETY: as the caller vouches.
    unsafe { groups_as(list, count, groups, mode()) }
}

/// [`groups`] with the switch at `mode`.
///
/// # Safety
/// As [`groups`].
pub(crate) unsafe fn groups_as(list: &HostList, count: u32, groups: *const u8, mode: u8) {
    let pass = &list.barriers;
    #[cfg(not(feature = "diagnostics"))]
    if mode == 0 {
        record(list);
        pass.set(
            &pass.waited,
            &pass.pending,
            State {
                clean: true,
                pending: false,
            },
            true,
        );
        return;
    }
    #[cfg(feature = "diagnostics")]
    let mut buffers: Vec<Seen> = Vec::new();
    // SAFETY: as the caller vouches.
    let call = unsafe {
        read(groups, count, |_seen| {
            #[cfg(feature = "diagnostics")]
            tally(_seen, &mut buffers);
        })
    };
    let mut s = pass.state(&pass.waited, &pass.pending);
    let decision = if mode == 0 {
        (s.clean, s.pending) = (true, false);
        Decision::Wait
    } else {
        decide(&mut s, &call)
    };
    if decision.records() {
        record(list);
    }
    pass.set(&pass.waited, &pass.pending, s, decision.records());
    #[cfg(feature = "diagnostics")]
    {
        // What the switch on does (or would do, with it off), counted.
        let would = if mode == 0 {
            // (`touched` is one count for both views.)
            let mut w = pass.state(&pass.would_waited, &pass.would_pending);
            let d = decide(&mut w, &call);
            pass.set(&pass.would_waited, &pass.would_pending, w, d.records());
            d
        } else {
            decision
        };
        counted(list, &call, &buffers, would, decision.records());
    }
}

/// Records the wait of [`groups`].
fn record(list: &HostList) {
    record_wait(&list.list);
    #[cfg(test)]
    list.barriers.recorded.fetch_add(1, Relaxed);
}

/// The host list closes: a wait an end of use deferred is recorded now (the
/// first use after it may come in a later list).
pub(crate) fn close(list: &HostList) {
    let pass = &list.barriers;
    let s = pass.state(&pass.waited, &pass.pending);
    if s.pending && !s.clean {
        record(list);
        pass.set(
            &pass.waited,
            &pass.pending,
            State {
                clean: true,
                pending: false,
            },
            true,
        );
        #[cfg(feature = "diagnostics")]
        if mode() != 0 {
            add(N::FlushedAtEnd, 1);
            add(N::Recorded, 1);
        }
    }
    pass.pending.store(false, Relaxed);
    #[cfg(feature = "diagnostics")]
    if mode() == 0 && pass.would_pending.swap(false, Relaxed) {
        let w = pass.state(&pass.would_waited, &pass.would_pending);
        if !w.clean {
            add(N::FlushedAtEnd, 1);
        }
    }
}

/// Per-frame counts (diagnostics), since the last line.
#[cfg(feature = "diagnostics")]
#[derive(Clone, Copy)]
enum N {
    Calls,
    Groups,
    Barriers,
    Global,
    Texture,
    Buffer,
    First,
    Last,
    AfterWrite,
    WriteAfterRead,
    ReadAfterRead,
    Other,
    CallsFirst,
    CallsLast,
    CallsHandover,
    CallsWait,
    CallsUnread,
    /// Calls with nothing recorded since the last wait of the switch-on view.
    CallsAdjacent,
    /// Calls that wait only for writes to buffers that no shader of the
    /// list declared writing since the last wait (the next step's drops).
    CallsNoWrite,
    /// Calls that wait on textures (the next step cannot see their writes).
    CallsTextures,
    /// What the switch on does: waits recorded by calls and at lists' ends;
    /// calls left out.
    WouldWait,
    WouldFlush,
    FlushedAtEnd,
    LeftFirst,
    LeftDefer,
    LeftAdjacent,
    LeftCovered,
    /// Waits recorded for the title's enhanced calls (as they ran).
    Recorded,
    /// The title's legacy barrier calls and their barriers by type.
    LegacyCalls,
    LegacyTransitions,
    LegacyAliasing,
    LegacyUav,
}

#[cfg(feature = "diagnostics")]
const COUNTS: usize = N::LegacyUav as usize + 1;
#[cfg(feature = "diagnostics")]
static COUNT: [AtomicU64; COUNTS] = [const { AtomicU64::new(0) }; COUNTS];

#[cfg(feature = "diagnostics")]
fn add(n: N, v: u64) {
    COUNT[n as usize].fetch_add(v, Relaxed);
}

/// Counts one barrier of a call by type and kind; keeps the buffers that
/// wait for writes, for [`counted`].
#[cfg(feature = "diagnostics")]
fn tally(seen: Seen, buffers: &mut Vec<Seen>) {
    add(
        match seen.group {
            0 => N::Global,
            1 => N::Texture,
            _ => N::Buffer,
        },
        1,
    );
    add(
        match seen.kind {
            Kind::First => N::First,
            Kind::Last => N::Last,
            Kind::AfterWrite => N::AfterWrite,
            Kind::WriteAfterRead => N::WriteAfterRead,
            Kind::ReadAfterRead => N::ReadAfterRead,
            Kind::Other => N::Other,
        },
        1,
    );
    if !matches!(seen.kind, Kind::First | Kind::Last) {
        buffers.push(seen);
    }
}

/// Counts a call: its kind, whether it is adjacent, whether it waits only
/// for buffers no shader of the list declared writing since the last wait,
/// and what the switch on does with it.
#[cfg(feature = "diagnostics")]
fn counted(list: &HostList, call: &Call, waiting: &[Seen], would: Decision, recorded: bool) {
    add(N::Calls, 1);
    add(N::Groups, u64::from(call.groups));
    add(N::Barriers, u64::from(call.barriers));
    let class = if !call.readable {
        N::CallsUnread
    } else if call.wait > 0 || call.barriers == 0 {
        N::CallsWait
    } else if call.first > 0 && call.last > 0 {
        N::CallsHandover
    } else if call.last > 0 {
        N::CallsLast
    } else {
        N::CallsFirst
    };
    add(class, 1);
    if would == Decision::Adjacent || would == Decision::Covered {
        add(N::CallsAdjacent, 1);
    }
    if recorded {
        add(N::Recorded, 1);
    }
    add(
        match would {
            Decision::Wait => N::WouldWait,
            Decision::Flush => N::WouldFlush,
            Decision::First => N::LeftFirst,
            Decision::Defer => N::LeftDefer,
            Decision::Adjacent => N::LeftAdjacent,
            Decision::Covered => N::LeftCovered,
        },
        1,
    );
    if matches!(would, Decision::Wait) && call.readable {
        if waiting.iter().any(|s| s.group == 1) {
            add(N::CallsTextures, 1);
        }
        let mark = list.barriers.mark.load(Relaxed);
        let free = mark != 0
            && !waiting.is_empty()
            && waiting.iter().all(|s| {
                s.group == 2
                    && s.kind == Kind::AfterWrite
                    && crate::resource::buffer_bytes(s.resource as crate::object::This).is_some_and(
                        |(data, size)| {
                            let from = (data as usize).saturating_add(s.offset.min(size) as usize);
                            let len = s.size.min(size.saturating_sub(s.offset)) as usize;
                            !list.writes_declared_since(
                                (from, from.saturating_add(len)),
                                (mark - 1) as usize,
                            )
                        },
                    )
            });
        if free {
            add(N::CallsNoWrite, 1);
        }
    }
    if would.records() {
        list.barriers.mark.store(list.steps() as u64 + 1, Relaxed);
    }
}

/// The layer's own barriers (diagnostics), apart: those around copies of
/// data (title texture copies, card copies of title buffers, the uploads
/// and compares of the card's copies of title memory, the preludes' copies
/// of twins and texture uploads) and those between shader passes (texture
/// state changes for draws and dispatches, the preludes' clamps).
#[cfg(feature = "diagnostics")]
#[derive(Clone, Copy)]
pub(crate) enum Ours {
    /// `crate::list::ensure_state` to a copy or resolve state, and to any
    /// other state.
    CopyStates,
    ShaderStates,
    /// `HostList::copy_on_card`: two global barriers each.
    CardCopy,
    /// The card's copies of title memory: the state change after an upload,
    /// and the compare passes' waits.
    CardUpload,
    CardCompare,
    /// A prelude's state changes into and out of its copies (twins, texture
    /// uploads), and to the states its list starts with.
    PreludeCopy,
    PreludeStates,
    /// A prelude's clamps: a global barrier before them and a buffer
    /// barrier after.
    PreludeClamp,
}

#[cfg(feature = "diagnostics")]
static OURS: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];

/// `n` barriers of ours of kind `what` were recorded (diagnostics).
#[cfg(feature = "diagnostics")]
pub(crate) fn ours(what: Ours, n: u64) {
    OURS[what as usize].fetch_add(n, Relaxed);
}

/// The title's legacy barrier call: its barriers by type (diagnostics).
#[cfg(feature = "diagnostics")]
pub(crate) fn legacy(kinds: impl Iterator<Item = u32>) {
    add(N::LegacyCalls, 1);
    for kind in kinds {
        add(
            match kind {
                0 => N::LegacyTransitions,
                1 => N::LegacyAliasing,
                _ => N::LegacyUav,
            },
            1,
        );
    }
}

/// Logs the counts per frame (`frames` presented since the last line), with
/// the in-list clamps the caller counted (each with its barrier).
#[cfg(feature = "diagnostics")]
pub(crate) fn log(frames: f64, clamps: f64) {
    let c: Vec<f64> = COUNT
        .iter()
        .map(|a| a.swap(0, Relaxed) as f64 / frames.max(1.0))
        .collect();
    let o: Vec<f64> = OURS
        .iter()
        .map(|a| a.swap(0, Relaxed) as f64 / frames.max(1.0))
        .collect();
    let v = |n: N| c[n as usize];
    let w = |n: Ours| o[n as usize];
    tracing::info!(
        "  barrier pass per frame (VICTORY_BARRIER_OPT={}): the title's enhanced barrier calls {:.0} ({:.0} groups, \
         {:.0} barriers: {:.0} global, {:.0} texture, {:.0} buffer); barriers by what they wait for: {:.0} nothing \
         (first uses), {:.0} waited for by nothing (ends of use), {:.0} after writes, {:.0} writes after reads, {:.0} \
         reads after reads, {:.0} split or unread; calls: {:.0} first uses only, {:.0} ends of use only, {:.0} both, \
         {:.0} that wait, {:.0} unread; {:.0} with nothing recorded since the last wait; {:.0} that wait only for \
         buffers no shader of the list declared writing since the last wait, {:.0} that wait on textures; with the \
         switch on: {:.0} waits ({:.0} calls, {:.0} deferred ends of use at a first use, {:.0} at a list's end), \
         left out {:.0} first uses, {:.0} ends of use deferred, {:.0} adjacent, {:.0} ends of use covered; waits \
         recorded for these calls {:.0}; the title's legacy barrier calls {:.0} ({:.0} state changes, {:.0} \
         aliasing, {:.0} unordered access)",
        mode(),
        v(N::Calls),
        v(N::Groups),
        v(N::Barriers),
        v(N::Global),
        v(N::Texture),
        v(N::Buffer),
        v(N::First),
        v(N::Last),
        v(N::AfterWrite),
        v(N::WriteAfterRead),
        v(N::ReadAfterRead),
        v(N::Other),
        v(N::CallsFirst),
        v(N::CallsLast),
        v(N::CallsHandover),
        v(N::CallsWait),
        v(N::CallsUnread),
        v(N::CallsAdjacent),
        v(N::CallsNoWrite),
        v(N::CallsTextures),
        v(N::WouldWait) + v(N::WouldFlush) + v(N::FlushedAtEnd),
        v(N::WouldWait),
        v(N::WouldFlush),
        v(N::FlushedAtEnd),
        v(N::LeftFirst),
        v(N::LeftDefer),
        v(N::LeftAdjacent),
        v(N::LeftCovered),
        v(N::Recorded),
        v(N::LegacyCalls),
        v(N::LegacyTransitions),
        v(N::LegacyAliasing),
        v(N::LegacyUav),
    );
    tracing::info!(
        "  barriers of ours per frame: between shader passes {:.0} ({:.0} texture state changes for draws and \
         dispatches, {:.0} in-list clamps, {:.0} preludes' state changes to their lists' first states, {:.0} \
         preludes' clamp barriers); around copies {:.0} ({:.0} title texture copies' state changes, {:.0} card \
         copies of title buffers, {:.0} after uploads of title memory to the card, {:.0} in the card's compares, \
         {:.0} preludes' twin copies and texture uploads)",
        w(Ours::ShaderStates) + clamps + w(Ours::PreludeStates) + w(Ours::PreludeClamp),
        w(Ours::ShaderStates),
        clamps,
        w(Ours::PreludeStates),
        w(Ours::PreludeClamp),
        w(Ours::CopyStates)
            + w(Ours::CardCopy)
            + w(Ours::CardUpload)
            + w(Ours::CardCompare)
            + w(Ours::PreludeCopy),
        w(Ours::CopyStates),
        w(Ours::CardCopy),
        w(Ours::CardUpload),
        w(Ours::CardCompare),
        w(Ours::PreludeCopy),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_SHADING: u32 = 0x1000;
    const COMPUTE: u32 = 0x80;
    const EXECUTE_INDIRECT: u32 = 0x800;
    const RENDER_TARGET: u32 = 0x40;
    const UAV: u32 = 0x10;
    const SRV: u32 = 0x80;
    const RT: u32 = 0x8;
    const ARGS: u32 = 0x200;

    /// The kinds of the gameplay's most frequent barriers (the tally of
    /// `crate::copystat::barrier_groups_seen`).
    #[test]
    fn the_titles_barriers_wait_for_what_they_declare() {
        assert_eq!(kind(ALL_SHADING, ALL_SHADING, UAV, UAV), Kind::AfterWrite);
        assert_eq!(kind(ALL_SHADING, ALL_SHADING, UAV, SRV), Kind::AfterWrite);
        assert_eq!(kind(SYNC_NONE, ALL_SHADING, NO_ACCESS, UAV), Kind::First);
        assert_eq!(kind(ALL_SHADING, SYNC_NONE, SRV, NO_ACCESS), Kind::Last);
        assert_eq!(
            kind(ALL_SHADING, ALL_SHADING, SRV, UAV),
            Kind::WriteAfterRead
        );
        assert_eq!(
            kind(ALL_SHADING, EXECUTE_INDIRECT, UAV, ARGS),
            Kind::AfterWrite
        );
        assert_eq!(
            kind(EXECUTE_INDIRECT, ALL_SHADING, ARGS, UAV),
            Kind::WriteAfterRead
        );
        assert_eq!(kind(RENDER_TARGET, ALL_SHADING, RT, SRV), Kind::AfterWrite);
        assert_eq!(
            kind(ALL_SHADING, RENDER_TARGET, SRV, RT),
            Kind::WriteAfterRead
        );
        assert_eq!(kind(COMPUTE, COMPUTE, UAV, UAV), Kind::AfterWrite);
        assert_eq!(
            kind(ALL_SHADING, ALL_SHADING, SRV, ARGS),
            Kind::ReadAfterRead
        );
        // Common access may write; split halves and malformed first and last
        // uses wait.
        assert_eq!(kind(ALL_SHADING, ALL_SHADING, 0, SRV), Kind::AfterWrite);
        assert_eq!(kind(SYNC_SPLIT, ALL_SHADING, NO_ACCESS, UAV), Kind::Other);
        assert_eq!(kind(ALL_SHADING, SYNC_SPLIT, UAV, NO_ACCESS), Kind::Other);
        assert_eq!(kind(SYNC_NONE, ALL_SHADING, UAV, UAV), Kind::Other);
        assert_eq!(kind(ALL_SHADING, SYNC_NONE, UAV, SRV), Kind::Other);
        // A bit no read has counts as a write.
        assert_eq!(
            kind(ALL_SHADING, ALL_SHADING, 0x4000_0000, SRV),
            Kind::AfterWrite
        );
    }

    fn call(first: u32, last: u32, wait: u32) -> Call {
        Call {
            readable: true,
            groups: 1,
            barriers: first + last + wait,
            first,
            last,
            wait,
        }
    }

    /// A list's calls with the switch on: first uses record nothing, ends
    /// of use defer their wait to the next first use, waits stand, and a
    /// wait right after one of ours is left out.
    #[test]
    fn the_pass_keeps_every_wait_a_call_declares() {
        let mut s = State::default();
        // The list begins: nothing is known about what came before it.
        assert_eq!(decide(&mut s, &call(0, 0, 1)), Decision::Wait);
        assert!(s.clean);
        // Nothing recorded since: the next wait is covered.
        assert_eq!(decide(&mut s, &call(0, 0, 2)), Decision::Adjacent);
        // Ends of use right after a wait: their reads are covered.
        assert_eq!(decide(&mut s, &call(0, 1, 0)), Decision::Covered);
        assert!(!s.pending);
        // Work, then a first use with nothing deferred: no wait.
        s.clean = false;
        assert_eq!(decide(&mut s, &call(1, 0, 0)), Decision::First);
        // Ends of use after work: deferred...
        assert_eq!(decide(&mut s, &call(0, 2, 0)), Decision::Defer);
        assert!(s.pending);
        // ...more work, and the first use after them waits for it all.
        assert_eq!(decide(&mut s, &call(3, 0, 0)), Decision::Flush);
        assert_eq!(
            s,
            State {
                clean: true,
                pending: false
            }
        );
        // A deferred end of use is taken by the next wait of any kind.
        s.clean = false;
        assert_eq!(decide(&mut s, &call(0, 1, 0)), Decision::Defer);
        assert_eq!(decide(&mut s, &call(0, 0, 1)), Decision::Wait);
        assert!(!s.pending);
        s.clean = false;
        assert_eq!(
            decide(&mut s, &call(1, 0, 0)),
            Decision::First,
            "nothing deferred any more"
        );
        // An alias handed over (an end of use and a first use in one call),
        // calls that cannot be read and empty calls wait.
        assert_eq!(decide(&mut s, &call(1, 1, 0)), Decision::Wait);
        s.clean = false;
        assert_eq!(
            decide(
                &mut s,
                &Call {
                    readable: false,
                    ..call(1, 0, 0)
                }
            ),
            Decision::Wait
        );
        s.clean = false;
        assert_eq!(decide(&mut s, &call(0, 0, 0)), Decision::Wait);
    }

    #[repr(C)]
    struct Group {
        kind: u32,
        count: u32,
        barriers: *const u8,
    }

    #[repr(C)]
    struct Buffer {
        sync: [u32; 2],
        access: [u32; 2],
        resource: usize,
        offset: u64,
        size: u64,
    }

    #[repr(C)]
    struct Texture {
        sync: [u32; 2],
        access: [u32; 2],
        layout: [u32; 2],
        resource: usize,
        subresources: [u32; 6],
        flags: u32,
    }

    /// Groups as the title passes them are read barrier by barrier, with
    /// the buffers' ranges and the textures' resources.
    #[test]
    fn groups_are_read_as_the_title_passes_them() {
        assert_eq!(size_of::<Group>(), 16);
        assert_eq!(size_of::<Buffer>(), 40);
        assert_eq!(size_of::<Texture>(), 64);
        let global = [[ALL_SHADING, ALL_SHADING, UAV, UAV]];
        let buffers = [
            Buffer {
                sync: [ALL_SHADING, ALL_SHADING],
                access: [UAV, SRV],
                resource: 0x1000,
                offset: 64,
                size: 128,
            },
            Buffer {
                sync: [SYNC_NONE, ALL_SHADING],
                access: [NO_ACCESS, UAV],
                resource: 0x2000,
                offset: 0,
                size: u64::MAX,
            },
        ];
        let textures = [Texture {
            sync: [ALL_SHADING, SYNC_NONE],
            access: [SRV, NO_ACCESS],
            layout: [0, 0],
            resource: 0x3000,
            subresources: [0; 6],
            flags: 0,
        }];
        let groups = [
            Group {
                kind: 0,
                count: 1,
                barriers: global.as_ptr().cast(),
            },
            Group {
                kind: 2,
                count: 2,
                barriers: buffers.as_ptr().cast(),
            },
            Group {
                kind: 1,
                count: 1,
                barriers: textures.as_ptr().cast(),
            },
            Group {
                kind: 2,
                count: 0,
                barriers: std::ptr::null(),
            },
        ];
        let mut seen = Vec::new();
        // SAFETY: groups laid out as the title's.
        let c = unsafe {
            read(groups.as_ptr().cast(), groups.len() as u32, |s| {
                seen.push(s)
            })
        };
        assert_eq!(
            c,
            Call {
                readable: true,
                groups: 4,
                barriers: 4,
                first: 1,
                last: 1,
                wait: 2
            }
        );
        let shapes: Vec<_> = seen
            .iter()
            .map(|s| (s.group, s.kind, s.resource, s.offset, s.size))
            .collect();
        assert_eq!(
            shapes,
            [
                (0, Kind::AfterWrite, 0, 0, 0),
                (2, Kind::AfterWrite, 0x1000, 64, 128),
                (2, Kind::First, 0x2000, 0, u64::MAX),
                (1, Kind::Last, 0x3000, 0, 0),
            ]
        );
        // An unknown group type or a null array is not read: the call waits.
        let odd = [Group {
            kind: 7,
            count: 1,
            barriers: global.as_ptr().cast(),
        }];
        // SAFETY: as above.
        let c = unsafe { read(odd.as_ptr().cast(), 1, |_| {}) };
        assert!(!c.readable);
        // SAFETY: a null array is tolerated.
        let c = unsafe { read(std::ptr::null(), 1, |_| {}) };
        assert!(!c.readable);
    }

    /// One compute pass of the WARP test: `words[dst] = words[src] + add`.
    const STEP_CS: &str = r#"
#define RS "RootConstants(num32BitConstants=3, b0), UAV(u0)"
cbuffer c : register(b0) { uint dst; uint src; uint add; };
RWByteAddressBuffer words : register(u0);
[RootSignature(RS)]
[numthreads(1, 1, 1)]
void main() { words.Store(dst * 4, words.Load(src * 4) + add); }
"#;

    /// A host list with passes that read what earlier ones wrote, and the
    /// title's barrier calls of every kind between them, as a title records
    /// them: with the switch off every call waits; on, first uses record
    /// nothing, ends of use wait at the next first use or at the list's end,
    /// waits right after one of ours are left out, and the passes' results
    /// are the same (needs a host device; the software adapter does:
    /// `VICTORY_D3D12_WARP=1`).
    #[test]
    fn warp_the_pass_keeps_results_and_records_only_declared_waits() {
        use windows::Win32::Graphics::Direct3D12::*;
        let Some(host) = crate::host::host() else {
            return;
        };
        let code = crate::host::compile(
            STEP_CS,
            windows::core::s!("main"),
            windows::core::s!("cs_5_1"),
        )
        .expect("the step shader compiles");
        // SAFETY: plain object creation from our own bytecode.
        let (root, pso) = unsafe {
            let root: ID3D12RootSignature = host
                .device
                .CreateRootSignature(0, &code)
                .expect("root signature");
            let pso: ID3D12PipelineState = host
                .device
                .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                    pRootSignature: std::mem::transmute_copy(&root),
                    CS: D3D12_SHADER_BYTECODE {
                        pShaderBytecode: code.as_ptr().cast(),
                        BytecodeLength: code.len(),
                    },
                    ..Default::default()
                })
                .expect("pipeline");
            (root, pso)
        };
        let Some((words, mapped)) = host.shared_buffer(256) else {
            return;
        };
        // SAFETY: a live buffer.
        let words_gpu = unsafe { words.GetGPUVirtualAddress() };
        let global = |sync: [u32; 2], access: [u32; 2]| [[sync[0], sync[1], access[0], access[1]]];
        let after_write = global([ALL_SHADING, ALL_SHADING], [UAV, UAV]);
        let first_use = global([SYNC_NONE, ALL_SHADING], [NO_ACCESS, UAV]);
        let end_of_use = global([ALL_SHADING, SYNC_NONE], [UAV, NO_ACCESS]);
        // (mode, waits recorded, results)
        for (mode, waits) in [(0u8, 7u32), (1, 4)] {
            let Ok(list) = host.new_list() else {
                return;
            };
            let _ = host.log_messages();
            // SAFETY: the mapped buffer; nothing runs yet.
            unsafe { std::ptr::write_bytes(mapped, 0, 256) };
            // A pass of the title's: a call reaches the list, then records.
            let step = |dst: u32, src: u32, add: u32| {
                list.barriers.touch();
                // SAFETY: an open list, our root signature and pipeline, a
                // live buffer.
                unsafe {
                    list.list.SetComputeRootSignature(&root);
                    list.list.SetPipelineState(&pso);
                    list.list.SetComputeRoot32BitConstants(
                        0,
                        3,
                        [dst, src, add].as_ptr().cast(),
                        0,
                    );
                    list.list.SetComputeRootUnorderedAccessView(1, words_gpu);
                    list.list.Dispatch(1, 1, 1);
                }
            };
            let call = |barriers: &[[u32; 4]; 1]| {
                let groups = [Group {
                    kind: 0,
                    count: 1,
                    barriers: barriers.as_ptr().cast(),
                }];
                // SAFETY: one global group as the title passes it; an open list.
                unsafe { groups_as(&list, 1, groups.as_ptr().cast(), mode) };
            };
            // Every read below of what a pass wrote has a call that waits
            // between the two, as the title's own barriers have.
            step(0, 0, 1); // words[0] = 1
            call(&after_write); // waits
            step(1, 0, 1); // words[1] = 2
            call(&first_use); // off: waits; on: nothing
            call(&after_write); // waits (the pass above is not covered)
            call(&after_write); // off: waits; on: right after one of ours
            step(2, 1, 1); // words[2] = 3, reading words[1]
            call(&end_of_use); // words[1] read: off, waits; on, deferred
            step(3, 0, 10); // words[3] = 11
            call(&first_use); // off: waits; on: the deferred wait
            step(1, 3, 1); // words[1] = 12: written after its read above
            call(&end_of_use); // off: waits; on: deferred to the list's end
            list.close();
            assert_eq!(host.log_messages(), None, "the debug layer found an error");
            host.execute(&[&list]);
            // SAFETY: the list has run; the mapped buffer.
            let got: Vec<u32> = (0..4)
                .map(|i| unsafe { mapped.cast::<u32>().add(i).read_volatile() })
                .collect();
            assert_eq!(got, [1, 12, 3, 11], "mode {mode}: the passes' results");
            assert_eq!(
                list.barriers.recorded(),
                waits,
                "mode {mode}: the waits recorded"
            );
            list.reset();
            assert_eq!(
                list.barriers
                    .state(&list.barriers.waited, &list.barriers.pending),
                State::default()
            );
            eprintln!("barrier pass mode {mode}: results {got:?}, {waits} waits recorded");
        }
    }
}
