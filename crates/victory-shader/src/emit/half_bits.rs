//! 16-bit typed buffer accesses that keep their bits (fxceless #927, tippythetaidum), a child
//! module of the lowering so it shares the emitter's state.
//!
//! On the console `buffer_load_format_d16_*` and `buffer_store_format_d16_*` move each channel's
//! 16 bits as they are through a 16-bit integer view, and the title keeps half floats (and plain
//! 16-bit integers, indices, beside them) in `R16G16B16A16_UINT` and `R16_UINT` buffers: the
//! network that animates the faces (`compute_graph_ops`) moves every tensor so. Ours lowered
//! those accesses as float ones (`bufferLoad.f32` then `legacyF32ToF16`, `legacyF16ToF32` then
//! `bufferStore.f32`, through handles of `f32` components): through an integer view that is
//! outside what D3D12 defines (the component type a shader declares must be the view format's
//! kind: float, unsigned or signed), and NVIDIA's cards read 0 there, so the network computed
//! zeros and no mouth moved.
//!
//! Through a descriptor the program loads from a fixed place in a table, such an access now
//! moves unsigned integers (`bufferLoad.i32` / `bufferStore.i32`, the handle annotated as `u32`),
//! packing and unpacking the halves as before, and the place is named for the host
//! ([`super::super::HostShader::half_views`]), which shows the shader a 16-bit float or signed
//! buffer view there as the unsigned format of the same layout. A plain format load whose every
//! channel the program reads only as a half float (`v_cvt_f32_f16`, a `v_fma_mix*` source of 16
//! bits) moves 16-bit integers too: the head program reads the network's output so, through the
//! same `R16_UINT` views. A place is named only where every access through it moves halves and
//! the program picks nothing of its table at run time; every other access keeps the float path.
//!
//! `VICTORY_HALF_BITS=0`: no place named, every translation as before.

use std::collections::{BTreeSet, HashMap, HashSet};

use super::{Fields, Inst, Lower, Operand, Reg, Role, RootType, Site, Source, Value};

/// Whether 16-bit typed buffer accesses keep their bits ([the module notes](self)): on unless
/// `VICTORY_HALF_BITS=0`. Part of the translation the shader cache keys on.
pub(super) fn half_bits() -> bool {
    #[cfg(test)]
    if OFF.with(std::cell::Cell::get) {
        return false;
    }
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("VICTORY_HALF_BITS").map_or(true, |v| v.trim() != "0"))
}

#[cfg(test)]
thread_local! {
    /// Tests: [`half_bits`] off on this thread ([`with_half_bits_off`]).
    static OFF: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Tests: `f` run with [`half_bits`] off on this thread, as `VICTORY_HALF_BITS=0` has it.
#[cfg(test)]
pub(in crate::emit) fn with_half_bits_off<R>(f: impl FnOnce() -> R) -> R {
    OFF.with(|off| off.set(true));
    let r = f();
    OFF.with(|off| off.set(false));
    r
}

/// A d16 access of a typed buffer through a buffer descriptor (MUBUF; the title's MTBUF loads
/// convert by their own format, through raw views).
fn d16(name: &str) -> bool {
    name.starts_with("buffer_") && name.contains("_format_d16_")
}

/// The channels of a format load of whole dwords (`buffer_load_format_x` to `_xyzw`, not d16).
fn format_channels(name: &str) -> Option<u8> {
    match name.strip_prefix("buffer_load_format_")? {
        "x" => Some(1),
        "xy" => Some(2),
        "xyz" => Some(3),
        "xyzw" => Some(4),
        _ => None,
    }
}

/// What reads each value of a program: the instructions (by site) and the phis (their values).
struct Uses {
    insts: HashMap<Value, Vec<Site>>,
    phis: HashMap<Value, Vec<Value>>,
}

impl Uses {
    fn of(p: &crate::ir::Program) -> Self {
        let mut insts: HashMap<Value, Vec<Site>> = HashMap::new();
        let mut phis: HashMap<Value, Vec<Value>> = HashMap::new();
        for (b, block) in p.blocks.iter().enumerate() {
            for phi in &block.phis {
                for &arg in &phi.args {
                    phis.entry(arg).or_default().push(phi.value);
                }
            }
            for (n, i) in block.insts.iter().enumerate() {
                for &(_, v) in &i.reads {
                    let sites = insts.entry(v).or_default();
                    if sites.last() != Some(&(b, n)) {
                        sites.push((b, n));
                    }
                }
            }
        }
        Uses { insts, phis }
    }
}

impl Lower<'_> {
    /// The table places ([`Self::table_place`]) every access through which moves halves (a d16
    /// access, or a format load read only as halves), and the format loads read only as halves
    /// ([`Self::format_load_read_as_halves`]), by site. A place the program reaches otherwise
    /// too is left out (a format load of numbers beside one of halves, a raw or scalar load, an
    /// image, a descriptor chosen by control flow), and so is every place of a table the
    /// program also picks from at run time: what the program makes of such a view is not
    /// halves alone. Found before the code is lowered.
    pub(super) fn half_places(&self) -> (BTreeSet<(u8, u32)>, HashSet<Site>) {
        let p = self.x.p;
        let mut uses: Option<Uses> = None;
        let mut loads = HashSet::new();
        let mut halves = BTreeSet::new();
        let mut other = BTreeSet::new();
        let mut picked = HashSet::new();
        for d in &self.x.res.descriptors {
            if d.role == Role::Sampler {
                continue;
            }
            let inst = p.blocks.get(d.site.0).and_then(|b| b.insts.get(d.site.1));
            let half = matches!(d.source, Source::Loaded { .. })
                && inst.is_some_and(|i| {
                    if d16(i.name()) {
                        return true;
                    }
                    if format_channels(i.name()).is_none() {
                        return false;
                    }
                    let uses = uses.get_or_insert_with(|| Uses::of(p));
                    let read_as_halves = self.format_load_read_as_halves(i, uses);
                    if read_as_halves {
                        loads.insert(d.site);
                    }
                    read_as_halves
                });
            for leaf in d.source.leaves() {
                let Source::Loaded { load, .. } = leaf else {
                    continue;
                };
                let Some(a) = self.scalar.get(load) else {
                    continue;
                };
                if a.dynamic.is_some() {
                    if let Some(rp) = self.x.native.layout.get(a.param)
                        && rp.root == RootType::Table
                    {
                        picked.insert(rp.slot);
                    }
                } else if let Some(place) = self.table_place(leaf) {
                    if half {
                        halves.insert(place);
                    } else {
                        other.insert(place);
                    }
                }
            }
        }
        halves.retain(|p| !other.contains(p) && !picked.contains(&p.0));
        (halves, loads)
    }

    /// Whether `inst` is a format load of whole dwords (not d16) whose every channel is read
    /// only as halves ([`Self::read_only_as_halves`]).
    fn format_load_read_as_halves(&self, inst: &Inst, uses: &Uses) -> bool {
        let Fields::Buffer {
            data: Some(Operand::Vector { reg, .. }),
            ..
        } = inst.fields
        else {
            return false;
        };
        let Some(count) = format_channels(inst.name()) else {
            return false;
        };
        (0..count).all(|k| {
            inst.write(Reg::V(reg + k))
                .is_some_and(|v| self.read_only_as_halves(v, uses))
        })
    }

    /// Whether every use of `value`, a typed buffer load's result, reads one 16-bit half of it
    /// as a half float: a `v_fma_mix*` source with its `op_sel_hi` bit set, or `v_cvt_f32_f16`.
    /// A program that takes a format load apart like that reads a 16-bit integer view on the
    /// console (the load gives the zero-extended integer, whose bits are a half; a float view's
    /// value it would not take apart). A use is a read as an operand
    /// ([`crate::ir::operand_reads`]): a later write of the register reads the value only to
    /// keep it in the lanes EXEC leaves alone, and a phi carries it on, its own uses counting.
    /// False without a use.
    // shortcut: what a later write under EXEC keeps of the value in the lanes it leaves alone,
    // and what reads that, is not followed (as #927 has it: following it without reasoning
    // about the lanes loses the head program's loads, whose registers the code reuses under
    // other masks).
    fn read_only_as_halves(&self, value: Value, uses: &Uses) -> bool {
        let p = self.x.p;
        let mut used = false;
        let mut seen = HashSet::from([value]);
        let mut todo = vec![value];
        while let Some(value) = todo.pop() {
            for &phi in uses.phis.get(&value).into_iter().flatten() {
                if seen.insert(phi) {
                    todo.push(phi);
                }
            }
            for &(b, n) in uses.insts.get(&value).into_iter().flatten() {
                let i = &p.blocks[b].insts[n];
                let operands = crate::ir::operand_reads(p, i);
                let regs: Vec<Reg> = i
                    .reads
                    .iter()
                    .filter(|r| r.1 == value && operands.contains(&r.0))
                    .map(|r| r.0)
                    .collect();
                if regs.is_empty() {
                    continue;
                }
                used = true;
                let Fields::Valu(v) = &i.fields else {
                    return false;
                };
                match i.name() {
                    "v_cvt_f32_f16" => {}
                    "v_fma_mix_f32" | "v_fma_mixlo_f16" | "v_fma_mixhi_f16" => {
                        let whole = v.src.iter().enumerate().any(|(k, op)| {
                            matches!(*op, Operand::Vector { reg, .. } if regs.contains(&Reg::V(reg)))
                                && v.opsel_hi >> k & 1 == 0
                        });
                        if whole {
                            return false;
                        }
                    }
                    _ => return false,
                }
            }
        }
        used
    }

    /// The table descriptor `src` is, as (root parameter, dword in the table), when the code
    /// loads it from a fixed place in a table: the load's own place, not the start of the range
    /// its used-resource entry describes (a range of several descriptors starts before most of
    /// them). `None` for a descriptor picked at run time, built in the code or chosen by
    /// control flow.
    pub(super) fn table_place(&self, src: &Source) -> Option<(u8, u32)> {
        let Source::Loaded { load, first } = src else {
            return None;
        };
        let a = self.scalar.get(load)?;
        let rp = self.x.native.layout.get(a.param)?;
        (rp.root == RootType::Table && a.dynamic.is_none())
            .then(|| (rp.slot, a.offset / 4 + u32::from(*first)))
    }

    /// The table place of the typed buffer access `name` at `site` through `src`, if it moves
    /// halves: a d16 access or a format load read only as halves, through a place every access
    /// of the program moves halves through ([`Self::half_places`]).
    pub(super) fn half_place(&self, site: Site, name: &str, src: &Source) -> Option<(u8, u32)> {
        if self.half_only.is_empty() {
            return None;
        }
        let place = self.table_place(src)?;
        (self.half_only.contains(&place) && (d16(name) || self.half_loads.contains(&site)))
            .then_some(place)
    }
}
