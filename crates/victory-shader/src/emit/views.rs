//! The table places through which a program uses buffers with atomics or writes them (a child
//! module of the lowering, so it shares the emitter's state): what the host layer needs to put
//! a title buffer in the card's memory where the program's accesses would otherwise cross the
//! bus ([`super::super::HostShader::atomic_views`], `written_views`, `picked_views`).
//!
//! An access is placed when its descriptor is one the code loads from a fixed place in a table
//! (the place of [`Lower::table_place`]: root parameter, dword in the table), as for
//! `half_views`, or a root UAV (the parameter, dword [`ROOT_VIEW`]). A descriptor the code picks from a table at an index it computes is named by
//! its table only ([`super::super::HostShader::picked_views`]): the host can find which of
//! the table's views it is only when the program runs. Neither covers a descriptor built in the
//! code around an address, one fixed in the code, one that differs between lanes, nor the
//! `global_*` and `flat_*` accesses, whose addresses the program computes; those are counted
//! for the census ([`take_census`]).
//!
//! Nothing here changes what is written: the lists are read off the program's descriptors.

use std::collections::{BTreeMap, BTreeSet};

use super::{Fields, Lower, Role, RootType, Source};
use crate::native::ResourceType;

/// The dword of a place that names a root UAV (a root parameter that is a buffer address, no
/// table): the view of the parameter itself.
pub const ROOT_VIEW: u32 = u32::MAX;

/// Bit of [`super::super::HostShader::picked_views`]: views of the table are used with atomics.
pub const PICKED_ATOMIC: u8 = 1;
/// Bit of [`super::super::HostShader::picked_views`]: views of the table are written.
pub const PICKED_WRITTEN: u8 = 2;

/// The places and their coverage, for a [`super::Lowered`].
#[derive(Debug, Default)]
pub(super) struct ViewUses {
    pub atomic: Vec<(u8, u32)>,
    pub written: Vec<(u8, u32)>,
    pub picked: Vec<(u8, u8)>,
}

/// Census columns of the last program lowered on this thread ([`take_census`]).
pub const CENSUS: [&str; 8] = [
    "atomic_placed",
    "atomic_picked",
    "atomic_other",
    "store_placed",
    "store_picked",
    "store_other",
    "address_atomic",
    "address_store",
];

thread_local! {
    static LAST: std::cell::Cell<[u32; CENSUS.len()]> = const { std::cell::Cell::new([0; CENSUS.len()]) };
}

/// The counts of [`CENSUS`] for the program this thread lowered last: buffer atomic and store
/// instructions whose descriptor has a table place, is picked from a table at run time, or is
/// neither; and atomics and stores through computed addresses (`global_*`, `flat_*`).
pub fn take_census() -> [u32; CENSUS.len()] {
    LAST.with(|l| l.replace([0; CENSUS.len()]))
}

/// How the descriptor of one access is reached.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Reach {
    Placed,
    Picked,
    Other,
}

impl Lower<'_> {
    /// The places through which the program uses buffers with atomics or writes them (see the
    /// module notes).
    pub(super) fn view_uses(&self) -> ViewUses {
        let p = self.x.p;
        let mut atomic = BTreeSet::new();
        let mut written = BTreeSet::new();
        let mut picked: BTreeMap<u8, u8> = BTreeMap::new();
        let mut counts = [0u32; CENSUS.len()];
        for d in &self.x.res.descriptors {
            if d.role == Role::Sampler {
                continue;
            }
            let Some(inst) = p.blocks.get(d.site.0).and_then(|b| b.insts.get(d.site.1)) else {
                continue;
            };
            if !matches!(inst.fields, Fields::Buffer { .. }) {
                continue;
            }
            let name = inst.name();
            let is_atomic = name.starts_with("buffer_atomic");
            if !is_atomic && !name.starts_with("buffer_store") {
                continue;
            }
            let mut worst = Reach::Placed;
            for leaf in d.source.leaves() {
                let reach = match leaf {
                    Source::Loaded { load, .. } => match self.scalar.get(load) {
                        Some(a) if a.dynamic.is_some() => match self.x.native.layout.get(a.param) {
                            Some(rp) if rp.root == RootType::Table => {
                                let bit = if is_atomic { PICKED_ATOMIC | PICKED_WRITTEN } else { PICKED_WRITTEN };
                                *picked.entry(rp.slot).or_default() |= bit;
                                Reach::Picked
                            }
                            _ => Reach::Other,
                        },
                        Some(_) => match self.table_place(leaf) {
                            Some(place) => {
                                if is_atomic {
                                    atomic.insert(place);
                                }
                                written.insert(place);
                                Reach::Placed
                            }
                            None => Reach::Other,
                        },
                        None => Reach::Other,
                    },
                    // A root UAV (a raw buffer by address, no table): the parameter itself.
                    Source::Built { address, .. } => match self.x.native.layout.get(address.param) {
                        Some(rp)
                            if rp.root == RootType::Descriptor
                                && self.x.native.used.iter().any(|u| {
                                    u.user_data == rp.user_data && u.kind == ResourceType::Uav
                                }) =>
                        {
                            let place = (rp.slot, ROOT_VIEW);
                            if is_atomic {
                                atomic.insert(place);
                            }
                            written.insert(place);
                            Reach::Placed
                        }
                        _ => Reach::Other,
                    },
                    _ => Reach::Other,
                };
                worst = worst.max(reach);
            }
            let at = if is_atomic { 0 } else { 3 };
            counts[at + worst as usize] += 1;
        }
        for b in &p.blocks {
            for i in &b.insts {
                let n = i.name();
                if !(n.starts_with("global_") || n.starts_with("flat_")) {
                    continue;
                }
                if n.contains("_atomic") {
                    counts[6] += 1;
                } else if n.contains("_store") {
                    counts[7] += 1;
                }
            }
        }
        LAST.with(|l| l.set(counts));
        ViewUses {
            atomic: atomic.into_iter().collect(),
            written: written.into_iter().collect(),
            picked: picked.into_iter().collect(),
        }
    }
}
