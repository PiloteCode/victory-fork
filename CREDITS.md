# Credits

PiloteBox is a tree of other people's work with ours on top. This file says
exactly what was taken from where. Everything listed is licensed
GPL-3.0-or-later unless noted, as this repository is.

The history of this repository is ours: one commit for each step of our own
work. The upstream trees came in as merges; their own per-commit histories are
in the repositories linked below, at the commits named.

## The project

- **[VIctory](https://github.com/Marvisak/VIctory)** by Marvisak and its
  contributors. The project itself: the loader for the title's image, the
  runtime services, the Direct3D 12 back end and the shader recompiler.
  Its code is the base of every tree below. Pull requests of that repository
  that we ported ourselves are named in our commit messages by their number
  (early depth after its #2085; mesh primitive indices after its #1718;
  normalized ends after its #169).

## Trees merged into this one

- **[fxceless/victory-fork](https://github.com/fxceless/victory-fork)**: the
  fork our own work started on (our versions 0.1 to 0.11), and the one our
  pull requests go to. Carried from its pull requests, by number and author:
  - #927 by tippythetaidum: 16-bit typed buffers read as zeros on NVIDIA (the
    defect behind mouths that do not move). Our translator's own fix follows
    its diagnosis.
  - #995 by tunczzz: saves stopped by error 4395, stale textures after
    DirectStorage writes.
  - #990 by ryotdev: copies that fail instead of faulting.
  - #964 by avt0m4t48: removed-device copy jobs not counted as finished.
  - #959 by Sashwiin Somu: an unreadable pipeline library is left alone.
  - #1060 by gmanka: split placeholders joined before a mapping retry.
  - #952: a files test that deletes its file again for up to a second.
  - A fix by BradRoland to our node tests (review of our pull request #994
    there), taken as his commit.
  - A fix by HyperEndgame for structure views at a fixed place of a table
    (a comment on #994 there): the same change is in the layer below; the
    test is ours.
- **[avatarmanz/VIctory-7900XTX](https://github.com/avatarmanz/VIctory-7900XTX)**:
  **the layer this tree is built on**, merged at its commit `0a435665`. From
  it: list calls written down and translated by threads of the layer, a
  thread that prepares submissions, flights that are not settled, the relay
  and landing threads, steps done on the card, textures on a copy queue, the
  frame recorder and replay, and its merges of the trees named in its own
  credits. Also taken: its commit `9ba5661f` (replay of buffers placed in
  heaps opened from memory), and its translator's reading of the upscaler's
  containers (its commits `66afb2b4` and `389db9ef`), which ours follows.
  Merged again at its commit `014d52de` (our wave 13). What came with it,
  by author as that history names them (its translator's changes are not
  here: ours is kept whole):
  - avatarmanz: the renderer ports 1 (`VICTORY_PORT_CONSTANTS`), 2
    (`VICTORY_PORT_DESCRIPTORS`), 5 (`VICTORY_PORT_CBV`, which needs its own
    translator and does nothing with ours), 6 (`VICTORY_PORT_GUARD`), 7
    (`VICTORY_PORT_JOURNAL`) and RT (`VICTORY_PORT_RT`); its merge of
    codec-d/cool_fox's parts (the residency budgets, tables planned once per
    pipeline and heaps of one slot, the second plane composed, the heap
    buffer pass per heap, XMA voices refilled); copies in the card's memory
    brought up on a copy queue; heap records copied as bytes; output size,
    render scale and frame rate switches; light and texture probes, a store
    of larger texture levels; the stand-in for the indirect light with ray
    tracing too.
  - beloooo (Belobug): title textures uploaded in a submission's prelude,
    decided at submission after the refresh; the RX 9070 XT work (a guard for
    the driver's compiler crashing on a worker) and the HUD panel; PC
    FidelityFX FSR 4.1.2 to 4K, ported from DexNom.
  - gomeaw: structured buffer slots read as raw bytes on hosts without
    descriptor twins (now behind `VICTORY_STRUCTURED_RAW=1`, off), clamp
    arguments declared volatile, legal states in the replay fixture, where
    pipeline workers spend their time.
  - Jake: our threads on the cores of the largest last-level cache
    (`VICTORY_CACHE_CORES`). ESAT: sampler heaps of 2,048 slots with a null
    sentinel; replay tests on capped view heaps. Diogo Sousa: timing runs by
    kind of list. Yoshi-233: packed 16-bit diagnostic targets decoded, the
    pipeline book's flushes and retries. sharan104: `VICTORY_WRITE_SITES`
    kept out of a source tree, and xtask's check that the switches the
    documents name are read. zocram4cc: guard pages in copies. UwUCammie: a
    frame counted once its present is accepted, buffer slot scans masked.
    Lux-Designs: replay's clamp scratch copy. FictionHub: DRED's stop
    command. Adika713: the memory category log. feanorsun: census, file
    scan, storage quota and warm worker reports. mattkajorinc: lint fixes.
- **codec-d/cool_fox** (an archive of its commit `f4b48ad7`; the repository
  was not public when we looked): work on an RTX 5090 on the same base.
  From it: a submission gives its tables' dynamic descriptor slots back when
  the card has run it, a settle and a second try before a table is refused,
  one oversized indirect record no longer empties the draws after it, the
  two presented planes of the Series S profile composed into one image,
  host-memory admission for texture staging, bake uploads done at once. Its
  own journal is kept as `docs/bug-journal.md`.
- **TaynanGT's tree** ([github.com/TaynanGT/Vice-Tay](https://github.com/TaynanGT/Vice-Tay)),
  merged at its commit `4ee1a47b` (wave 14): avatarmanz's layer at `126acb35`
  tuned on an RTX 4090, with the pull requests it took in. Its translator
  changes are not in this tree (ours is kept whole); its own documents are in
  `docs/taynangt`. Taken, by author:
  - TaynanGT (Taynan Piza), with the work of that tree's own commits:
    dynamic slots given back when a flight lands and only to the heap that
    took them, the slot flip that keeps its record, placed views keyed
    without the words the title leaves unwritten (and the cap on views no
    resource releases), small pooled textures at 4 KiB outside AMD cards,
    the switch for structured views off 16 bytes, shadow refreshes that keep
    the page lock through the copy, readback and write-watch work without
    allocations, decoration bake coverage by heap, PC FSR 3.1.6 and DLSS for
    the Series S profile and DLSS to 4K, AMD frame generation, full screen
    and the keys file, saves with a blob gone, XAsync and close fixes,
    texture allocation retries, a failed shader cache write that keeps the
    translation, the opt-in Agility SDK build.
  - Vladimir: the AVX2/SSE2 micro-tile detiler and its pool; the bitmask of
    uncopied pages, lock-free mirrors and an AVX2 `Shadow::take`.
  - KGAT1W: title buffers of 64 KiB made before the world. zzzz: the card
    reservation for buffer budgets (`VICTORY_CARD_RESERVE_BUDGETS`).
  - beloooo (Belobug): the HUD panel's later form; magenta left in the
    probes' memory is not light. CowboyBingus: the hold of traced reflections
    to the light around (`VICTORY_AMBIENT_REFLECT`).
  - pawcijo: DualSense over USB and Bluetooth. superboynick: relative mouse
    input. Geckomacabre: the monitor to play on. Maksim Gorbunov: the
    interface plane at 4K over a 1080p output. nidscode: a ceiling on the
    game's working set (`VICTORY_WORKING_SET_MB`).
  - meshahid973: warm workers that survive failures, PE relocation bounds,
    the device-loss range pattern.
  - alex (nullexxx): `VICTORY_LOG_FILE` (in our own form), vkd3d's empty
    pipeline library, `VICTORY_SKIP_PIPELINES` in release builds.
  - Filip Michalski: pipeline workers that survive a panic. loyodea: no
    worker count of zero. edsuuu and fxceless: pending texture changes kept
    when queues fill, GPU-written texture views after a CPU upload, a view
    moved to the first level with memory. Pototoooo: pipeline cache saves
    retried. loftymocha: the DLSS library looked for before NGX loads.
    KainanP: held title-window copies counted against the memory budget.
    Flare: heap samplers keep point mips when anisotropic. RVZO6: BC7
    channel rotation in BCPack. Garu667: the profiler's stack walk bounded.
    gomeaw: readback destinations kept until their flight lands. feanorsun:
    the idle texture sweep restarted. welisonzika14-jpg, efsebayern,
    kripotker, rafaconvaitis, spk, anes66 and cwWwWwW: snapshot pool,
    texture copy and descriptor fixes and their tests.
  - Through it, from trustno1-coder/VIctory-NVIDIA: pipelines with static
    samplers made by the driver on NVIDIA and helpers woken by need
    (readmet3xt-lab), the fault-capture reader, descriptor counts by kind
    (LucaruluDev), the optional mimalloc build (Dastano).

## Through those trees

The trees above carry work that their own credits name; we repeat them here
because that code is in this tree:

- **[DexNom/VIctory](https://github.com/DexNom/VIctory)**: ray-tracing fixes,
  cube gradients, sparse textures, and the bridge to PC FidelityFX FSR 4.1.2
  at 4K (its commit `ab34a8b2`).
- **[Belobug/VIctory-Belobug-AMD](https://github.com/Belobug/VIctory-Belobug-AMD)**:
  the stand-in for the indirect light, pipeline workers, and work by
  romainhedouin.
- **[FQY55kai/ZJOU-VICTORY](https://github.com/FQY55kai/ZJOU-VICTORY)**.
- **[myokex/victory-amd](https://github.com/myokex/victory-amd)**: the guard
  over every view of a page (with Spexivus, joroyt and tightfall), and Joshua
  Odom's reads into title memory.
- **Geckomacabre** and **cxy0072019-sketch**: the texture read-back.
- The authors of the pull requests merged in fxceless/victory-fork and in
  VIctory, who are in those repositories' histories.

## Read, with one fix taken

- **[stellarr-studio/kelvaroq](https://github.com/stellarr-studio/kelvaroq)**
  (Stellarr Studio SAS and contributors): its published patches were read
  for fixes worth taking. Taken: the fix of its patch 0009 for the parallel
  compare's first cut (a big compared range lost its first slice when its
  copy did not start on a 64 KiB piece), with a test of ours. We also sent
  that fix to fxceless/victory-fork (pull request #1407 there), which its
  maintainer merged by hand in its commit `81d54b06`.

## Read only

- Hardware measurements of RDNA2 behaviour quoted in the translator's tests
  come from a community project, through VIctory's own history.
- Another compatibility layer for the same console (GPL-2.0-only) was read
  for how one title behaves under it; none of its code is here.

## Ours

The shader translator's forms for 32-lane GPUs, the fixes named in our commit
messages, the PiloteBox commands and this repository's documents are by
Pilote Production and the people who contribute here. Our changes to the
trees above are sent back as pull requests where they apply.

If something of yours is here and is not named, or is named wrongly, open an
issue: we will fix this file the same day.
