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

## Where it stands (2026-11-27, version 0.12.2)

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
| Series X profile (full quality) | 79-95 ms a frame (10.5-12.5 fps) in the prologue's first playable scenes, about 4,800 draws a frame; the picture is right |
| Series S profile (lighter, upscaled) | shows a picture since 0.12.2; its frame time is being measured again (44 ms a frame before the upscaler's passes ran) |
| Ray-traced indirect light (`--rt`) | runs, far too slow to play with today |
| Stability | no crash and no device loss in eleven automated runs of this version; long sessions are not measured yet |
| Memory | about 37 GB of private memory and 11-19 GB of video memory: 64 GB of RAM recommended |
| Shaders | 25,953 of the 25,953 programs of the title's shader archive translate and validate for 32-lane GPUs |

The same scenes took 130-168 ms a frame with the layer this tree replaced two
days earlier.

**What bounds the frame today is the card.** A timing run of the Series X
profile counts 57.5 ms of card work a frame: 31.7 ms in 3,812 compute
dispatches and 20.8 ms in 4,471 indirect executions. Preparing a frame on the
processor takes 22-26 ms. The [roadmap](#roadmap) says what we do about it.

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
cargo build --release -p victory               # makes target\release\pilotebox.exe
target\release\pilotebox.exe doctor            # checks the PC, the cache and the title's files
target\release\pilotebox.exe play --game D:\titles\mine
```

Or double-click `PiloteBox.cmd`: it builds when the sources changed, then
plays. Without `--game`, the title is looked for in `VICTORY_GAME_DIR`, then in
`victory.toml` (copy `victory.example.toml`), then beside the program and in
the folder above the repository.

## Commands

| Command | What it does |
|---|---|
| `pilotebox play` | Plays the title. `--mode x` is the Series X profile (the default), `--mode s` the Series S profile, `--rt` turns the ray-tracing structure builder on, `--safe` leaves the newest speed-ups out (the first thing to try when something is wrong). |
| `pilotebox bench` | A timed run that ends by itself (`--seconds 300` by default) on a **copy** of your saves, then its figures: time to the world, mean and worst frame, preparing, card, copy-back, draws, dropped draws. Written as Markdown, JSON and a log beside the cache folder. |
| `pilotebox doctor` | Checks this PC: card and driver, wave size, view heap, PCIe link, AVX2, memory, disk, the cache's parts, the title's files, other runs. |
| `pilotebox check` | `doctor`, then a short `bench`, ending with PASS or FAIL. |
| `pilotebox clean` | Lists what in the cache folder is old and removes it with `--yes`. It never touches saves. |

The developer commands of the layer are all still there: `run`, `shaders`,
`warm`, `translate`, `translate-diff`, `shader-info`, `replay`, `inspect`,
`census`, `config`. `pilotebox <command> --help` describes each.

```powershell
pilotebox bench --name first                    # five minutes, Series X profile
pilotebox bench --mode s --seconds 180          # three minutes, Series S profile
pilotebox check                                 # is this build and this PC all right?
```

## Modes

| Mode | What you get | What it costs |
|---|---|---|
| `--mode x` (default) | The Series X profile: the title's full quality. | 79-95 ms a frame on an RTX 5090 today. |
| `--mode s` | The Series S profile: a 960x540 scene upscaled by the title's own upscaler. Softer, and much lighter on the card. | Being measured; the fast mode while the Series X profile is brought to 30 fps. |
| `--rt` | The title's ray-traced indirect light: the structure builder and the walks. | With the walks really running, a tester measured 333-425 ms a frame on an RTX 4070 SUPER. For looking, not for playing. |
| `--safe` | The layer's older, slower ways. | Slower; use it to tell a bug of the speed-ups from a bug of the layer. |

Every switch of the layer (`VICTORY_*`) still works as it is; the play settings
are the defaults, so none is needed. The switches are described in
[docs/findings.md](docs/findings.md), [docs/README-fxceless.md](docs/README-fxceless.md)
and each module's notes.

### Switches that bear on the card's time and on the settle

The play settings are the defaults; these are the ones that matter most for
the goal, with what they are by default:

| Switch | Default | What it does |
|---|---|---|
| `VICTORY_PORT_RT=1` | off | When a fill of upload memory reads bytes that a copy at its place declared written, the card brings them, or the fill takes them from the copy's source, instead of the submission waiting for every flight (the one settle a frame). Its frame line counts the cases even when off. |
| `VICTORY_PORT_CONSTANTS=1` | off | The title's 174 MB constants heap is read by the card where the title writes it: no uploads and no fills of it. With it, indirect executions whose arguments sit there are prepared alone right before they run (`VICTORY_PORT_CONSTANTS_ORDER`, on; 2-5 ms). |
| `VICTORY_FILL_TITLE=2` | off | Small fills of upload memory go on without waiting for flights where no shader wrote the page lately. |
| `VICTORY_RT_STAMPS=1` | off | Diagnostics builds: the card's time of every compute dispatch, by kind and by pipeline. |
| `VICTORY_PORT_DESCRIPTORS=1`, `VICTORY_PORT_GUARD=1`, `VICTORY_PORT_JOURNAL=1` | off | The layer's renderer ports 2, 6 and 7: less work on the processor (descriptor stores reported by the title's own code, a shared guard lock, the heaps' records split). |
| `VICTORY_TEXTURE_MAX` | 0 | Streamed textures at their full size (the layer underneath caps them at 1,024 texels a side for 16 GB cards). |
| `VICTORY_RESIDENCY`, `VICTORY_FALLBACK_HEAPS`, `VICTORY_SWEEP_PER_HEAP`, `VICTORY_XMA_REFILL` | 1 | codec-d/cool_fox's memory budgets, heaps of one slot, the heap buffer pass per heap and XMA refill, as on an RTX 5090 before. |
| `VICTORY_DSTORAGE_STAGING_MB` | 256 | A DirectStorage staging floor (cool_fox). |
| `VICTORY_CARD_BUFFERS_MIN_KB`, `VICTORY_CARD_BUFFERS_MB`, `VICTORY_CARD_WINDOWS_MB`, `VICTORY_CARD_RESERVE_BUDGETS=1` | unset | Title buffers from that size up, and the windows shaders read, kept on the card within those budgets; with the last one, textures leave the room (TaynanGT's tree's RTX 4070 preset: 64, 1536, 1024, 1). |
| `VICTORY_STRUCTURED_RAW=0` | 1 | Structured views off 16 bytes without a copy of their own (one lock fewer for the list threads; our translator reads such views as raw bytes, so look at the picture). |

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

The goal is the Series X profile at a steady 30 fps on NVIDIA cards: the card's
work has to come down from 57.5 ms a frame to under 30, and the processor's
side has to stay under 33.

1. **Small buffers where shaders write them.** About 11 ms of the card's
   time go to nine small programs whose atomics and stores land on title
   buffers kept in the PC's memory, across the bus. A narrow rule (buffers
   used with atomics, and small written buffers, live in the card's memory)
   is being built; moving every small buffer costs the processor as much as
   it saves the card.
2. **The console's 64-lane programs on 32 lanes.** Their forms cost about
   15 ms a frame together; the entry barrier and the per-lane guards are the
   levers.
3. **One settle a frame.** The preparing thread waits about 36 ms a frame for
   the card at one point, for a fill of upload memory that reads title memory.
   Renderer port RT (`VICTORY_PORT_RT=1`) is the first thing to measure there.
4. **The newest layer underneath** is merged: its renderer ports are
   switches, off until measured on NVIDIA (see the switches above).
5. **The Series S profile** as the fast mode meanwhile.
6. **The ray-traced light** once the rest fits in a frame.

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
  folder (`%LOCALAPPDATA%\victory\data` by default); copy it before trying a
  new version.

## Contributing

Pull requests are welcome and are checked on every push: the asset policy, the
build, the lints and the tests on the software adapter. Read
[CONTRIBUTING.md](CONTRIBUTING.md) first; it is short. We aim to answer within
a day.

## Credits

PiloteBox stands on other people's work, named exactly in
[CREDITS.md](CREDITS.md): VIctory by Marvisak and its contributors,
fxceless/victory-fork, avatarmanz/VIctory-7900XTX (the layer this tree is built
on), codec-d/cool_fox, and the people whose pull requests those trees carry.

Internal names (crates, modules, the `VICTORY_*` switches, the data folder)
stay upstream's on purpose: those trees are merged here regularly, and renaming
their insides would turn every merge into conflicts.

## License

GPL-3.0-or-later, as VIctory. See [LICENSE](LICENSE); the one file that is not
covered is named in [docs/asset-policy.md](docs/asset-policy.md).
