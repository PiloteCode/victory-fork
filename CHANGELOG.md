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

## 0.12.2 (2026-11-27)

The upscaler's containers, and the commands for playing.

- **The Series S profile shows a picture.**
  - The upscaler's 20 compute programs, kept in containers of their own,
    translate. This follows avatarmanz's translator.
  - The scene is drawn at 960x540 in our captures, so this is the fast, soft
    mode.
  - In our first measurement it ran about twice as fast as the Series X
    profile, with the upscaler's passes still missing.
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

- **Mouths move on NVIDIA cards.** 16-bit typed buffer accesses through a
  table's places now move their bits as unsigned integers.
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
  - a texture's own bytes;
  - stale textures after DirectStorage writes;
  - saves and error 4395;
  - keys by their places;
  - the pipeline library;
  - split placeholders.
- **The play settings are `run`'s defaults.** `VICTORY_PLAY_DEFAULTS=0` gives
  the layer's own, and `=safe` gives the script's `-Safe` set.
- **Builds** target x86-64-v3, with thin LTO and one codegen unit.
- **Our older layer (0.5 to 0.10) stays in the history** behind the first
  parent. Its switches that are not named above went with it.

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
