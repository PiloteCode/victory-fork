# VIctory fork (work in progress)

A work-in-progress fork of [VIctory](https://github.com/Marvisak/VIctory), a
compatibility layer that runs an Xbox Series title natively on Windows from
files the user supplies. This fork follows upstream
`main` and adds fixes aimed at one goal: getting the game's world to render
correctly and playably on one particular PC.

**This repository contains no game files, code or assets.** You need your own
copy of the game. It is not affiliated with or endorsed by the game's
developer or publisher.

## Credits

- **[VIctory](https://github.com/Marvisak/VIctory)** by Marvisak and its
  contributors is the base of everything here: the loader, the runtime
  services, the Direct3D 12 back end and the shader recompiler. Licensed
  GPL-3.0-or-later; so is this fork. Upstream's history is kept intact below
  the single commit that carries this fork's changes.
- **[XCL](https://github.com/ivall/xcl)** by ivall and its contributors
  (GPL-2.0-only) was used as a reference for how the same title behaves under
  another layer. Fixes inspired by it were written anew in VIctory's code.
- The guard over every view of a memory page is code by **myokex**
  ([victory-amd](https://github.com/myokex/victory-amd), commit `c6dc4d8`), which
  adapts pull request #115 here by Spexivus, its correction by joroyt and a part of
  VIctory #1122 by tightfall. The read-beside fix for kernel reads into title
  memory is by **Joshua Odom** (commit `10ea38e` of the same fork family). Both
  are carried here unchanged apart from one merge conflict.
- This fork's changes were developed with [Claude Code](https://claude.com/claude-code).

## Status of this fork (2026-11-25)

**Not playable yet: about 5 to 5.5 frames per second in gameplay on an RTX 5090 (2.5 to 4 on an RTX 4070 before the second pass below), about 5 in the opening cutscenes, and shadowed areas are dark.**
Tested on one machine: Windows 11, Intel i7-14700KF, NVIDIA RTX 4070 12 GB,
32 GB RAM. The Series X profile is used throughout.

| Stage | State in this fork |
|-------|--------------------|
| Boot, menus, start screen | works, 30 fps (as upstream) |
| Opening cutscene | plays from start to finish, in order, lit: title cards, credits cards, night and dusk shots |
| Saves | the game autosaves a checkpoint and loads it again |
| First gameplay scene (the prologue) | renders: characters, textures, sky, sunlit exterior; HUD, pause menu and pad input work |
| Light in shadow | missing: the game's ray-traced bounce light and ray-traced lamp and sun shadows do not run yet, so anything the sun does not hit directly is black |
| Frame rate in the world | 2-4 fps: 420-450 ms per frame by default, 240-350 ms with the newer switches (`VICTORY_RELAY=2`, or `VICTORY_LATE_WAIT=3 VICTORY_WIDE_BACK=1 VICTORY_CARD_BUFFERS_MIN_MB=1 VICTORY_CARD_BUFFERS_MB=1024`); it started at 3-6 seconds per frame |
| Stability | no driver hang or device loss in normal runs (a hang introduced by the wave64 update was found and fixed: its barrier form for 64-lane programs is now off unless `VICTORY_WAVE_WIDE=1`) |

### Newest (2026-11-25, later the same day)

**Update, the same night: the first room now renders right on the test
machine** (walls, textures, people, lamps), at 190 to 450 ms a frame (2 to 5
frames a second): the picture work has caught up, the frame rate has not.
What did it, on top of the list below:
- Descriptor records the title copies into its heap as plain bytes are
  mirrored before every submission (pull request #305 by Flyander, after
  upstream VIctory 86a5ed6). Measured here: about 40,000 records a frame
  that no call had announced; their host slots were empty or stale.
- A buffer view that does not start on an element of its buffer's host
  copy gets a copy of its own bytes instead of being refused (after
  upstream 86a5ed6): 3 to 6 such views a frame.
- Room on the card is kept for the render targets and buffer copies a
  world makes in its first frame: 98 buffer copies are made (945 MB) where
  the runs before made 1, and the processor no longer compares 550 to
  710 MB a frame looking for shader writes (25 MB now).
- Levels on demand ran on the card for the first time
  (`VICTORY_LEVELS_ON_DEMAND=1`, still off by default; with the fixes of
  pull request #747 by AscilCH): 3.1 GB of the card not allocated for
  texture levels the title does not hold, no device loss.
- Guarded blocks under storage writes are opened by default; 14 more
  shader programs translate; a program is translated with first-half
  interpolation pairs only if it was refused for one (with that on for
  every program, 8,551 working pixel programs came out changed).
- Frame rate, first cuts on the processor side: the descriptor sync uses a
  cheap fingerprint and leaves draw-table slots to their draws (the pass
  over the heap went from 70-220 ms a frame to about 3), and memory the
  guards already hold is not asked of the write watch (pull request #670
  by Jonirulah). Card copies' pieces can come down inside their own
  flights (pull request #648 by Jonirulah, `VICTORY_CARD_FETCH_INFLIGHT=1`,
  off by default: the copy-back thread's wait went from 181 ms a frame to
  0 here, but the card got busier, so no gain is claimed yet). The frame is
  still 250 to 380 ms: the card is the limit now, most of it the
  per-record clamp before each of some 6,700 indirect draws a frame.
- Pull request #220 (cheaper indirect draws) is IN this tree now, its
  windows off by default (`VICTORY_INDIRECT_WINDOW=1`). History, so nobody
  repeats it: with windows on it lost the device three times here. Its own
  checker (`VICTORY_INDIRECT_WINDOW_CHECK=1`) then showed one list drawing
  90 records of 2^24 vertices from a window, where the per-draw path lets
  eight through; a record of 2^23 vertices an instance or more now runs
  empty. Since that fix: five runs with windows on, no device loss, the
  card's time per frame roughly halved. One loss after the fix had the
  in-flight fetch (#648) on as well; do not combine the two. The in-flight
  fetch itself measured slower here in two runs and stays off. (In this
  fork windows have been on by default since its second pass, below; they
  stay on with the fix: see "Upstream's second night, merged".)
- Frame rate, second night (an RTX 4070; the first room went from about
  3.5-4 to 6-7 frames a second with ALL of the switches below; the
  defaults are unchanged and give about 4.5-5):
  - `VICTORY_ASYNC_SUBMIT=1`: submissions are prepared and sent by a
    thread of the layer, not by the title's render thread, which a
    profile showed inside the submit path for 257 of 292 ms a frame.
  - `VICTORY_QUEUE2=1`: fetches of what shaders wrote go to a copy queue
    (brought back, with the fix for a piece fetched beside a later
    flight); the copy-back thread's wait fell from about 125 to 7 ms a
    frame. (On by default in this fork, in place of its own copy queue.)
  - `VICTORY_INDIRECT_WINDOW=1`: see above.
  - `VICTORY_TEXTURE_NO_WAIT=1`: a texture refresh no longer waits while
    the title remaps memory; the loading phase had 5 readings over a
    second where it had 18.
  - `VICTORY_MERGE_GAP=64 VICTORY_MERGE_RUNS=4096`: title bytes put in
    under flights' shader writes are joined over small gaps (NOT exact:
    read the comment at `merge_gap` before using it).
  - On by default: fills of upload memory and read-only buffer copies no
    longer wait for flights and are checked against what the flights then
    wrote (`VICTORY_FILL_WAIT=1` waits as before; no mismatch in any run
    so far); up to 512 idle host lists and texture staging buffers are
    reused instead of made and freed every frame (the staging buffers by
    this fork's own pool, which was there first).
  - `VICTORY_PROFILE=1` (diagnostics builds): a sampling profiler inside
    the layer; it is how the above was found.
  Still open: the frame rate dips (5-second readings of 300-500 ms among
  150-215); in some of them texture uploads are 3-5 times the usual, in
  others nothing logged stands out. Not shader compiles (checked: 4-9
  first uses and no waiting in those readings). A per-frame logger is the
  next step.
- Known and counted, not fixed yet: an indirect compute pass asks for
  about 77,500 thread groups in x where one dispatch runs 65,535 (the
  rest is cut off).

Memory sync between the title's memory and the copies on the card:
- **A write through one view of a physical page is seen by the copies taken
  through its other views** (`VICTORY_VIEW_GUARD=0` turns it off). The title
  shows one page at several addresses and a copy was guarded at one of them
  only. Measured on the test machine: 0.8 to 1.0 million such writes in a
  five-minute run, every one of which used to leave a copy stale. Since it
  went in, the first room had its walls in the later frames of 4 runs out of
  4 (2 of 5 before); the room's very first shot still comes up without walls
  in some runs.
- Kernel reads into title memory are read beside and copied in, so a block
  guarded again in between no longer fails the read.
- Writes of the storage layer into physical pages are counted where the title
  has those pages mapped (10,000 to 50,000 guarded blocks of copies lay under
  them in every run) and can open the guards there:
  `VICTORY_PAGE_WRITES=guards` (off by default; `1`, `slots`, `all` do more).
- Failed page-destination reads are retried (`VICTORY_DSTORAGE_RETRIES=0` = as before).

Card memory (the test card is full from the first frame of the world: 85 to
99 % of its budget for a whole run, about 26,000 texture copies let go and
3,400 made again in five minutes, 2.8 GB of render targets that are all in
use):
- Levels on demand, **off by default** (`VICTORY_LEVELS_ON_DEMAND=1`): a
  texture the title fills takes card memory only for the levels the title
  holds (2.2 to 3.0 GB of the copies were levels it does not hold). Its test
  runs on the software adapter; it has not run on a card yet.
- Refused texture slots are held back for a few seconds so that a pass
  reaches the slots behind them (`VICTORY_FULL_CARD_PASS=0` = as before);
  room can be kept for the card copies of title buffers
  (`VICTORY_CARD_ROOM_FOR_BUFFERS=1`, off).

Tools and diagnostics:
- `victory translate <shader file> --warm all --jobs N` translates every
  shader ahead of a run, on the processor only (about 4.5 to 6.5 minutes
  here); the shader cache is keyed on the translator's source, so a new
  build that does not touch the translator starts warm.
- Counters, all off or diagnostics-only: `VICTORY_MERGE_RACE=1` (title bytes
  a copy-back is about to put back to older values: none found in 138,000
  copy-backs), `VICTORY_WALLS_TRACE`, `VICTORY_CARD_ACCOUNTING`, and a census
  of the copies the texture sweep never lets go.

### What this fork changed (summary)

Rendering:
- Meshlet culling read its scene buffers as the wrong kind of view, so the world drew almost nothing; fixed.
- Per-draw descriptor tables could run out, after which draws used slots never made for them and the card hung; fixed.
- Light packed by the game's compute shaders was read back through empty copies (sky, probes, fog); fixed for every mip level and layer.
- The fog reconstruction, shadow-mask, hair, blend-shape skinning, fire and several material programs did not translate; fixed.
- Gathers at a named mip level (with and without an offset) read level 0; fixed.
- Implicit-level samples outside pixel shaders produced invalid DXIL in 730 compute programs; fixed.
- Rectangle lists, depth and stencil twins for depth textures written by compute, texture read-backs.
- Ray tracing, in progress and off by default: the game's own structure-builder programs translate, title memory can be read by raw address from shaders, the hardware ray/box and ray/triangle test is emulated in DXIL (checked against a reference on the GPU), and 59 of the 64 trace programs translate. Nothing is lit by it yet. See [docs/fork-ray-tracing.md](docs/fork-ray-tracing.md).

Speed and memory:
- Waits for memory values, write-watch queries and page queries no longer walk page tables per call.
- Large title buffers get a copy in card memory, with shader writes found by hashing on the card.
- Submissions can be sent ahead of the wait for the card (`VICTORY_LATE_WAIT`), with a relay thread that lands results as the card finishes (`VICTORY_RELAY`): in a first five-minute run the card's idle time behind the submitting thread fell from about 136 to 30 ms per frame.
- Title memory can be opened as a graphics heap so that buffers are used in place instead of copied (`VICTORY_INPLACE`, first stage only; a probe confirmed the approach on the test GPU).
- The bindless texture queue no longer uploads stale views; the view table no longer grows without bound; the pipeline library is mapped instead of read into memory.
- An intermittent device loss was traced to the pipeline library file being replaced while mapped; fixed.

- Translated shaders get clean-up passes over the finished module (dead code, constant folding, reuse of values, branch facts); the pack's host instruction count falls by about 43 %, worth about 20 ms of GPU time per frame on the test machine (`VICTORY_SHADER_OPT=0` turns them off).

Each speed switch is described in a comment where it is read.

### Second pass (2026-11-25, Intel i9-14900KF, NVIDIA RTX 5090 32 GB, 128 GB RAM)

Measured with diagnostics builds, scripted keys and the same scenes, five
minutes a run; "frame" is the median time per frame of the lines with 500
draws or more.

| Change | What it fixed | Measured |
|--------|---------------|----------|
| Range lock for reads of title memory in place (`watch.rs`, `xmem.rs`) | one lock over all title memory: while the world streamed in, texture refreshes waited 1-6.7 s a frame for the runtime's remappings | waits in the opening cutscene 439 ms → 0.7 ms a frame line |
| Shader cache keyed by the translator (`build.rs`, `shader_cache.rs`) | every rebuild retranslated all ~3,000 shaders | a rebuild that leaves the translator alone keeps them |
| 3/4 of the processors make pipelines (`pipeline/make.rs`) | 8 of 32 threads | the first run's backlog clears sooner |
| On by default: relay, wide snapshots/compares/refreshes/merges, card copies from 1 MiB (`=0` turns each off) | prepare, card and copy-back ran one after another | gameplay frame 408 → 232 ms |
| Indirect windows on by default, scratch buffer left read between draws (`draw.rs`, `indirect.rs`, `host.rs`) | a change of state around each of 5,000 one-record indirect draws stopped the card each time | card 131/129 → 95/103 ms, frame 240/238 → 221/218 ms |
| Shader translation fixes (agent, `victory-shader`) | 110 programs did not translate (puddles, terrain samplers, ped cut-outs, decals...) | 16 left; 25,576 of 25,758 pass translation, validation and pipeline creation |
| Relay fetches on a copy queue of their own (`host.rs`) | the copy-back's fetches of card copies waited behind every later flight on the title's queue (88-104 ms a frame on the relay thread) | gameplay frame 213 → 170 ms, mean 371 → 228 ms |
| Wave64 compute programs in the paired form (agent, `victory-shader`) | 24 compute programs (screen-tracing tiles, particles, alpha resolve, decal trails, hair shadow masks), 3 builder programs and the alpha-tested cascade shadows were refused on 32-lane GPUs; tiles of an older picture showed in the opening cutscene | no compute pipeline refused in play; the rectangles are gone |
| Spare staging buffers (`texture.rs`) | every streamed texture made and let go an upload buffer of its own, ~500 a second at 0.2-0.3 ms each | opening cutscenes: heap pass 187 → 65 ms, frame 307 → 205 ms |
| DirectStorage page numbers read again (`dstorage.rs`, `xmem.rs`) | a request whose page numbers the title wrote after enqueueing it failed (up to 302 in a burst at loading): its model or texture kept old data | no failed request since |
| Shader translation fixes, second round (agent, `victory-shader`, `pipeline/host.rs`) | 76 programs: a float the console keeps constant packed beside an integer or the front face got another interpolation mode; 64-bit floats and wave sums of them (water); two reductions sharing their end block; blending asked on an integer target (the windshield-wiper mask pipeline was refused) | in the whole opening cutscene no pipeline is refused and no shader fails to translate; offline 25,680 of 25,953 programs pass (the rest: unused voxel geometry stages, mesh outputs over the host's limit that NVIDIA builds anyway, five ray-trace programs, one driver failure) |
| Indirect records clamped into a ring, behind one enhanced barrier (`indirect.rs`, `host.rs`) | each clamp waited for the last indirect call to have read the one scratch buffer, and the call after it for the clamp | clamps 33 → 19 ms of card time a frame, the card's commands 102 → 95 ms; frames 179 → 171 ms (median), 210 → 189 ms (mean) |
| Shader translation fixes, third round (agent, `victory-shader`, `pipeline/host.rs`) | 36 wave64 compute programs (shadow-mask tiles and traces, temporal resolves, irradiance-cache and fallback lighting, light clustering, scattering gathers, decal and fire resolves, the light atlas, a lens-flare scan) ran on 32-lane GPUs in an approximate form, each half-wave counting, ORing or electing over its own 32 lanes; 71 pedestrian, vehicle and prop mesh programs declared more output than D3D12's 32 KiB; `weatherParticle:CS_BeginUpdate` got no pipeline (NVIDIA's compiler refuses a group-shared atomic add of a negated value, E_OUTOFMEMORY, DXC's own HLSL included); `PS_VoxelizeVisiblity` did not translate (a lane mask parked in two VGPR lanes came back as two scalars) | the 36 run in the exact paired form (three more lens-flare scans were exact already: an allocation with a multiply-add); an oversized mesh program is linked with its pixel program for each pipeline (all 710 pairs tried offline and the 3 drawn so far link, validate and build); offline 25,682 of 25,953 programs pass (the rest: unused voxel geometry stages, five ray-trace programs, and the 71 mesh programs as translated alone, valid once linked) |
| Geometry stages, and oversized mesh programs drawn alone (agent, `victory-shader`, `pipeline/host.rs`) | the 195 voxelization geometry programs did not translate (the console keeps their input and output vertices in LDS rings, and they read the ring's place from a table of the driver's), and a pipeline with a geometry stage was refused; one of the 71 oversized mesh programs drawn without a pixel program (the title draws depth with 143 mesh programs alone) kept its invalid translation | each geometry program runs as a host geometry shader between the container's vertex program and the pixel program (a program of their form, written for the tests, draws right on the WARP device; all 195 validate offline and get a pipeline on WARP; no run has drawn with one yet); a mesh program over 32 KiB without a pixel program keeps its system values only (all 71 validate and build so); offline 25,952 of 25,953 programs translate and validate (the rest: one ray-trace program), the DXIL of every program that passed before unchanged |

Together: the opening of the gameplay went from 487 ms to about 175-195 ms a
frame (2 → 5-5.5 fps), the opening cutscene's streaming from a median of 590 ms
(and seconds-long frames) to about 205 ms.

Tried and left off, with numbers: title memory in place (`VICTORY_INPLACE=1`:
358 ms, `=2`: 677 ms; the card reading system memory is slower), 512 lists in
the air (248 ms; the wait moves to reads of title memory), no waits around
the indirect clamp (no change), skipping the ray-tracing passes (35 ms saved
but the indirect light they already give is lost: rooms go black), unguarded
copies of title memory (`VICTORY_COPY_MODE=plain`: crashes at once on pages
never committed), the title's buffer copies run on the card (205 → 232 ms:
they fill small constant buffers whose CPU-side copies then wait for the
card), a table's constants read from the card copies (the scene vanished:
refreshes leave ranges under shader writes alone), clamp windows of 1,024
records or lasting to the title's next barrier (no fewer clamps that matter,
no frame time gained), the ray-tracing structure builder
(`VICTORY_BVH_BUILDER=1`: its first frame runs whole; the title died one
frame later because the builder's dispatches by thread counts were taken
for group counts, fixed since; see docs/fork-ray-tracing.md).

Still open: the ray-tracing structure builder (all 52 programs get
pipelines and its first frame runs, with its upload memory, sizes, copies,
streamed pages and dispatches by thread counts fixed; off by default until
a run with it on walks clean, so shadows stay dark); ~95 ms of card time a frame, 19 ms of it
our clamps of indirect records (the card still turns from drawing to
compute and back for each of 2,500 a frame: the title's ~710 barriers a
frame cut its runs of records short); ~60 ms a frame that the submitting
thread waits for flights to land, because small constant buffers the card
writes are copied into upload memory on the CPU (they are not on the
256-byte line a host constant view needs). The paired form makes 36 compute
programs exact at about 2.9 times their DXIL size (a thread runs two console
lanes, at half the threads): not timed in a run yet.

### Upstream's second night, merged (f85101b)

Where upstream and this fork had each done the same thing, one was kept:

| Both had | Kept | Why |
|----------|------|-----|
| Fetches of what shaders wrote on a copy queue (ours: `VICTORY_RELAY_COPY_QUEUE`, on; upstream: `VICTORY_QUEUE2` bit 1, off) | upstream's lane, on by default here | ours had the hole upstream fixed: a piece fetched while a later flight writes it comes down half written, and when that flight leaves it as it was, nothing fetches it again. Upstream's WARP test fails on our fetch as it stood (title memory keeps the half-way value) and passes on the merge. On by default as ours was (ours took the gameplay frame from 213 to 170 ms when it went in, 5afe21a); `VICTORY_QUEUE2=0` fetches on the direct queue, and so does `VICTORY_RELAY_COPY_QUEUE=0` with `VICTORY_QUEUE2` unset |
| Indirect records left readable between draws (ours: noted after each draw; upstream: tracked at the clamp) | upstream's tracking | the same barriers; nothing depends on each draw reporting back |
| Cheaper indirect clamps (ours: a ring behind one enhanced barrier; upstream: one budget charge per group, the budget in the card's memory) | both: the ring stays the default, upstream's clamp program and budget serve both paths (the program also runs this fork's split dispatches, and counts compute records per group) | they cut different costs |
| Windows of indirect records (ours: on by default; upstream: off, three device losses with them on, then a fix) | on, with upstream's fix: a draw record of 2^23 vertices an instance or more runs empty | measured in the second pass; `VICTORY_INDIRECT_WINDOW_CHECK=1` now works with the ring too |
| Spare texture staging buffers (ours: classes of an eighth, 1 GiB; upstream: powers of two up to 8 MiB, 256 MB) | ours (`VICTORY_STAGING_POOL=0` turns it off as well) | measured first here, wastes less, pools every size |

Upstream's alone, as it shipped them: the submit thread
(`VICTORY_ASYNC_SUBMIT=1`, off; with it a page remap first sends what the
queues hold and may run, so that it is prepared from title memory as it
stood), fills of upload memory and read-only buffer copies that no longer
wait for flights (on; the first fill that read bytes a flight then changed
turns the waits back on, and says so in the log; `VICTORY_FILL_WAIT=1`
waits from the start), 512 idle host lists,
`VICTORY_MERGE_GAP`/`VICTORY_MERGE_RUNS`, `VICTORY_PROFILE` (diagnostics).

Not timed in a game run on this machine yet (the runs go through the
general test of the merged build). Checked on the software adapter
(`VICTORY_D3D12_WARP=1`, diagnostics): upstream's test of the fetch beside
a later flight fails on this fork's fetch as it stood and passes on the
merge (and fails again with `VICTORY_QUEUE2_REFETCH=0`); the end-to-end
test of the copies in the card's memory gives the same title memory with
`VICTORY_QUEUE2` at 0, 1 (the default) and 3; the indirect tests, the
window check and the draws with no barrier of their own included, run on
both the ring and the scratch buffer.

To run it: `Lancer GTA6 (VIctory).bat` next to the game's files builds the
release and starts the title.

### Picture checks (2026-11-25, RTX 5090, diagnostics builds)

Captures of the opening cutscene (a new game) and of the save's gameplay,
before and after the merge of the upstream fork, traced to the passes that
make them with the frame listings and target dumps:

- **Bright blocks** (white, blue, pink, green 16-pixel tiles, mostly along the
  bottom of the picture; "NINE MONTHS AGO", the prison yard, the diner): made
  by the screen-probe indirect light (`indirect_diffuse_lighting`:
  `CS_FinalGather_*`, then `CS_ScreenProbes_TemporalResolve_*`) and by the
  per-material `CS_RaytracingResolve` passes, whose half-size output is
  scrambled into blocks over the whole picture. Both shade what the title's
  own ray traversal returns, and the structures it traverses are not built:
  300 to 1,400 builder dispatches a frame are dropped while the builder is
  off (six listed cutscene frames; 632 in a gameplay frame). Rays
  that find nothing take the sky's light (`CS_ScreenProbe_RayMiss`), and the
  probes near the bottom edge, whose screen-space traces leave the picture
  first, are the brightest. Not a fault of the layer's copies or views: it
  goes with the builder (see the ray-tracing notes). The white eyes of close
  faces are probably the same miss light (`ped_standard_eye` has a resolve
  pass of its own); not traced further.
- **Hair drawn as fans of strands and a face pulled out of shape** (the
  long-haired man in the night street shots) and **rooms without some of their
  walls**: present before the merge, not seen in the same shots after it
  (one run each; the upstream fork's writes through other views of a page and
  byte-copied descriptor records).
- **Black frames**: every presented frame is now probed (diagnostics), so a
  black frame is told from a fade by the frames around it. In a five-minute
  cutscene run, three: two cuts through black between loading screens before
  the world, and one fade of the cutscene (255 down to 79 over five frames,
  black for two, then the next shot). In the save's gameplay run, two: a cut
  between loading screens, and the world's very first frame (4,996 draws),
  black once before the picture comes up over the next frames (2, 46, 169,
  91, 92...); its listing was not captured, so that one is not traced yet.
  None in the cutscene went black for a single frame.
- **Washed-out frames after cuts** (the white "NINE MONTHS AGO" shot): the
  title's small exposure targets show the scene's luminance jump about fifty
  times while the exposure-like value stays put (0.054 to 0.063), and the
  picture settles within about 25 frames: the title's eye adaptation, which
  at five frames a second has few frames to do it in. Not a fault found in
  the layer.
- **Depth bounds**: the upstream fork's test (#82) now also has a GPU test of
  the mesh path. In eleven listed frames (seven of the opening cutscene, four
  of the gameplay) the title set the bounds 1,595 times, always to 0 and 1,
  which pass every depth: the test changes nothing in those scenes.
- **Views of another shape over a texture the layer writes** ("what the host
  wrote is not what it shows"): every pair logged in more than fifty runs is a
  block-compressed streamed texture whose memory begins 16 to 672 KiB before an
  RGBA8 render target and covers its first part. That is memory the title
  reuses, not a reinterpretation (which would start at the same address, as
  the probes' R32_UINT/R9G9B9E5 pairs that are copied do): what the view must
  show is what the title last wrote there, title memory. Showing the target's
  texels would decode RGBA8 pixels as BC blocks. Measured in one cutscene run:
  of 8.3 million reads of placed views, one met a target the layer had written
  in the same or the frame before; in a gameplay run none, and six of the
  eight targets met there were never written by the layer at all.
- **Buffer views**: a raw view that does not start on 16 bytes of its buffer's
  copy now gets a copy of its own bytes, as views off their elements already
  did (it was left null: zeros); a view of no elements and a shader's address
  of a null view read as on the console (zeros, and address 0), and so do the
  placed unordered texture views left null: every one seen had no memory
  (address 0). In the runs after these changes no buffer view was left null,
  and 196 raw views of the gameplay run were made on their own bytes.

### Picture checks, second round (2026-11-25 afternoon)

- **Tiles left out of the screen-probe indirect light**: the final gather of
  the indirect diffuse light (`indirect_diffuse_lighting:CS_FinalGather_*`)
  is an indirect dispatch of one group per 8x8 tile of each of two layers at
  2560x1440, up to 2 x 57,600 = 115,200 groups in x; one host dispatch runs
  at most 65,535. The clamp of indirect records cut such a record to 65,535,
  so the groups past it never ran: 0.3 to 3.5 records a frame in the
  opening, about one a frame in the save's gameplay (68,952 to 75,816
  groups asked there, in a static shot). With the tiles in reading order, a
  cut leaves everything after one point of the picture without its gather:
  the rows below it, and the end of its own row. That is the shape of the
  blocks this section's first round put on the missing ray-tracing
  structures (blocks "mostly along the bottom of the picture"), and of the
  kitchen shot's blinds, unprocessed below a row and right of a column.
  Compute programs now count their groups in x from a base the host gives
  them (a user-data word, `USER_GROUP_BASE`; not programs with a spilled
  user-data table, whose root signature is near the host's 64 dwords), and
  a dispatch of more groups runs as several: a direct one as several
  dispatches, an indirect one through records the clamp splits in four
  (base, counts), up to 262,140 groups in x. The two final-gather programs
  named are now run whole; no pipeline was refused for the extra word. In
  the save's gameplay (the inmate at the cell window, the same static view):
  before, about 0.8 records a frame were cut and blue and white blocks lined
  the bottom edge, and the hair and the head's half over the window took a
  washed-out orange light with a vertical seam (taken for frosted glass
  drawn over the head); after, 166 frame lines asked for more than 65,535
  groups and none was cut, the bottom edge is clean and the hair is lit as
  the rest of the room (the build after also has `integration`'s newer
  commits; the cut was the one thing measured to stop).
- **The opening's later montage** (a woman on a field and over a valley at
  dusk, a terrace, an aerial flight, 280 to 360 s after the world's first
  frame): flat untextured ground and low-poly figures and buildings (their
  lowest levels of detail), the lower part of the picture unlit or in
  blocks. A build of `main` (no upstream merge) shows the same shots the
  same way (one run of each), so they do not come from the upstream changes;
  in some runs of either the montage holds a shot for tens of seconds while
  the streaming catches up. The blocks and unlit bottom match the cut final
  gather above; the low detail is the streaming's (not traced further).
- **New diagnostics**: `VICTORY_CAPTURE_SECONDS=<s>,...` lists (and with
  `VICTORY_DUMP_TARGETS` dumps) the first frame past each time after the
  world's first frame (the opening keeps time with its sound: frame numbers
  name other shots in every run). Note that target dumps of a listed frame
  take 15 to 20 s each and the cutscene stalls on its shot meanwhile, and
  `VICTORY_CAPTURE_WORLD` lists four frames of its own: a run with dumps
  is not timed like one without. The listing now shows mesh pipelines'
  depth, stencil and blend state and the title's clears; a texture the host
  writes made in the memory of another one alive is logged (in the opening
  only shadow-map depth textures of a pool overlap that way, re-rendered
  each frame); a program whose indirect dispatch asks for more groups than
  run is named.

### Known problems

- Everything in shadow is black (see above).
- The interior of the first gameplay room is sometimes not drawn after the cutscene hands over to gameplay.
- Around 24 GB of memory is in use during play.
- A few style lints are switched off for this fork in the workspace `Cargo.toml` until the newer code is tidied.
- Many changes here are specific to this title and this GPU, and some are marked `shortcut:` in the code where they approximate what the console does.

Everything below this line is upstream VIctory's own README, unchanged; its
status section describes upstream, not this fork.

---

# VIctory

Runs the title (Xbox Series X|S) natively on Windows, from files
the user supplies.

The title's x86-64 code runs unchanged inside our process; VIctory supplies
the platform underneath it and converts the title's data, ahead of time where
it can and on first use where it must (shaders). See
[docs/architecture.md](docs/architecture.md).

## Status (2026-11-23)

**Not playable yet.** The title boots, runs its menus at its own 30 fps and
loads the world, where rendering fails.

| Stage | State |
|-------|-------|
| Boot, first-boot screens (accessibility, language, settings, legal) | works, 30 fps |
| Intro video with sound | works |
| Start screen, profile and saves | works |
| Loading into the world (about 50 s) | works |
| The world (first mission's intro) | reached and runs with indirect draws; its title card renders, the scene after it is still dark; 0.5-1.8 s per frame |

Tested on one machine: Windows 11, AMD RX 6700 XT.

> **Warning.** Entering the world on a real GPU can currently hang it, and
> Windows then resets the display driver (every monitor goes black for a
> moment). To look further, use a diagnostics build with
> `VICTORY_D3D12_WARP=1`, which renders on the software adapter.

What each part does today and what it still needs:

| Subsystem | Today | Needs |
|-----------|-------|-------|
| Loader | Runs the title's executable and its DLLs in our process; crash reports name the faulting code and are kept in `crashes.log` | â€“ |
| Runtime services | One signed-in offline user, licences, packages, saves that survive a kill at any step, text entry dialog | Speech is silent; the dialog and relaunch are untested live |
| Network | Offline by design: the title cannot reach the network | â€“ |
| Files and streaming | Archives and streamed data read, zlib and BCPack decoded in parallel | Loading is limited by writes into title memory |
| Input | XInput pads, keyboard, keyboard stand-in pad | Trigger rumble |
| Audio | Menu, video and loading sound; XMA voices and Opus streams decode | Reverb is silent; Opus results unverified against the title |
| GPU: menus | Draws, compute and presentation on host D3D12; only changed title memory is copied | Frame pacing drifts on one screen in release builds |
| GPU: world | Mesh pipelines, shader-written buffers, descriptor lifetimes, indirect execution (arguments clamped), pipelines made by worker threads and kept on disk | Most of the scene is still missing (untranslated shaders drop draws); frames take seconds; real queries, depth bounds, HDR and scaling are missing |
| Shaders | Console GPU code recompiled to DXIL at first use, cached on disk. Menus: all. World: mesh 8,191 of 8,506, vertex/pixel 10,617 of 12,346, compute 1,643 of 2,539 | 64-bit image atomics, the ray-tracing builder's shaders; geometry stages translate but no run has drawn with one; some compute programs need a GPU with 64-lane waves |
| Textures | Console tiled layouts and detiling for the modes the title uses | Render-target metadata planes |
| Series S mode | Runs through the menus to the loading screen | The world, as above |

## Getting started

Requirements: Windows 10/11 x64, a CPU with AVX2, a Direct3D 12 GPU with
shader model 6.6 (and mesh shaders for the world), Rust (pinned in
`rust-toolchain.toml`).

The title also needs several GiB of available Windows commit at startup.
Check **Committed** under **Task Manager → Performance → Memory**. If
`crashes.log` reports Windows error 1455, close other applications or increase
the Windows paging file size, then retry.

```powershell
cargo xtask install-hooks                # asset-policy check on every commit
copy victory.example.toml victory.toml   # then set your paths (outside the repo)
cargo test --workspace                   # needs no game data
cargo run -p victory -- config           # check your paths
```

Run the title:

```powershell
cargo build --release -p victory
target\release\victory.exe run <game dir>\<the title's exe> --execute
```

The window caption shows the framerate. Saves, settings and `crashes.log`
are kept under the data folder (default `%LOCALAPPDATA%\victory\data`), derived
data under the cache folder.

Input: XInput pads. With none connected the keyboard is a pad while the
title's window has focus:

| Keys | Pad |
|------|-----|
| W A S D / I J K L | left / right stick |
| arrows | D-pad |
| Enter, Backspace, X, Y | A, B, X, Y |
| Q / E, Z / C | shoulders, triggers |
| F / H | stick clicks |
| Esc, Tab | Menu, View |

## Commands

```
victory inspect <exe|dll> [--imports] [--exports]
victory census [paths...] [--missing] [--strict]
victory lib-exports <import.lib> [--list]
victory shader-info [paths...] [--verbose] [--disasm]
victory translate [paths...] [--validate] [--pipelines] [--list] [--jobs N]
victory run <exe> [--execute] [--unimplemented trap|zero] [--strict]
            [--console series-x|series-s] [-- <title arguments>]
victory config
```

Logging: `VICTORY_LOG=victory_loader=trace,info` (a release build logs
info and above).

## Debugging

A release build has no debugging aids. Build them in with

```powershell
cargo build --profile diag -p victory --features diagnostics   # target\diag\victory.exe
```

| Variable | Effect |
|----------|--------|
| `VICTORY_LOG=<target>=debug,...` | debug and trace output |
| `VICTORY_STACKS=20,60` | every thread's stack at those seconds |
| `VICTORY_WATCH=<rva>,...` | logs a title u32 when it changes |
| `VICTORY_KEYS=30:Enter,35:X` | presses keys at those seconds, focus or not |
| `VICTORY_CRASH_SAVE=<rva>:<len>,...` | saves title memory to the cache folder on a crash |
| `VICTORY_CAPTURE=<dir>` | saves presented frames (powers of two, or `VICTORY_CAPTURE_FRAMES=<list>`) |
| `VICTORY_DUMP_TARGETS=1` | with the two above, saves every render target of those frames |
| `VICTORY_D3D12_DEBUG=1` | host debug layer |
| `VICTORY_D3D12_GBV=1` | with the above, GPU-based validation (bad descriptor use reported, not faulted; slow) |
| `VICTORY_D3D12_DRED=1` | on a GPU failure, the allocation hit and the commands in flight, on every queue |
| `VICTORY_HANG_WATCH=0` | no watch of the card's progress (by default a send to the host queue the card stays on for 500 ms or more is logged with its lists' pipelines) |
| `VICTORY_PIPELINE_WAIT_MS=24` | reduce the per-frame wait for background pipeline creation from the default of 500 ms. A draw can be omitted until its pipeline is ready |
| `VICTORY_SKIP_PIPELINES=<hash>,...` | omit draws and dispatches, including indirect ones, whose pipeline contains a listed 16-digit shader hash |
| `VICTORY_MESH_LINK=0` | a mesh pipeline whose mesh program's outputs pass 32 KiB is made of the shared translations, as before, not of the pair linked for it (or, without a pixel program, of the mesh program with its system values only) |
| `VICTORY_GPU_TIMING=1` | with the above, each list of a submission run and timed on its own; slow lists logged with their slowest commands |
| `VICTORY_D3D12_WARP=1` | host device on the software adapter (slow; cannot reset the driver) |
| `VICTORY_SHADER_LOOP_LIMIT=<n>` | translated shader loops whose end the data decides stop after `n` iterations per lane instead of their stage's limit (own shader cache entries); `pixel:<n>,vertex:<n>,compute:<n>,trace:<n>` (any of them) for those stages alone; if a lower `n` makes world frames much faster, loops hitting the limit are what makes them slow |
| `VICTORY_HELPER_OWN=0` | helper lanes of pixel shaders read the host wave's first lane in the title's first-lane reads (waterfall loops) instead of their own value, as before the fix (own shader cache entries): for telling what the fix changes on screen |
| `VICTORY_LOOP_NOTES=1` | translated loops count the lanes that reach their limit (own shader cache entries); every five seconds the log names the programs and loops whose counts grew (`loop notes:` lines; helper lanes of pixel shaders are never counted) |
| `VICTORY_AUDIO_CAPTURE=<file.wav>` | records what the title plays (our process only) |
| `VICTORY_XMA_DUMP=<dir>` | saves the first 32 XMA voices' context and input |

Every build appends crashes and host GPU failures to `crashes.log` in the
title's data folder.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
