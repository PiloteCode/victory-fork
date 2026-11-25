# Ray tracing in this fork: what the title does and what has been built

Status, 2026-11-25: **nothing is lit by ray tracing yet.** With
`VICTORY_BVH_BUILDER=1` the builder runs its whole first frame in the game
(some 12,000 dispatches in the user-save scenario). In run rt-int1 the title
died one frame later, its threads reading pointers of all ones out of pages
that held what the builder wrote. The cause is found and fixed: the builder
dispatches its box rehydration by thread counts, which the layer took for
group counts, and those stages wrote empty nodes over gigabytes past their
structures (see "Dispatches by thread counts" below). No run with the fix
has been made yet. The builder stays off by default.

## What the title does

The title's indirect light (probes, bounce, reflections) and the
shadows of its lamps and sun are ray traced. Without them, anything the sun
does not hit directly is black: interiors, the shadowed side of a face, rooms
lit only by lamps.

The title does not use a ray-tracing API:

- Traversal is ordinary compute-shader code: a per-thread stack in shared
  memory, and loads through raw 40-bit addresses of title memory.
- The static world's acceleration structures ship pre-built in the title's
  data, in the console GPU's native node format (64-byte nodes: box nodes with
  four children and 16-bit float bounds, 128-byte box nodes with 32-bit float
  bounds, triangle nodes, instance nodes). They are unpacked on the GPU.
- A set of builder programs (from the title's `XboxBvhBuilder.dll`, 52 compute
  programs) builds and refits the dynamic structures on the GPU every frame.
- Only the per-node test is hardware: the instructions
  `image_bvh64_intersect_ray` and `image_bvh_intersect_ray`. One call tests a
  ray against a box node's four children (returning the hit children, nearest
  first) or against a triangle node (returning the intersection's numerator,
  denominator and barycentric numerators).

So mapping this onto the host's own ray tracing (DXR) is not practical: the
layer would have to know the geometry behind structures the title never
describes to it, and the traversal loops live in 64 of the title's own
programs. This fork runs the title's builder and traversal as translated
shaders and emulates the one instruction.

## What this fork adds

1. **The builder programs translate** (all 52). They pass buffer addresses in
   a spilled parameter table rather than in descriptors. Added: the spilled
   user-data table, root unordered-access views (translator and host binding),
   byte and 16-bit buffer accesses, and several control-flow shapes. On a GPU
   with 32-lane waves, three of them (prefix sums over groups of 256 and 512
   threads, whose 64-lane waves read lanes across their halves) run in the
   paired form: one host wave for each console wave, each thread two of its
   lanes.
2. **Title memory by raw address from shaders.** A directory with one entry per
   64 KiB of title address space leads a shader to the layer's host copy of
   that memory; loads, stores, atomic add/sub and float min/max go through it.
   Before a dispatch the host makes ready the memory its root views and root
   constants name.
3. **The ray test, emulated in DXIL.** Four slab tests and a sorting network
   for box nodes; a watertight ray/triangle test for triangle nodes; both forms
   of the instruction. A plain Rust reference lives in
   `crates/victory-shader/src/bvh.rs`; tests run the emitted DXIL on the GPU
   over 311 ray/node cases and compare with the reference bit for bit.
4. **The trace and shading programs translate** (59 of 64 trace programs, and
   the irradiance-cache shading program). This needed lane masks treated as
   plain numbers on some paths, and exact forms for programs written for
   64-lane waves running on a GPU with 32-lane waves.
5. **A safety rule on the host side.** The builder's stages run only while
   every builder pipeline the title has created has a host pipeline; otherwise
   none run (`VICTORY_BVH_BUILDER=1` asks for it; `VICTORY_BVH_QUARANTINE=1`
   also keeps the builder's stores out of title memory, for a first test). A
   partly working builder wrote garbage bounds into title memory and crashed
   the title.
6. **What running the builder in the game took so far** (2026-11-25, each a
   commit of its own):
   - *List upload memory where shaders reach it.* The builder writes the
     tables its stages read into list upload memory and hands them the
     addresses; its header stage (InitHeaders) reads its records by address.
     The blocks of the process heap the layer gave out lay at 1.5-1.8 TiB,
     past the console's 40 bits and the directory (below 128 GiB): the records
     read as zeros and no structure header was written. Upload memory now
     comes from 4 MiB chunks of one write-watched reservation below 128 GiB
     (`list.rs`, `upload`), named before every builder dispatch.
   - *The structures' sizes, copied on the card.* The title copies each
     structure's compacted size out of the builder's buffer with
     `CopyBufferRegion` in the list that computed it. Done on the CPU at
     submission, the copy took what the buffer held before the list's
     shaders ran; in lists whose shaders store by address it now runs on the
     card behind them (`HostList::copy_on_card`).
   - *Copies of title memory by address kept current where named.* Every list
     that ran a builder stage refreshed and compared all numbered copies
     (1,023 windows, 4 GiB): with the stages bound and not run that alone made
     a median 975 ms a frame against 353 without the builder. A list now
     refreshes the copies its dispatches name (and the upload memory's) and
     declares written only those, and only for programs that store through an
     address (`VICTORY_MEMORY_ALL=1`: as before). Low dwords of address pairs
     are no longer also taken as addresses in 256-byte units (717 of the 802
     windows named that way in the first frame were), and there are 2,047
     numbers: the first frame ran out of the 1,023.
   - *Remaps after the flights.* `CopyPageMappingsBatchX` waits for flights
     whose shaders may write the range it maps.
   - *DirectStorage's writes into physical pages, seen wherever the pages are
     shown.* The title streams its pre-built structures into pool pages and
     maps them into its tiled heap. A write through another view than the
     guarded address never reached the host copies, which kept what the pages
     held before. The integration branch has this report from upstream
     (`xmem.rs`, `pages_written`; `watch.rs`, `title_memory_written`,
     `VICTORY_PAGE_WRITES`) and guards every view of a page together
     (`VICTORY_VIEW_GUARD`), so a write the title's CPU makes through one view
     is seen at the others.
   - *Dispatches by thread counts.* `DispatchX(x, y, z, flags)` passes the
     console's dispatch initiator bits as its flags. The builder sets
     `USE_THREAD_DIMENSIONS` (0x20; with 0x8000 for wave32 programs) on four
     of its dispatch calls: header copies (DecompressHeader), the box
     rehydration of pre-built structures (Box32Rehydration,
     Box16Rehydration) and triangle reloads (ReloadTriangles,
     ReloadProcedurals). Then x, y and z count threads: the last group is cut
     short, and no thread past the count is launched. The layer took them for
     group counts. The box stages rebuild one level of a tree a dispatch, in
     place, one node a thread, group g writing its 64 nodes at g times 8 KiB
     (4 KiB for 16-bit boxes), and a lane count read off EXEC
     (`WaveAllBitCount`) places each node's compressed payload. In rt-int1's
     first frame they ran 272,948 and 47,026 groups where 5,983 and 4,217
     were wanted, and wrote 2.2 GiB of empty nodes past their levels; where
     that was put back, it lay over the title's own objects, which share
     those pages at a CPU view 24 GiB lower. Every crash site of rt-int1
     (title +0x2bd385b, +0x27c195d, +0x4f38ba5, +0x4e51965) lies in such an
     overrun. (The copies and reloads check their counts themselves: for
     them it cost time only, 236,385 groups of ReloadTriangles for 7,410.)
     `dispatch_x` now makes groups of the program's console size out of the
     counts. Where a last group is cut short, the pipeline runs in a form of
     its program translated for that
     (`victory_shader::emit::translate_thread_limited`, made at its first such
     dispatch): the host passes the counts (`USER_THREAD_LIMIT`), and the
     threads past them leave in the entry block, so that wave operations, a
     ballot of EXEC on entry and the title's barriers count the launched
     threads only, as the console's partial wave does. The two host waves of
     a console wave64 program that exchange first tell each other whether
     they have a launched thread (one with none leaves whole, its exchange
     dword holding 0). The paired and approximate forms are refused (a host
     thread holding two console lanes cannot leave for one of them): a
     builder stage without the form turns the builder off.
   - *Addresses in the records of upload memory.* LeafRehydration reads its
     geometry descriptions (48-byte `D3D12_RAYTRACING_GEOMETRY_TRIANGLES_DESC`
     records) through a root SRV in list upload memory and loads vertices
     and indices through the addresses in them, which no root argument
     names: in rt-int1, 638 of the 2,139 such addresses lay in no copy when
     the stage ran, and those leaves read zeros. Before a dispatch, the
     block of upload memory each of the shader's root SRVs points into is
     looked through (16 KiB at most) for 8-byte title addresses, which are
     named like the dispatch's own (`memory.rs`).
7. **Four more sun-shadow trace programs translate.** They take the address
   of their structure out of a descriptor they pick from the view heap; the
   host keeps a table of view addresses by heap slot (`SLOT_VIEW_ADDRESSES`).
   The game's scene in the user-save scenario uses the checkerboard variant,
   which translated before.
8. **Diagnostics.** `VICTORY_BVH_TRACE=<file>` traces every dispatch of the
   builder and of the programs that reach memory by address, the list upload
   memory they read, every range put back into title memory, page mappings
   (with their ranges) and copies; `VICTORY_BVH_DRY=1` binds the builder's
   dispatches without running them; `VICTORY_BVH_CHECK=<hash of the header
   stage>` walks the structures it initialises a frame later, in title memory,
   and reports bad nodes; `VICTORY_CRASH_MEMORY=1` adds the title memory
   around the registers (and one pointer further) to a crash report. With the
   user's own `XboxBvhBuilder.dll`, `VICTORY_TEST_BVH_DLL` runs the builder's
   wave64 programs in their picked and paired forms on the card and compares
   them (they agree).

## Known gaps

- No run with the builder on has been made since the fix for dispatches by
  thread counts (above). Until then (rt-int1 and before): the title died one
  frame after the builder's first frame, 13 access violations on several
  threads reading pointers of all ones or of infinities, each faulting
  object 24 GiB below a tiled-heap range the box rehydration had overrun.
- The trees are not known to be valid yet. The walk (`VICTORY_BVH_CHECK`,
  which covers the structures InitHeaders starts: the dynamic ones and the
  top-level ones) found 19,638 bad nodes in 283 structures in rt-int1, most
  of them boxes of zeros or of the header stage's empty bounds, among them
  the top-level structure at 0xd5f205e00 (787 instances), which lies in no
  overrun: its instances' bounds come from the headers of the structures
  they name, which the overruns and the leaves' zeros may have spoiled. The
  walk reads title memory, which holds only what was put back: a node in a
  window no dispatch named reads as it was.
- What a stage reaches only through addresses in title memory (not in
  upload memory), such as the structures a top-level build's instances
  name, is not made ready unless something else named it; an access that
  finds no copy reads 0 and is noted for the next frame (`F` lines of the
  trace), which comes too late for a structure built once.
- Copy numbers are never given back: the builder named some 1,450 windows in
  six minutes (rt-dry1), and 2,047 numbers run out in a longer session.
- One shadow-mask trace program (FullAdaptive) stops at a lane mask read as
  data.
- An access to a title page that has no host copy is lost (a read gives zero, a
  store is dropped).
- Not publicly documented, and assumed: the instruction's rounding, the order
  of two boxes entered at the same distance, and the result for a ray exactly
  through an edge; that `USE_THREAD_DIMENSIONS` launches exactly the counted
  threads, the last group cut short (the flags carry no `PARTIAL_TG_EN`, but
  the box stages place their payloads by the lanes in EXEC, which only a cut
  short wave makes right).
- Cost on the GPU is unmeasured.

## Where to look

- `crates/victory-shader/src/bvh.rs`: the reference for the node test.
- `crates/victory-shader/src/emit/lower.rs`: the lowering of the two
  instructions and of memory by address (`Lower::memory`).
- `crates/victory-shader/src/resource.rs`: address tracking for descriptors
  and raw addresses.
- `crates/victory-gpu/src/pipeline.rs`, `draw.rs`: the builder rule and the
  host side of root views and title memory.
- `crates/victory-gpu/src/memory.rs`: title memory by address (directory,
  numbered copies, what a dispatch names).
- `crates/victory-gpu/src/bvhtrace.rs`: the builder's trace and the
  structure walk.
- `crates/victory-hle/src/modules/xmem.rs`: physical pages, their views, and
  the addresses a write into them shows at.
