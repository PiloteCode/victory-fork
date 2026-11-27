# Changelog

What each version of PiloteBox brought, newest first. A version is a wave of
the fork's work: `0.12` is wave 12, and the steps after it are `0.12.1`,
`0.12.2`. Earlier waves are named by their merge commits ("Make wave N the
version to play"); waves not listed were working waves merged into the next
one.

Every figure says what it was measured on. Unless it says otherwise:
- the card is an RTX 5090 on a PCIe 5 link, with an i9-14900KF;
- the build is a diagnostics build;
- the profile is the Series X one;
- the scene is the prologue's first playable scene, about 4,800 draws a frame.

## 0.13.0 (2026-11-27)

Waves 13, 14 and 15: avatarmanz's newer layer, TaynanGT's tree, title
buffers on the card's side, and the cause of the one settle a frame. The
version also takes 13 and 14, which were working waves. Our translator is
unchanged apart from its new lists of written and atomic views: all 25,953
programs still translate and validate.

### On our card

**14.7 frames a second in that scene, not 30.** One run of four minutes per
row, measured while other builds ran, with the process at high priority.
Diagnostics build of wave 15, about 4,850 draws a frame:

| Placement | A frame | The thread that prepares submissions | The card |
|---|---|---|---|
| As before | 89.2 ms (11.2 fps) | worked 80.6 ms | work queued about 89 % of the frame |
| Title buffers on the card, windows through the bus | 75.5 ms (13.2 fps) | worked 54.4 ms | work queued 51 ms |
| Title buffers on the card, two-sided windows (the new default on NVIDIA) | 68.0 ms (14.7 fps) | worked 63.9 ms | work queued 34 ms |

For reference:
- 0.12.2 (wave 12) took 86.9 ms a frame on a quiet machine.
- Wave 13's layer took 171 ms before its structured-view rule went behind a
  switch.

**The card is no longer the bound: it waits for us 23 ms a frame. The
processor is.**
- One thread of ours prepares the title's submissions and sends them. It
  works 64 ms of the 68 and sets the frame: the title's threads only hand it
  their submissions, and wait 35 ms at the present for it.
- Its parts, a frame: about 44 ms preparing (the write watch 5, the title's
  bytes put under flights' writes 4.5, the lists' steps 19 with the refreshes
  of host copies at 12.6, copies in the card's memory 11), about 10 ms
  planning the steps done on the card, and 14 ms between submissions, giving
  back the lists that have run.

- **The card's work, halved by placement** (sync12).
  - The commands' times on the card went from 53.8 ms a frame with the
    placement of before to 26.6 ms with every title buffer and the windows of
    title memory on the card's side.
  - Dispatches went from 29.9 to 7.5 ms.
  - The two shadow-mask programs went from 5.6 and 2.1 ms to 0.2 and 0.3 ms.
  - `VICTORY_CARD_SIDE` is now on by default on NVIDIA cards (`=0`: the
    placement of before). Elsewhere it is off unless `=1`: it is not measured
    there.
  - Its sizes follow the card's budget (crates/victory-gpu/src/shadow/
    placement.rs). The 8 GB and 12 GB rows are guesses still to be measured.
  - The windows are two-sided copies. `VICTORY_WINDOWS_BAR_MB` (windows
    through the bus) stays a switch of its own.
- **Loops are not the card's time.** Of the 746 programs drawn with counted
  loops, one pixel program reaches its iteration limit. No compute program
  does.
- **The one settle a frame, found** (sync12). A 240-byte buffer copy sits at
  its place in a list, and a draw of the same list reads it as constants. The
  fill of upload memory for those constants waited for every flight.
  - `VICTORY_FILL_FORWARD` (on) makes such a fill read the copy's source
    instead, on the card with the other steps, when nothing of the submission
    writes those bytes in between.
  - It forwarded 19 fills a frame, and 0.14 submissions a frame were still
    refused and waited.
  - With the placement on, the settle's wait is 1.2 to 1.5 ms a frame. It was
    38.7 ms in 0.12.2.
- **Structured views off 16 bytes stay on their buffer's copy**, as in every
  wave up to 12. `VICTORY_STRUCTURED_RAW=1` brings back the rule that came
  with wave 13's layer (gomeaw's, for hosts without descriptor twins).
  - With the rule, 2,270 views a frame went through one lock in
    `shadow::exact`, which made 400 new copies a frame.
  - List translation took 1,249 ms a frame instead of 156.
  - Without it: about 200 ms of translation, 1.7 views a frame asked, and
    the picture is right.
  - TaynanGT's tree found the same on an RTX 4090, and has the same default.

### Wave 15: the placement and the settle (sync12, ours)

- `VICTORY_CARD_SIDE`, on by default on NVIDIA cards, as described above. The
  texture queue keeps its room, and each buffer's own switch still wins.
- The finer switches it is made of, all off:
  - `VICTORY_CARD_WRITTEN_KB` and `VICTORY_CARD_ATOMIC`: buffers that shaders
    store to or do atomics on;
  - `VICTORY_CARD_UNORDERED_KB`: buffers the title made for unordered
    access;
  - `VICTORY_CARD_JOIN_GAP`: a copy's ranges joined over gaps the card
    already holds.
- The translator names the table places and root views a program writes or
  does atomics through (`atomic_views`, `written_views`, `picked_views`;
  gpu-shaders3, ours). A bind then asks for that buffer's copy as such.
- The frame line says what goes up to copies in the card's memory, and which
  steps a copy at its place keeps off the card.

### Wave 14: TaynanGT's tree

- **Bookkeeping of host copies** (Vladimir): a bitmask of uncopied pages,
  lock-free mirrors and an AVX2 `Shadow::take`. Refreshes now keep the page
  lock through the copy (TaynanGT), so a refresh no longer lays older bytes
  over newer ones.
- **Textures:**
  - an AVX2 detiler with a pool for big textures (Vladimir);
  - small pooled textures at 4 KiB outside AMD cards (`VICTORY_POOL_SMALL`);
  - allocation retries, and pending texture changes kept when queues fill
    (edsuuu);
  - GPU-written texture views after a CPU upload (fxceless).
- **Long sessions** (TaynanGT): placed views are keyed without the words the
  title leaves unwritten. `VICTORY_OWNERLESS_VIEWS` caps the views that no
  resource releases at a million.
- **Card budgets for small title buffers**, switches that are off:
  - `VICTORY_CARD_BUFFERS_MIN_KB`, `VICTORY_CARD_BUFFERS_MB` and
    `VICTORY_CARD_WINDOWS_MB`, with `VICTORY_CARD_RESERVE_BUDGETS=1` (zzzz,
    TaynanGT) so that textures leave that room;
  - `VICTORY_TITLE_PREWARM_COUNT` (KGAT1W).
- **Upscaling and frame generation for the Series S profile** (TaynanGT; off,
  and the libraries are the user's):
  - `VICTORY_FSR=quality|balanced|performance`: AMD FSR 3.1.6, with
    `PILOTEBOX_FIDELITYFX_DIR`;
  - `VICTORY_DLSS=...`: NVIDIA DLSS, with `nvngx_dlss.dll` in
    `PILOTEBOX_DLSS_DIR` or the driver's own copy;
  - `VICTORY_FRAMEGEN=1`: AMD frame generation, with `PILOTEBOX_FFX_FG_DIR`;
  - `VICTORY_DLSS_4K=...` for the Series X profile.
- **Input and window:**
  - DualSense pads (pawcijo, `VICTORY_DUALSENSE`);
  - relative mouse (superboynick, `VICTORY_GAMEPAD_MOUSE=1`);
  - the monitor to play on (Geckomacabre, `VICTORY_MONITOR`);
  - `VICTORY_FULLSCREEN=1` and F11, and `VICTORY_KEYS_FILE` (TaynanGT).
- **Fixes:**
  - slots given back only to the heap that took them (cwWwWwW, TaynanGT);
  - a slot flip keeps its record;
  - a failed shader cache write keeps the translation;
  - pipeline workers survive a panic (Filip Michalski);
  - a save version with a blob gone is still read when it is the only one;
  - only the title's window ends the title on `WM_CLOSE`;
  - pipelines with static samplers are made by the driver on NVIDIA
    (readmet3xt-lab). `VICTORY_LIBRARY_STATIC_SAMPLERS=1` takes them from
    the library again.
- **Tools:** `VICTORY_LOG_FILE` (alex), `scripts\faultbook.ps1`, and the HUD
  panel with the card's load as Windows counts it (beloooo, TaynanGT).
- Their documents are kept in `docs/taynangt`.

### Wave 13: avatarmanz's layer at `014d52de`

- **Title textures uploaded in a submission's prelude** (beloooo), decided at
  submission after the refresh. This puts textures in frame order and stops
  the flicker.
- **The renderer ports** (avatarmanz), all off:
  - 1 (`VICTORY_PORT_CONSTANTS`), 2 (`VICTORY_PORT_DESCRIPTORS`), 6
    (`VICTORY_PORT_GUARD`), 7 (`VICTORY_PORT_JOURNAL`) and RT
    (`VICTORY_PORT_RT`);
  - port 5 (`VICTORY_PORT_CBV`) needs its own translator and does nothing
    with ours.
- **PC FidelityFX FSR 4.1.2 to 4K** (`VICTORY_FSR_4K`, beloooo after DexNom),
  and the RX 9070 XT work (beloooo).
- **33 community pull requests** that layer carried (see CREDITS.md).
- **Ours:**
  - textures at their full size by default (`VICTORY_TEXTURE_MAX=0`);
  - the texel noise of the own-bytes rule fixed;
  - the parallel compare's first cut (after kelvaroq's patch 0009; merged
    upstream as fxceless `81d54b06`);
  - tests of the indirect budget.

## 0.12.2 (2026-11-27)

The upscaler's containers, and the commands for playing.

- **The Series S profile shows a picture.**
  - The upscaler's 20 compute programs, kept in containers of their own,
    translate. This follows avatarmanz's translator.
  - The scene is drawn at 960x540 in our captures, so this is the fast, soft
    mode.
  - Its frame time with the upscaler's passes is not measured on a quiet
    machine yet (44 ms a frame before those passes ran).
- **The commands for playing.** `pilotebox.exe` is the program; the package
  is still `victory`.
  - **`setup`:** finds the title, writes `pilotebox.toml`, runs `doctor` and
    translates the shaders ahead.
  - **`play`:** finds the title with no path to type, takes the profile by
    name (`--mode x|s`, `--rt`, `--safe`), and says what it plays in one
    block.
  - **`bench`:** a timed run on a copy of the data folder. It ends by closing
    the window, sums up the layer's own frame lines, and writes `.md`,
    `.json` and the log. Its last line is a one-line summary for issues.
  - **`check`:** `doctor`, then a short `bench`, ending with PASS or FAIL.
  - **`doctor`:**
    - checks the card: driver, widest wave, view heap cap and PCIe link;
    - checks the rest of the machine: AVX2, memory, disk;
    - checks the folders, the cache, the title's files and any other running
      copy.
  - **`clean`:** lists old cache files and set-aside folders, and removes
    them with `--yes`.
  - **`migrate`:** moves from `%LOCALAPPDATA%\victory` to
    `%LOCALAPPDATA%\PiloteBox`. The saves are copied and checked, never
    removed.
  - **`report`:** writes one text file for a bug report, with user names
    taken out.
  - A bare `pilotebox` says where things stand.
  - `--json` works on `doctor`, `bench` and `check`.
- **`PiloteBox.cmd`:** a launcher for a double click.
- **Switches.** Every `VICTORY_*` switch is also `PILOTEBOX_*`.
- **The config file.** `pilotebox.toml` is read first, then `victory.toml`.
- **Folders.** Data and cache now live in `%LOCALAPPDATA%\PiloteBox`. The
  old `%LOCALAPPDATA%\victory` stays in use until `pilotebox migrate`.
- **Log lines.** On the console, the modules lose their `victory_` prefix. A
  play's log also goes to a file.
- **The window's caption** reads `PiloteBox | N fps M ms/frame D draws/frame`.

## 0.12.1 (2026-11-27)

Mouths, and shaders.

- **The fix for mouths on NVIDIA cards.** 16-bit typed buffer accesses
  through a table's places now move their bits as unsigned integers. It
  passes its tests on the card (all 65,536 patterns come back); it is not
  yet confirmed in a cutscene.
  - The switch is `VICTORY_HALF_BITS`, on by default.
  - Before, the face network read its tensors as zeros on NVIDIA.
  - This is a port of fxceless #927 by tippythetaidum.
- **Legacy multiplies are looked at again once every loop join is filled.**
  The switch is `VICTORY_LEGACY_LOOPS`, on by default.
- **Ray-tracing structures.** A structure's view at a fixed place of a table
  gives the structure's address, after HyperEndgame's fix in fxceless #994.
- **Frame replay.** Buffers placed in heaps opened from title memory are made
  again. A replay that loses the device names the list and command it
  stopped at.

## 0.12 (2026-11-27)

The new layer: avatarmanz/VIctory-7900XTX's layer at 0a435665, with our
shader translator, codec-d/cool_fox's fixes for NVIDIA cards, and our own
fixes.

- **Our shader translator.** All 25,953 of the title's programs translate and
  validate for cards of 32-lane waves too. Wave64 programs come in paired,
  split and local forms. The translator also covers geometry programs,
  linked mesh programs, and thread-limited dispatches.
- **Fixes from codec-d/cool_fox** (archive of f4b48ad7):
  - a table's slots are given back once the card has run its submission;
  - a settle and a second try come before a table is refused;
  - host-memory admission for staging;
  - the two presented planes are composed.
- **Our fixes the layer lacked:**
  - a texture's own bytes and stale textures after DirectStorage writes
    (both behind switches and off: the first put texel noise on textures in
    this layer);
  - saves and error 4395;
  - keys by their places;
  - the pipeline library;
  - split placeholders.
- **The play settings are `run`'s defaults.** `VICTORY_PLAY_DEFAULTS=0` gives
  the layer's own, and `=safe` gives the script's `-Safe` set.
- **Builds** target x86-64-v3, with thin LTO and one codegen unit.
- **Our older layer (0.5 to 0.10) is in this repository's history.** Its
  switches that are not named above went with it.

Measured:

| What | Figure |
|---|---|
| Frame time | 79-95 ms a frame (10.5-12.5 fps), the picture right |
| The same scene with the layer this replaced | 130-168 ms |
| Card time, in a timing run | 57.5 ms a frame: 31.7 ms in 3,812 compute dispatches, 20.8 ms in 4,471 indirect executions |
| How busy the card was | 75-84 % |
| Preparing a frame | 22-26 ms |
| The ray-traced light (`--rt`, off by default) | 333-425 ms a frame once the walk really runs: an RTX 4070 SUPER, a tester's measurement (fxceless #994) |

## 0.10 (2026-11-25)

- **No more texel noise on a character's skin.** A placed texture's own bytes
  now end only at a view written after its own.
- **Legacy multiplies are plain products where that is exact.**
- Title bytes under shader writes can go up by byte masks
  (`VICTORY_MERGE_MASKED`, off).

## 0.9 (2026-11-25)

- **Boats no longer sink.** The textures shaders write and the title reads
  on the CPU are copied back by default. This follows fxceless #999 by
  Geckomacabre.
- Lazy copy-back and the heap-sync parts of fxceless #995 (tunczzz) sit behind
  switches, all off.

## 0.8 (2026-11-25)

- **Others' fixes, from fxceless pull requests:**
  - copy jobs on a removed device are no longer counted finished (#964, by
    avt0m4t48);
  - kernel copies work when the copy fault handler cannot be registered
    (#990, by ryotdev);
  - an unreadable pipeline library is left alone (#959, by Sashwiin).
- **Fixes from fxceless #995 (tunczzz):**
  - saves are no longer stopped by error 4395;
  - the texture re-check reads only the texture's own bytes;
  - textures uploaded before a DirectStorage write are made due again.
- Switches to measure coherence, barriers and copy-back, all off.
- **Measured:** 132 ms a frame in the world, at PCIe gen 5.

## 0.7 (2026-11-25)

- **The official repository.** Its merge, and its pull requests, among them
  #2085 (early depth).
- **The official translator's ports:**
  - 9-bit mesh primitive indices;
  - normalized ends;
  - the cube gradient level;
  - five speed-ups.
- GPU upload heaps behind `VICTORY_GPU_UPLOAD_HEAP`, off.
- **Measured:** 26 % faster than 0.5 while the title streams, 5 % in steady
  play.

## 0.5 (2026-11-25)

- **Upstream fxceless** up to 0b0074e0, and the official repository's fixes.
- **Our translator:** 25,953 of 25,953 programs.
- The GPU-layer work and fixes of waves 1-5.
- **Defaults for speed:**
  - the structure builder off;
  - copies in place;
  - no longest tables.
