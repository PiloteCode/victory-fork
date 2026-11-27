# PiloteBox

**An Xbox Series (Scarlett) compatibility layer for Windows, tuned for NVIDIA cards.**

[![CI](https://github.com/PiloteCode/PiloteBox/actions/workflows/ci.yml/badge.svg)](https://github.com/PiloteCode/PiloteBox/actions/workflows/ci.yml)

PiloteBox runs an Xbox Series title's own x64 code natively on a Windows PC. It
is not an emulator of the processor: the title's code runs as it is. What
PiloteBox supplies is everything around it: a loader for the title's image, the
console's GDK runtime (`XGameRuntime`, memory, files, DirectStorage, input,
audio) on Win32, the console's Direct3D 12 (`d3d12_xs`) on the PC's Direct3D 12,
and a recompiler that turns the console's RDNA2 (GFX10) shaders into DXIL.

Its own subject is the cards the console's shaders were not written for: GPUs
with 32-lane waves (NVIDIA, Intel) and a view heap capped at 1,000,000
descriptors. The goal is plain: **the Series X profile, at full quality, at a
steady 30 frames a second.** It is not there yet; the numbers below say where
it is.

> **This repository contains no game files, code, keys or assets.** You need
> your own lawful copy of the title you run. PiloteBox is not affiliated with
> or endorsed by any console maker, developer or publisher. We do not help
> anyone obtain a game: requests for game files, keys, dumps or links are
> closed without an answer.

## Where it stands (2026-11-27, version 0.13.0)

Measured on one machine: Windows 11, GeForce RTX 5090 on a PCIe 5.0 x16 link,
Core i9-14900KF, 128 GB of RAM, a 2560x1440 display, a diagnostics build, one
Xbox Series title, automated runs of four to five minutes that start a new game
or load a save.

| Step | State |
|---|---|
| Loads and boots | yes |
| Menus, videos, loading screens | yes, 30 fps |
| In the world | yes |
| Playable | yes: a new game's opening and the prologue |
| Series X profile (full quality) | 68.0 ms a frame (14.7 fps) in the prologue's first playable scene, about 4,850 draws a frame, with title buffers on the card (the default on NVIDIA); 89.2 ms (11.2 fps) with the placement of before. Not 30 fps yet. One four-minute run each, while other builds ran |
| The card's work (Series X) | 53.8 ms a frame by command with the placement of before, 26.6 ms with every title buffer and the windows on the card's side |
| Series S profile (lighter, upscaled) | shows a picture since 0.12.2; its frame time is being measured again (44 ms a frame before the upscaler's passes ran) |
| Ray-traced indirect light (`--rt`) | runs, far too slow to play with today |
| Stability | no crash and no device loss in eleven automated runs of this version; long sessions are not measured yet |
| Memory | about 37 GB of private memory and 11-19 GB of video memory: 64 GB of RAM recommended |
| Shaders | 25,953 of the 25,953 programs of the title's shader archive translate and validate for 32-lane GPUs |

For comparison, 0.12.2 took 86.9 ms a frame in that scene on a quiet machine.

**What bounds the frame now is the processor's side.** The card's work was
halved by where title buffers live: with every title buffer's copy and the
windows of title memory on the card's side, the card's work by command fell
from 53.8 to 26.6 ms a frame (dispatches 29.9 to 7.5 ms). The card now waits
for us 23 ms a frame. The title's own submitting thread spends 56 ms of the 68
inside its submissions, and the thread that prepares them works 64 ms.

The one settle a frame has its cause and its answer. A 240-byte copy at its
place in a list is read as constants by a draw of the same list;
`VICTORY_FILL_FORWARD` (on) answers it. Its wait is now 1.2 to 1.5 ms a frame,
where it was 38.7 ms in 0.12.2. Loops are not the card's time either: of 746
programs drawn with counted loops, one pixel program reaches its limit.

The [roadmap](#roadmap) says what we do next.

Known to be wrong:

- Textures of what just came into view appear late: skin and clothes can stay
  grey for seconds.
- Mouths in cutscenes: the fix (16-bit typed buffers moved as integers) is in
  0.12.1 and passes its tests on the card; it is not yet confirmed in a
  cutscene.
- Rooms with little direct light are too dark without `--rt`.
- The first run after a build or a driver update stutters while the driver
  makes its pipelines.

## Quick start

You need Windows 10 or 11 (x64), a processor with AVX2, a Direct3D 12 card with
shader model 6.6 and mesh shaders, [Rust](https://rustup.rs) (the toolchain is
pinned in `rust-toolchain.toml`), the MSVC build tools with a Windows SDK, and
your own copy of the title, in a folder outside this repository.

```powershell
git clone https://github.com/PiloteCode/PiloteBox
cd PiloteBox
cargo build --release -p victory                             # makes target\release\pilotebox.exe
target\release\pilotebox.exe setup --game D:\titles\mine     # once: remembers the folder, checks the PC, translates the shaders
target\release\pilotebox.exe play
```

Or double-click `PiloteBox.cmd`: it builds when the sources changed, then
plays. `pilotebox` alone says where things stand: the title found or not, the
mode, the cache, the last run and the last benchmark.

Without `--game`, the title is looked for in `PILOTEBOX_GAME_DIR`, then in
`pilotebox.toml` (which `setup` writes), then beside the program and in the
folder above the repository.

## Commands

| Command | What it does |
|---|---|
| `pilotebox setup` | The first run in one command: finds the title or takes `--game`, writes `pilotebox.toml`, runs `doctor`, translates the shaders ahead. |
| `pilotebox play` | Plays the title. `--mode x` is the Series X profile (the default), `--mode s` the Series S profile, `--rt` turns the ray-tracing structure builder on, `--safe` leaves the newest speed-ups out (the first thing to try when something is wrong). |
| `pilotebox bench` | A timed run that ends by itself (`--seconds 300` by default) on a **copy** of your saves, then its figures: time to the world, mean and worst frame, preparing, card, copy-back, draws, dropped draws. Written as Markdown, JSON and a log beside the cache folder; its last line is a one-line summary made for pasting. |
| `pilotebox doctor` | Checks this PC: card and driver, wave size, view heap, PCIe link, AVX2, memory, disk, the cache's parts, the title's files, other runs. |
| `pilotebox check` | `doctor`, then a short `bench`, ending with PASS or FAIL. |
| `pilotebox clean` | Lists what in the cache folder is old and removes it with `--yes`. It never touches saves. |
| `pilotebox report` | Writes one text file for a bug report: `doctor`, the last run, the last benchmark, the ends of the logs, with user names and the title's folder taken out. |
| `pilotebox migrate` | Moves an older installation's folders (`%LOCALAPPDATA%\victory`) to `%LOCALAPPDATA%\PiloteBox`. Saves are copied and checked, never removed; it only lists until `--yes`. |

The developer commands of the layer are all still there: `run`, `shaders`,
`warm`, `translate`, `translate-diff`, `shader-info`, `replay`, `inspect`,
`census`, `config`. `pilotebox <command> --help` describes each, and `doctor`,
`bench` and `check` take `--json`.

```powershell
pilotebox bench --name first                    # five minutes, Series X profile
pilotebox bench --mode s --seconds 180          # three minutes, Series S profile
pilotebox check                                 # is this build and this PC all right?
```

## Modes

| Mode | What you get | What it costs |
|---|---|---|
| `--mode x` (default) | The Series X profile: the title's full quality. | 68.0 ms a frame (14.7 fps) on an RTX 5090 in the prologue's first playable scene. |
| `--mode s` | The Series S profile: a 960x540 scene upscaled by the title's own upscaler. Softer, and much lighter on the card. | Being measured; the fast mode while the Series X profile is brought to 30 fps. |
| `--rt` | The title's ray-traced indirect light: the structure builder and the walks. | With the walks really running, a tester measured 333-425 ms a frame on an RTX 4070 SUPER. For looking, not for playing. |
| `--safe` | The layer's older, slower ways. | Slower; use it to tell a bug of the speed-ups from a bug of the layer. |

Every switch of the layer can be written `PILOTEBOX_<NAME>` (the upstream
spelling `VICTORY_<NAME>` still works); the play settings are the defaults, so
none is needed. The switches are described in
[docs/findings.md](docs/findings.md), [docs/README-fxceless.md](docs/README-fxceless.md)
and each module's notes.

### Switches that bear on the card's time and on the settle

The play settings are the defaults; these are the ones that matter most for
the goal, with what they are by default:

| Switch | Default | What it does |
|---|---|---|
| `VICTORY_CARD_SIDE` | on with NVIDIA cards, else off | Every title buffer's copy and the windows of title memory on the card's side (the windows as two-sided copies), up to totals by the card's budget (the 8 and 12 GB sizes are guesses still to be measured), their room kept by the texture queue; each buffer's own switch still wins. `=0`: the placement of before; `=1` on other cards (not measured there). The card's work by command: 53.8 ms a frame without it, 26.6 ms with it; the frame 89.2 ms without it, 68.0 ms with it. |
| `VICTORY_FILL_FORWARD` | on | A fill of upload memory that reads only what a copy at its place in the same list just wrote reads the copy's source instead, on the card (the cause of the one settle a frame). `=0`: it waits for the flights, as before. |
| `VICTORY_CARD_WRITTEN_KB`, `VICTORY_CARD_ATOMIC=1`, `VICTORY_CARD_UNORDERED_KB` | off | Narrower placements: buffers that shaders store to or do atomics on (from the translator's lists of written and atomic views), buffers the title made for unordered access. |
| `VICTORY_PORT_RT=1` | off | When a fill of upload memory reads bytes that a copy at its place declared written, the card brings them, or the fill takes them from the copy's source, instead of the submission waiting for every flight (the one settle a frame). Its frame line counts the cases even when off. |
| `VICTORY_PORT_CONSTANTS=1` | off | The title's 174 MB constants heap is read by the card where the title writes it: no uploads and no fills of it. With it, indirect executions whose arguments sit there are prepared alone right before they run (`VICTORY_PORT_CONSTANTS_ORDER`, on; 2-5 ms). |
| `VICTORY_FILL_TITLE=2` | off | Small fills of upload memory go on without waiting for flights where no shader wrote the page lately. |
| `VICTORY_RT_STAMPS=1` | off | Diagnostics builds: the card's time of every compute dispatch, by kind and by pipeline. |
| `VICTORY_PORT_DESCRIPTORS=1`, `VICTORY_PORT_GUARD=1`, `VICTORY_PORT_JOURNAL=1` | off | The layer's renderer ports 2, 6 and 7: less work on the processor (descriptor stores reported by the title's own code, a shared guard lock, the heaps' records split). |
| `VICTORY_TEXTURE_MAX` | 0 | Streamed textures at their full size (the layer underneath caps them at 1,024 texels a side for 16 GB cards). |
| `VICTORY_RESIDENCY`, `VICTORY_FALLBACK_HEAPS`, `VICTORY_SWEEP_PER_HEAP`, `VICTORY_XMA_REFILL` | 1 | codec-d/cool_fox's memory budgets, heaps of one slot, the heap buffer pass per heap and XMA refill, as on an RTX 5090 before. |
| `VICTORY_DSTORAGE_STAGING_MB` | 256 | A DirectStorage staging floor (cool_fox). |
| `VICTORY_CARD_BUFFERS_MIN_KB`, `VICTORY_CARD_BUFFERS_MB`, `VICTORY_CARD_WINDOWS_MB`, `VICTORY_CARD_RESERVE_BUDGETS=1` | unset | Title buffers from that size up, and the windows shaders read, kept on the card within those budgets; with the last one, textures leave the room (TaynanGT's tree's RTX 4070 preset: 64, 1536, 1024, 1). |
| `VICTORY_STRUCTURED_RAW=1` | 0 | Structured views off 16 bytes get a copy of their own bytes. Off: on cards whose heaps have no raw twins (NVIDIA) the rule made 400 copies a frame under one lock and doubled the frame's time. |

Renderer port 5 (`VICTORY_PORT_CBV`) needs the layer underneath's own
translator and does nothing with ours. `VICTORY_FSR_4K=quality` (Series X
profile only, with AMD's signed `amd_fidelityfx_upscaler_dx12.dll` in
`VICTORY_FIDELITYFX_DIR`) renders at 2560x1440 and upscales to 3840x2160 with
FSR 4.1.2; it is not measured on NVIDIA yet.

## What is different here

- **A shader translator for 32-lane GPUs.** Every one of the 25,953 programs
  of the title's shader archive translates and validates for a host whose
  widest wave is 32 lanes: paired, split, halves and local forms of the
  console's 64-lane programs, geometry programs as geometry shaders, mesh
  programs linked with their pixel programs, dispatches by thread counts.
- **The upscaler's own shader containers** (an `ACCC` part holding an `FSPX`
  blob) translate too, so the Series S profile shows a picture (0.12.2).
- **16-bit typed buffers moved as integers** where a program moves halves
  through unsigned views: on NVIDIA the float path read zeros (0.12.1).
- **Dynamic descriptor slots that do not run out** on a 1,000,000-view heap:
  a submission gives its tables' slots back when the card has run it (from
  codec-d/cool_fox, see [CREDITS.md](CREDITS.md)).
- **The play settings are the defaults**: no script and no environment
  variable to remember.
- **Builds for x86-64-v3** with thin LTO: the title needs AVX2 anyway.
- **The PiloteBox commands**: `play`, `bench`, `doctor`, `check`, `clean`, and
  a benchmark anyone can repeat and paste.
- **A frame recorder and replay** (from the layer underneath) that we use to
  time shader forms against each other without the title.

## How it works, in one paragraph

The loader maps the title's PE image at an address of its own and patches the
few instructions a PC processor lacks. The runtime answers the console's GDK
calls (`XGameRuntime`, `XMemAllocate`, `XTaskQueue`, `GameInput`, DirectStorage,
XMA audio) with Win32 services. The graphics layer takes the console's
`d3d12_xs` command lists, written down on the title's threads and translated by
threads of ours, keeps host copies of the title's buffers and textures coherent
with what shaders write (guard pages, a write watch, copies in the card's
memory, a relay that brings shader writes back), and sends host lists to
Direct3D 12 without waiting for the card. The shader recompiler lowers GFX10
(RDNA2) machine code to DXIL: wave operations, LDS memory, typed and raw
buffers, images, mesh and geometry stages, ray-tracing structure walks. See
[docs/architecture.md](docs/architecture.md).

## Benchmarks: measuring and reporting

`pilotebox bench` is the measure we use and ask for. It reads the layer's own
frame lines, leaves out the seconds in which pipelines are still being made,
and reports means over the world (1,000 draws a frame or more).

When you report a number, give the card, the profile, the scene and the
build, and say what was not tested. One run on a quiet machine: a build running
beside the title doubles the preparing time. A report template is in the issue
tracker.

## Roadmap

The goal is the Series X profile at a steady 30 fps on NVIDIA cards: the
card's work and the processor's side each under 33 ms a frame.

Done in 0.13.0:
- **The card's work, halved by placement.** It went from 53.8 to 26.6 ms a
  frame with every title buffer and the windows of title memory on the
  card's side (`VICTORY_CARD_SIDE`, now the default on NVIDIA cards); the
  frame from 89.2 to 68.0 ms (14.7 fps).
- **The one settle a frame: its cause found.** A 240-byte copy at its place
  in a list is read as constants by a draw of the same list.
  `VICTORY_FILL_FORWARD` (on) answers it.
- **Loops answered.** Of the 746 programs drawn with counted loops, one pixel
  program reaches its limit.

Next:
1. **The processor's side**, now the bound: the card waits for us 23 ms a
   frame, the title's own submitting thread spends 56 ms of the 68 inside its
   submissions, and the thread that prepares them works 64 ms.
2. **The placement on other cards**: the 8 and 12 GB sizes measured, and the
   placement tried on AMD and Intel cards.
3. **The console's 64-lane programs on 32 lanes**: their forms measured
   against each other, and a wave64 `ds_bpermute` within each half.
4. **The Series S profile** as the fast mode meanwhile, with PC upscaling and
   frame generation from TaynanGT's tree (the libraries are the user's).
5. **The ray-traced light** once the rest fits in a frame.

[ROADMAP.md](ROADMAP.md) has each track with its figures.

## Troubleshooting

- **Stutter in the first minutes:** the driver is making pipelines. A second
  run is smooth; `pilotebox shaders` and `pilotebox warm` make them ahead.
- **Everything is several times slower than it was:** look at the PCIe line of
  `pilotebox doctor`. After a driver reset a card can fall back to
  generation 1 until the PC is restarted.
- **A build fails with "no space left":** build folders are large (10 GB a
  tree). `pilotebox clean` only looks at the cache; `cargo clean` frees the
  build folder.
- **Something is wrong in the picture:** try `pilotebox play --safe`, then
  open an issue with the output of `pilotebox doctor` and the log lines.
- **Your saves:** `bench` and `check` run on a copy. `play` uses the data
  folder (`%LOCALAPPDATA%\PiloteBox\data`, or an older installation's
  `%LOCALAPPDATA%\victory\data` until `pilotebox migrate`); copy it before
  trying a new version.

## Contributing

Pull requests are welcome and are checked on every push: the asset policy, the
build, the lints and the tests on the software adapter. Read
[CONTRIBUTING.md](CONTRIBUTING.md) first; it is short. We aim to answer within
a day. What changed in each version is in [CHANGELOG.md](CHANGELOG.md); where
this is going, track by track, in [ROADMAP.md](ROADMAP.md).

## Credits

PiloteBox stands on other people's work, named exactly in
[CREDITS.md](CREDITS.md): VIctory by Marvisak and its contributors,
fxceless/victory-fork, avatarmanz/VIctory-7900XTX (the layer this tree is built
on), codec-d/cool_fox, and the people whose pull requests those trees carry.

Internal names (crates, modules, the switches as the layer reads them) stay
upstream's on purpose: those trees are merged here regularly, and renaming
their insides would turn every merge into conflicts.

## License

GPL-3.0-or-later, as VIctory. See [LICENSE](LICENSE); the one file that is not
covered is named in [docs/asset-policy.md](docs/asset-policy.md).
