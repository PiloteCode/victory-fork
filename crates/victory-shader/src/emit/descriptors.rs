//! Descriptors as host handles (a child module of `lower`, sharing the emitter's state): the
//! handle for a descriptor the program reads, its heap index (computed in the block, in each
//! lane, or chosen by control flow through phis), the offset a descriptor built around a root
//! descriptor adds, a root view's bias and a heap view's stride.

use super::{
    Attr, Bin, CHOSEN, Fields, Inst, Lower, NULL, Operand, Reg, ResourceType, RootType, Site,
    Source, Tag, UsedResource, V, Val, Why,
};

impl<'a> Lower<'a> {
    /// The byte offset a buffer descriptor built around a root descriptor adds
    /// (constant and computed parts); none for other descriptors.
    pub(super) fn built_offset(&mut self, src: &Source) -> Result<(u32, Option<V>), Why> {
        match src {
            Source::Built { address, .. } => {
                let mut x = match address.dynamic {
                    Some(v) => Some(self.i32_of(v)?),
                    None => None,
                };
                if let Some(bias) = self.root_bias(address.param) {
                    x = Some(match x {
                        Some(d) => self.s.m.bin(self.cur, Bin::Add, d, bias),
                        None => bias,
                    });
                }
                Ok((address.offset, x))
            }
            _ => Ok((0, None)),
        }
    }

    /// For a root view the host binds as a raw view that may begin before
    /// the title's address (`shader.rs` `USER_VIEW_BIAS`): the bytes to add
    /// to offsets through it. Every root UAV, and the root SRVs of programs
    /// with a spilled user-data table (the others keep the views they
    /// have).
    pub(super) fn root_bias(&mut self, param: usize) -> Option<V> {
        use super::super::shader::USER_VIEW_BIAS;
        let rp = *self.x.native.layout.get(param)?;
        if rp.root != RootType::Descriptor {
            return None;
        }
        let kind = self
            .x
            .native
            .used
            .iter()
            .find(|u| u.user_data == rp.user_data)
            .map(|u| u.kind);
        let biased = match kind {
            Some(ResourceType::Uav) => true,
            Some(ResourceType::Srv) => crate::spill::is_spilled(self.x.native),
            _ => false,
        };
        biased.then(|| self.host_word(RootType::Other(USER_VIEW_BIAS), rp.slot, 0))
    }

    /// The used-resource entry a descriptor loaded from a table falls in.
    pub(super) fn used_kind(&self, src: &Source) -> Option<&'a UsedResource> {
        if let Source::Phi { args, .. } | Source::Select { args, .. } | Source::Lanes { args, .. } =
            src
        {
            return self.used_kind(args.first()?);
        }
        if let Source::LanePhi { args, .. } = src {
            return args.iter().flatten().find_map(|a| self.used_kind(a));
        }
        if let Source::Built { address, .. } = src {
            let rp = self.x.native.layout.get(address.param)?;
            return self
                .x
                .native
                .used
                .iter()
                .find(|u| u.root == RootType::Descriptor && u.user_data == rp.user_data);
        }
        let (Source::Loaded { load, first } | Source::Fetched { load, first }) = src else {
            return None;
        };
        let a = self.scalar.get(load)?;
        let rp = self.x.native.layout.get(a.param)?;
        if rp.root != RootType::Table {
            return None;
        }
        let dword = (a.offset / 4) + u32::from(*first);
        self.x
            .native
            .used
            .iter()
            .filter(|u| {
                u.root == RootType::Table
                    && u.user_data == rp.user_data
                    && u.table_offset_dwords <= dword
            })
            .max_by_key(|u| u.table_offset_dwords)
    }

    /// A handle for the descriptor `src` read at `site`.
    pub(super) fn handle(
        &mut self,
        site: Site,
        src: &Source,
        sampler: bool,
        props: (u32, u32),
    ) -> Result<V, Why> {
        match src {
            Source::Loaded { load, first } => {
                let key = (self.cur, *load, *first, sampler, props);
                if let Some(&h) = self.handles.get(&key) {
                    return Ok(h);
                }
                let index = self.heap_index(*load, *first, sampler)?;
                // A waterfall loop's load at the first lane's offset reads
                // each lane's own entry (see `resource::Waterfall`); in a
                // pixel program a helper lane may have an offset of its own.
                let per_lane = self.x.res.waterfall.loads.contains(load) || self.helper_scalars;
                let h = self
                    .s
                    .heap_handle(self.cur, index, sampler, per_lane, props);
                self.handles.insert(key, h);
                Ok(h)
            }
            Source::Constant(words) if sampler => {
                let words: [u32; 4] = words
                    .get(..4)
                    .and_then(|w| w.try_into().ok())
                    .ok_or("a sampler constant")?;
                // Keyed by the words and the comparison: one sampler used with
                // and without comparison is two static samplers, each of the
                // kind its operations need (#250).
                let cmp = props.0 & 0x8000 != 0;
                let cmps = self.s.cmp_samplers;
                let found = (0..self.samplers.len())
                    .position(|n| self.samplers[n] == words && (cmps >> n & 1 != 0) == cmp);
                let n = match found {
                    Some(n) => n,
                    None => {
                        let n = self.samplers.len();
                        if n >= 64 {
                            return Err("more than 64 immediate samplers");
                        }
                        self.samplers.push(words);
                        if cmp {
                            self.s.cmp_samplers |= 1 << n;
                        }
                        n
                    }
                };
                Ok(self.s.sampler_handle(self.cur, n as u32))
            }
            Source::Constant(_) => Err("a constant resource descriptor"),
            Source::Built { address, .. } => {
                let rp = *self
                    .x
                    .native
                    .layout
                    .get(address.param)
                    .ok_or("a descriptor built around a table in the code")?;
                let kind = self
                    .x
                    .native
                    .used
                    .iter()
                    .find(|u| u.user_data == rp.user_data)
                    .map(|u| u.kind);
                match (rp.root, kind, props.0) {
                    (RootType::Descriptor, Some(ResourceType::Cbv) | None, 13) => {
                        Ok(self.s.root_cbv_handle(rp.slot))
                    }
                    (RootType::Descriptor, Some(ResourceType::Srv), 11) => {
                        Ok(self.s.root_srv_handle(rp.slot))
                    }
                    // A root UAV: a raw buffer whether the access reads
                    // (11) or writes (11 with the UAV bit). Indexed
                    // accesses reach here as raw ones at index times the
                    // entry's stride (`buffer`).
                    // (no rasterizer-ordered form: only heap views are ordered)
                    (RootType::Descriptor, Some(ResourceType::Uav), _) if self.s.ordered => {
                        Err("a root UAV of a pixel program ordered by primitive")
                    }
                    (RootType::Descriptor, Some(ResourceType::Uav), 11 | 0x100b) => {
                        Ok(self.s.root_uav_handle(rp.slot))
                    }
                    (RootType::Descriptor, Some(ResourceType::Srv), 0x100b) => {
                        Err("a write through a root SRV")
                    }
                    _ => Err("a typed root view"),
                }
            }
            Source::Phi { block, args } => {
                let _ = site;
                let phi = self.descriptor_phi(*block, args, sampler, 8)?;
                Ok(self.s.heap_handle(self.cur, phi, sampler, self.helper_scalars, props))
            }
            Source::Select { scc, args } => {
                // The heap index of each choice, picked by SCC (uniform).
                let [
                    Source::Loaded { load: a, first: fa },
                    Source::Loaded { load: b, first: fb },
                ] = args[..]
                else {
                    return Err("a descriptor chosen by a condition");
                };
                let cond = match self.get(*scc)? {
                    Val::Bit(x) => x,
                    _ => return Err("SCC without a value"),
                };
                let set = self.heap_index(a, fa, sampler)?;
                let clear = self.heap_index(b, fb, sampler)?;
                let index = self.s.m.select(self.cur, cond, set, clear);
                Ok(self.s.heap_handle(self.cur, index, sampler, self.helper_scalars, props))
            }
            Source::Fetched { .. } | Source::Lanes { .. } | Source::LanePhi { .. } => {
                // A descriptor each lane has its own of (a waterfall loop
                // reads it from the first active lane): each lane's heap
                // index, non-uniform.
                let index = self.lane_heap_index(src, sampler)?;
                Ok(self.s.heap_handle(self.cur, index, sampler, true, props))
            }
        }
    }

    /// The heap index a descriptor phi picks. Each predecessor computes its
    /// heap index at its end and the join picks one with a phi; a
    /// predecessor whose descriptor is itself chosen by control flow (a
    /// phi of an earlier join, which dominates it) gives that join's phi,
    /// at most `depth` joins deep.
    fn descriptor_phi(
        &mut self,
        block: usize,
        args: &[Source],
        sampler: bool,
        depth: u32,
    ) -> Result<V, Why> {
        const WHY: Why = CHOSEN;
        let p = self.x.p;
        let mut incoming = Vec::new();
        for (&q, arg) in p.blocks[block].preds.iter().zip(args) {
            if self.block[q].is_none() {
                continue;
            }
            let tail = self.tail[q].ok_or(WHY)?;
            let at = self.edges.get(&(q, block)).copied().unwrap_or(tail);
            let index = match arg {
                Source::Loaded { load, first } => {
                    let here = std::mem::replace(&mut self.cur, at);
                    let index = self.heap_index(*load, *first, sampler);
                    self.cur = here;
                    index?
                }
                Source::Phi { block: inner, args: inner_args } if depth > 0 => {
                    self.descriptor_phi(*inner, inner_args, sampler, depth - 1)?
                }
                _ => return Err(WHY),
            };
            incoming.push((index, at));
        }
        let join = self.block[block].ok_or(WHY)?;
        let phi = self.s.m.phi(join, self.s.t.i32);
        self.s.m.set_phi(phi, incoming);
        Ok(phi)
    }

    /// The bytes per element of the buffer view `src` names, if the program
    /// picks it from the view heap at an index it computes (root type
    /// `ResourceHeap`): read from the host's table by heap slot in the
    /// current block.
    pub(super) fn slot_stride(&mut self, src: &Source) -> Result<Option<V>, Why> {
        let Source::Loaded { load, first } = src else {
            return Ok(None);
        };
        let heap = self
            .scalar
            .get(load)
            .and_then(|a| self.x.native.layout.get(a.param))
            .is_some_and(|p| p.root == RootType::ResourceHeap);
        if !heap {
            return Ok(None);
        }
        let key = (self.cur, *load, *first);
        if let Some(&v) = self.strides.get(&key) {
            return Ok(Some(v));
        }
        let c = self.cur;
        let slot = self.heap_index(*load, *first, false)?;
        // Inside the table whatever the index (lanes that do not run
        // compute one too): a root view has no bounds of its own.
        let last = self.s.m.i32(super::super::shader::SLOT_STRIDES_COUNT - 1);
        let i32t = self.s.t.i32;
        let slot = self.s.op(c, "binary.i32", Attr::ReadNone, i32t, 40, &[slot, last]);
        self.s.m.tag(slot, Tag::Clamp);
        let two = self.s.m.i32(2);
        let byte = self.s.m.bin(c, Bin::Shl, slot, two);
        let h = self.s.root_srv_handle(super::super::shader::SLOT_STRIDES);
        let iu = self.s.m.undef(self.s.t.i32);
        let mask = self.s.m.i8(1);
        let align = self.s.m.i32(4);
        let r = self.s.op(
            c,
            "rawBufferLoad.f32",
            Attr::ReadOnly,
            self.s.t.res_ret,
            139,
            &[h, byte, iu, mask, align],
        );
        let f = self.s.m.extract(c, r, 0);
        let v = self.as_int(f);
        self.strides.insert(key, v);
        Ok(Some(v))
    }

    /// The heap index of a descriptor that may differ between lanes,
    /// computed in the current block (or, chosen by control flow, at the
    /// join).
    fn lane_heap_index(&mut self, src: &Source, sampler: bool) -> Result<V, Why> {
        match src {
            Source::Loaded { load, first } => self.heap_index(*load, *first, sampler),
            Source::Fetched { load, first } => self.fetched_index(*load, *first, sampler),
            Source::Lanes { mask, args } | Source::Select { scc: mask, args } => {
                let [a, b] = &args[..] else {
                    return Err("a descriptor chosen per lane");
                };
                let cond = if matches!(src, Source::Lanes { .. }) {
                    let &m = self
                        .x
                        .st
                        .masks
                        .get(mask)
                        .ok_or("a descriptor chosen per lane by an untracked mask")?;
                    self.boolean(m, self.cur)?
                } else {
                    match self.get(*mask)? {
                        Val::Bit(x) => x,
                        _ => return Err("SCC without a value"),
                    }
                };
                let set = self.lane_heap_index(a, sampler)?;
                let clear = self.lane_heap_index(b, sampler)?;
                Ok(self.s.m.select(self.cur, cond, set, clear))
            }
            // (in the paired form a thread comes the wave's way, not each lane its own)
            Source::LanePhi { .. } if self.twin.is_some() => {
                Err("a per-lane descriptor chosen by the lanes' ways, in paired lanes")
            }
            Source::LanePhi { block, args } => {
                // Each predecessor computes its heap index at its end (what
                // it reads is there: the dwords are, on that edge); the join
                // picks the one of the edge the lane came by. An edge no
                // lane reading it takes gives index 0.
                let join =
                    self.block[*block].ok_or("a per-lane descriptor chosen by control flow")?;
                let mut incoming = Vec::new();
                for (&q, arg) in self.x.p.blocks[*block].preds.iter().zip(args) {
                    if self.block[q].is_none() {
                        continue;
                    }
                    let tail =
                        self.tail[q].ok_or("a per-lane descriptor joined from a later block")?;
                    let at = self.edges.get(&(q, *block)).copied().unwrap_or(tail);
                    let index = match arg {
                        Some(arg) => {
                            let here = std::mem::replace(&mut self.cur, at);
                            let index = self.lane_heap_index(arg, sampler);
                            self.cur = here;
                            index?
                        }
                        None => self.s.m.i32(0),
                    };
                    incoming.push((index, at));
                }
                let phi = self.s.m.phi(join, self.s.t.i32);
                self.s.m.set_phi(phi, incoming);
                Ok(phi)
            }
            _ => Err("a per-lane descriptor from neither a heap load nor a choice of them"),
        }
    }

    /// The heap index of the descriptor at dword `first` of what the global
    /// load at `load` reads, in each lane: its address (base plus offsets)
    /// over the descriptor size.
    fn fetched_index(&mut self, load: Site, first: u8, sampler: bool) -> Result<V, Why> {
        const WHY: Why = "a per-lane descriptor load";
        let a = *self
            .scalar
            .get(&load)
            .ok_or("an untracked descriptor load")?;
        match self.x.native.layout.get(a.param).map(|p| p.root) {
            Some(RootType::Table | RootType::ResourceHeap | RootType::SamplerHeap) => {}
            _ => return Err("a descriptor outside the heaps"),
        }
        let li: &Inst = &self.x.p.blocks[load.0].insts[load.1];
        let Fields::Flat {
            addr: Some(Operand::Vector { reg, .. }),
            saddr,
            offset,
            ..
        } = li.fields
        else {
            return Err(WHY);
        };
        let c = self.cur;
        // The low dword of the address: SGPR base plus VGPR offset, or the
        // VGPR pair's low half (heap addresses fit in 32 bits).
        let mut at = self.i32_of(li.read(Reg::V(reg)).ok_or(WHY)?)?;
        if let Some(Operand::Scalar { code, .. }) = saddr
            && code != NULL
        {
            let base = self.i32_of(li.read(Reg::S(code)).ok_or(WHY)?)?;
            at = self.s.m.bin(c, Bin::Add, base, at);
        }
        let k = self
            .s
            .m
            .i32((offset as i32 as u32).wrapping_add(4 * u32::from(first)));
        at = self.s.m.bin(c, Bin::Add, at, k);
        let shift = self.s.m.i32(if sampler { 4 } else { 5 });
        Ok(self.s.m.bin(c, Bin::LShr, at, shift))
    }

    /// The heap index of the descriptor at dword `first` of what the scalar
    /// load at `load` reads, computed in the current block.
    pub(super) fn heap_index(&mut self, load: Site, first: u8, sampler: bool) -> Result<V, Why> {
        let a = *self
            .scalar
            .get(&load)
            .ok_or("an untracked descriptor load")?;
        match self.x.native.layout.get(a.param).map(|p| p.root) {
            Some(RootType::Table | RootType::ResourceHeap | RootType::SamplerHeap) => {}
            _ => return Err("a descriptor outside the heaps"),
        }
        let li: &Inst = &self.x.p.blocks[load.0].insts[load.1];
        let Fields::Smem {
            base,
            offset,
            soffset,
            ..
        } = &li.fields
        else {
            return Err("a descriptor load");
        };
        let Operand::Scalar { code, .. } = *base else {
            return Err("a descriptor load");
        };
        let c = self.cur;
        let base = li.read(Reg::S(code)).ok_or("a descriptor load")?;
        let mut addr = self.i32_of(base)?;
        let k = self
            .s
            .m
            .i32((*offset as u32).wrapping_add(4 * u32::from(first)));
        addr = self.s.m.bin(c, Bin::Add, addr, k);
        if !matches!(soffset, Operand::Scalar { code: NULL, .. }) {
            let so = self.operand(li, soffset)?;
            addr = self.s.m.bin(c, Bin::Add, addr, so);
        }
        let shift = self.s.m.i32(if sampler { 4 } else { 5 });
        Ok(self.s.m.bin(c, Bin::LShr, addr, shift))
    }
}
