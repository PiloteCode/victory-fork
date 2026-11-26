# Pilote Victory

A fork of [VIctory](https://github.com/Marvisak/VIctory), a compatibility layer
that runs an Xbox Series title natively on Windows from files the user supplies.
This one is about **NVIDIA cards**. It is the layer of
[avatarmanz/VIctory-7900XTX](https://github.com/avatarmanz/VIctory-7900XTX),
which was built for speed on AMD RDNA 3, with a shader translator that covers
cards of 32-lane waves and the fixes of
[codec-d/cool_fox](https://github.com/codec-d/cool_fox) for a view heap of
1,000,000 descriptors.

**This repository contains no game files, code or assets.** You need your own
copy of the game. It is not affiliated with or endorsed by the game's developer
or publisher.

## Where this fork stands (2026-11-27)

Tested on one machine only: Windows 11, Intel i9-14900KF, 64 GB RAM, GeForce
RTX 5090 on a PCIe 5 link, a diagnostics build, runs of about five minutes.

| | |
|---|---|
| Prologue, about 4,800 draws a frame, the Series X profile | 79-95 ms a frame (10.5-12.5 fps), the picture right |
| The same scene with the layer this tree replaced | 130-168 ms |
| The same scene under `--console series-s` | about 44 ms (22 fps), but a black picture: the upscaler's programs do not translate yet |

What bounds the frame under the Series X profile is the card: it is 75-84 %
busy with the translated shaders, while preparing a frame takes 22-26 ms.

Known to be wrong here:

- Textures of what just came into view appear late: skin and clothes can stay
  grey for seconds.
- Mouths do not move in cutscenes on NVIDIA cards (16-bit typed buffers; the
  fix is being fitted to this layer).
- The Series S profile shows a black picture (see the table).
- The first run after a build or a driver update stutters while the driver
  makes pipelines.

## How to run this fork

```powershell
cargo build --release -p victory
targeteleaseictory.exe run <path of your copy's executable> --execute
```

The play settings of the layer underneath are `victory run`'s defaults here
(next section): no script and no environment variable is needed.

## Their layer with our translator, and the defaults it runs with

This tree is the RX 7900 XTX fork's layer (everything below) with the shader
translator of our tree (PiloteCode's waves, `crates/victory-shader` as of
wave 11): every one of the title's 25,953 programs translates and validates for
a card of 32-lane waves too (NVIDIA), with paired forms of wave64 programs,
geometry programs, linked mesh programs, dispatches past 65,535 groups split,
thread-limited forms for dispatches by thread counts.

`victory run` now plays with the switches `scripts\play.ps1` set, as its
defaults (`apps/victory/src/defaults.rs`); a switch set in the environment
keeps its value:

| Switch | Default here | What it does |
|---|---|---|
| `VICTORY_THIN` | 1 | list calls written down, translated by eight threads of ours |
| `VICTORY_JOIN_PARTS` | 8 | parts of submissions prepared as one |
| `VICTORY_UPLOAD_AHEAD` | 1 | first texture uploads on a copy queue of their own |
| `VICTORY_RELAY` | 2 | what shaders wrote comes back beside the guards, on a thread of its own |
| `VICTORY_RELAY_COPY` | 1 | that fetch on a copy queue |
| `VICTORY_WIDE_BACK` | 1 | (the layer's default too) |
| `VICTORY_CARD_REHASH_RUNS` | 1 | card copies' hashes by runs |
| `VICTORY_JOIN_COPIES` | 1 | copies of a submission joined |
| `VICTORY_CARD_STEPS` | 2 | fills that need bytes on the card done on the card |
| `VICTORY_INDIRECT_WINDOW` | 1 | indirect draws clamped a window at a time |
| `VICTORY_INDIRECT_BATCH` | 2 | single-record executions clamped in batches, dispatches included |
| `VICTORY_TEXTURE_NO_WAIT` | 1 | texture uploads wait for no mapping |
| `VICTORY_SHOW_AFTER` | 1 | a slot shows a texture after its upload has run |
| `VICTORY_MAP_THREADS` | 1 | pages mapped by the caller alone |
| `VICTORY_TITLE_PRIORITIES` | 0 | the title's thread priorities as it sets them |
| `VICTORY_CARD_AWAKE` | 1 | the card's clocks kept up between submissions |
| `VICTORY_D3D12_DRED` | light | crash tracing that can stay on (diagnostics builds) |

`VICTORY_PLAY_DEFAULTS=0` leaves every one at the layer's own default;
`VICTORY_PLAY_DEFAULTS=safe` is the script's `-Safe` (no `THIN`, `JOIN_PARTS`,
`UPLOAD_AHEAD`; `VICTORY_UNDER_WIDE=0`, `VICTORY_LAND_ECHO=0`). Not among them,
and off unless set:

- `VICTORY_AMBIENT=0.8`, the script's stand-in for the traced indirect light;
- `VICTORY_BVH_BUILDER=1`, the ray-tracing structure builder (the script's
  `-RayTracing`): its cost kept it off in our measurements; while it does not
  run, the programs that walk its structures see an empty scene.

On by the layer's own default, as in our tree: texture read-back into title
memory (`VICTORY_TEXTURE_BACK=0` turns it off) and linked mesh programs
(`VICTORY_MESH_LINK=0`). Two texture rules of our older tree are here behind
switches and off, because the first put texel noise on textures in this layer
(RTX 5090, the prologue's first playable scene): a texture's own bytes ending
only at a view written after its own (`VICTORY_OWN_BYTES=newer`), and textures
made due again after DirectStorage writes (`VICTORY_PAGE_WRITES=1`).
Builds are made for x86-64-v3 (AVX2, which the title needs anyway;
`.cargo/config.toml`), release and diag with thin LTO and one codegen unit
(`Cargo.toml`).

## The layer underneath: avatarmanz's README

What follows is the README of
[avatarmanz/VIctory-7900XTX](https://github.com/avatarmanz/VIctory-7900XTX) as
this tree took it (its commit 0a435665). Its figures are its own machine's.

A fork of [VIctory](https://github.com/Marvisak/VIctory), a compatibility layer
that runs an Xbox Series title natively on Windows from files the user supplies.
This one is about **speed on AMD RDNA 3**: the world at 30 frames a second on
one PC, without giving up the picture. It is not there yet.

### Where it stands (2026-11-25)

Tested on one machine only: Windows 11, Intel i9-12900K, 64 GB RAM, Radeon
RX 7900 XTX, 1440p, the Series X profile.

| | |
|---|---|
| Menus, videos, loading screens | 30 fps |
| Prologue, indoors | 50-75 ms a frame (13-20 fps) |
| First mission with the boat | 46-53 ms (19-22 fps) |
| Street chase with many cars and people | 100-165 ms (6-10 fps), single frames far longer |
| Outdoors on foot, the camp | 60-90 ms |
| Flying | 42-57 ms |
| Longest session so far | 41 minutes, no crash |
| Memory | 40-46 GB of private memory: **64 GB of RAM recommended** |

It started at 280 ms a frame in the same scenes two days earlier.

What is known to be wrong:

- Trees, plants and far buildings flicker outdoors in some places.
- Rooms with little direct light are too dark without `-RayTracing`.
- Textures of what just came into view appear late while the title streams.
- The first run with a new driver or a new build stutters while the driver makes
  pipelines (`victory warm` makes them ahead).
- Recording a frame in the world makes the title's own watchdog end it.

### How to run it

You need Windows 10/11 x64, a CPU with AVX2, a Direct3D 12 GPU with shader
model 6.6 and mesh shaders, Rust (the toolchain is pinned), the MSVC build
tools, and your own dump of the title.

```powershell
git clone https://github.com/avatarmanz/VIctory-7900XTX
cd VIctory-7900XTX
copy victory.example.toml victory.toml      # then set your paths, outside this folder
cargo build --locked --profile diag -p victory --features diagnostics
scripts\play.ps1 -Exe <path of your copy's executable>
```

`scripts\play.ps1 -Safe` leaves out the newest, faster ways; try it first when
something is wrong. `-RayTracing` turns the title's ray-traced indirect light on.
In this branch `target\diag\victory.exe run <exe> --execute` alone plays with the
script's switches too (above), all but the stand-in light (`VICTORY_AMBIENT`).

Before the first run, ahead of time (both can be stopped and go on later):

```powershell
target\diag\victory.exe shaders <the shader archives>   # every shader translated, minutes
target\diag\victory.exe warm --used                     # pipelines earlier runs drew with, for a new driver or build
```

Commands, debugging switches and upstream's own description:
[docs/README-fxceless.md](docs/README-fxceless.md). Every `VICTORY_...` switch is
described in a comment where it is read.

### What is different here

In short; the reasons and the measurements are in [docs/findings.md](docs/findings.md).

- **List calls are written down, not carried out, on the title's threads**
  (`VICTORY_THIN`): eight threads of ours translate them.
- **Submissions are prepared by a thread of ours and joined**
  (`VICTORY_ASYNC_SUBMIT`, `VICTORY_JOIN_PARTS`), with what can be spread over
  threads spread: refreshes, compares (AVX2), descriptor records, landings.
- **What shaders wrote comes back beside the write guards**, by the dwords that
  changed, on a thread of its own (`VICTORY_RELAY`, `VICTORY_LAND`).
- **Buffer fills that need bytes still on the card are done on the card**
  (`VICTORY_CARD_STEPS`) instead of waiting for it.
- **The title's writes are caught more cheaply**: one view opened at a fault,
  blocks that rest, places in the title's code that look before they write.
- **Textures go up on a copy queue of their own** and a slot shows one only after
  its upload ran, which also ended a device loss that came every 15 minutes or so.
- **Boats float**: textures that shaders write and the title's CPU reads go back
  to title memory (from fxceless and Geckomacabre, on by default here).
- **Frame record and replay** with per-pass times and shader variants compared
  turn by turn ([docs/replay.md](docs/replay.md)).

## Credits

This is a tree of other people's work with more on top. The history is kept, and
commits taken from elsewhere name the commit and its author.

- **[VIctory](https://github.com/Marvisak/VIctory)** by Marvisak and its
  contributors: the loader, the runtime services, the Direct3D 12 back end and
  the shader recompiler.
- **[avatarmanz/VIctory-7900XTX](https://github.com/avatarmanz/VIctory-7900XTX)**:
  the layer this tree is built on, and most of this README.
- **[codec-d/cool_fox](https://github.com/codec-d/cool_fox)**: the work on an
  RTX 5090 that keeps the dynamic descriptor slots from running out on a
  1,000,000-view heap, the two presented planes of the Series S profile, host
  memory admission for texture staging (its `docs/bug-journal.md` is kept).
- **[stellarr-studio/kelvaroq](https://github.com/stellarr-studio/kelvaroq)**:
  its patches were read for fixes worth taking; each one taken names its patch.
- **[fxceless/victory-fork](https://github.com/fxceless/victory-fork)**: the fork
  this one started from, and the people whose pull requests it carries.
- **[DexNom/VIctory](https://github.com/DexNom/VIctory)**: ray tracing fixes,
  cube gradients, sparse textures.
- **[Belobug/VIctory-Belobug-AMD](https://github.com/Belobug/VIctory-Belobug-AMD)**:
  the ambient light idea, pipeline workers, and work by romainhedouin.
- **[FQY55kai/ZJOU-VICTORY](https://github.com/FQY55kai/ZJOU-VICTORY)**.
- **[myokex/victory-amd](https://github.com/myokex/victory-amd)**: the guard over
  every view of a page (with Spexivus, joroyt and tightfall), and Joshua Odom's
  reads into title memory.
- **Geckomacabre** and **cxy0072019-sketch**: the texture read-back.
- **[XCL](https://github.com/ivall/xcl)** (GPL-2.0-only) was read for how the same
  title behaves under another layer; no code of it is here.
- avatarmanz's README says of its own changes that they were made with
  [Claude Code](https://claude.com/claude-code).

## License

GPL-3.0-or-later, as VIctory. See [LICENSE](LICENSE); the one file that is not
covered is named in [docs/asset-policy.md](docs/asset-policy.md).
