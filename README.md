# VIctory on an RX 7900 XTX

## Run the codec_d Windows build

Download and extract the Windows x64 ZIP from the [private releases](https://github.com/codec-d/cool_fox/releases/latest). Open PowerShell in the extracted folder and point the launcher at your own complete Xbox dump.

```powershell
$env:VICTORY_GAME_DIR = 'D:\Games\MyDump'
.\Play-VIctory.cmd pareto-residency
```

The launcher selects Series S and the RTX-tested residency settings. It builds its shader cache locally under `%LOCALAPPDATA%\VIctory\cache-pareto`. Set `VICTORY_CACHE_DIR` to reuse an existing cache outside the checkout. Windows x64, an AVX2 CPU, and a Direct3D 12 GPU with shader model 6.6 and mesh shader support are required.

To build from this checkout, install the pinned Rust toolchain and the MSVC C++ build tools with a Windows SDK, then run `cargo build --release --locked -p victory`. The same launcher finds `target\release\victory.exe`. The sections below describe the upstream AMD fork and its separate launcher.

A fork of [VIctory](https://github.com/Marvisak/VIctory), a compatibility layer
that runs an Xbox Series title natively on Windows from files the user supplies.

## The goal

**The Series X version at 4K output (drawn at 1440p) with ray tracing on,
looking the way it does on the Xbox**, on an AMD RX 7900 XTX first.

In this order:

1. **A correct picture.** Textures and models reach full detail and stay there
   (no pop-in or pop-out, no flickering trees), and ray-traced lighting is right:
   not too dark, not too bright, no flicker.
2. **Then the frame rate:** 30 fps or more with ray tracing on, then as high a
   frame rate and as low frame times as the PC allows.
3. **Then smaller PCs:** 32 GB (later 16 GB) of RAM and smaller cards.

The way there is **porting, not emulating**: patching the title's renderer so it
behaves the way a PC renderer does, rather than having the layer watch and copy
the console's shared memory every frame. Shaders are translated ahead of time
into a versioned store that is kept between builds. The Series S profile and
ray tracing off stay available.

**This repository contains no game files, code or assets.** You need your own
copy of the game. It is not affiliated with or endorsed by the game's developer
or publisher.

### This branch: their layer with our translator, and the defaults it runs with

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
| `VICTORY_RESIDENCY` | 1 | codec-d/cool_fox's memory budgets (as its archive ran them on an RTX 5090) |
| `VICTORY_FALLBACK_HEAPS` | 1 | heaps of one slot where a list sets none (cool_fox) |
| `VICTORY_SWEEP_PER_HEAP` | 1 | the heap buffer pass per view heap (cool_fox) |
| `VICTORY_XMA_REFILL` | 1 | XMA voices go on after their end (cool_fox) |
| `VICTORY_DSTORAGE_STAGING_MB` | 256 | a DirectStorage staging floor (cool_fox) |

The renderer ports of the RX 7900 XTX fork are switches, off by default as
there: `VICTORY_PORT_CONSTANTS=1`, `VICTORY_PORT_DESCRIPTORS=1`,
`VICTORY_PORT_GUARD=1`, `VICTORY_PORT_JOURNAL=1` (its runs had 1, 2, 5, 6 and 7
on). Port 5, `VICTORY_PORT_CBV`, needs that fork's translator (constant buffers
read as raw bytes) and does nothing with ours.

`VICTORY_PLAY_DEFAULTS=0` leaves every one at the layer's own default;
`VICTORY_PLAY_DEFAULTS=safe` is the script's `-Safe` (no `THIN`, `JOIN_PARTS`,
`UPLOAD_AHEAD`; `VICTORY_UNDER_WIDE=0`, `VICTORY_LAND_ECHO=0`). Not among them,
and off unless set:

- `VICTORY_AMBIENT=0.8`, the script's stand-in for the traced indirect light;
- `VICTORY_BVH_BUILDER=1`, the ray-tracing structure builder (the script's
  `-RayTracing`): its cost kept it off in our measurements; while it does not
  run, the programs that walk its structures see an empty scene.

On by the layer's own default, as in our tree: texture read-back into title
memory (`VICTORY_TEXTURE_BACK=0` turns it off), linked mesh programs
(`VICTORY_MESH_LINK=0`). Off unless asked for: textures made due again after
DirectStorage writes (`VICTORY_PAGE_WRITES=1`) and such a write naming the
textures behind views the title wrote before (`VICTORY_OWN_BYTES=newer`); the
levels a texture holds always end at the next texture placed inside it.
Builds are made for x86-64-v3 (AVX2, which the title needs anyway;
`.cargo/config.toml`), release and diag with thin LTO and one codegen unit
(`Cargo.toml`).

## Where it stands (2026-11-26)

Tested on one machine only: Windows 11, Intel i9-12900K, 64 GB RAM, Radeon
RX 7900 XTX, renderer patches 1, 2, 5, 6 and 7 on. Runs of 270 s in a city
street: standing still, then walking and turning the camera.

| Version | Standing | Walking | RAM |
|---|---|---|---|
| **Series X, 4K drawn at 1440p, ray tracing on (the goal)** | 99 ms (10 fps) | ~110 ms (9 fps) | 45 GB |
| Series X, 4K drawn at 1440p, ray tracing off | 49 ms (20 fps) | ~62 ms (16 fps) | 45 GB |
| Series X, 1080p output, ray tracing off | 40 ms (25 fps) | ~43 ms (23 fps) | 42 GB |
| Series S, ray tracing off | 43 ms (23 fps) | ~64 ms (16 fps) | 37 GB |

Known problems with ray tracing on, being worked on now: lighting flickers and
swings between too dark and too bright even when nothing moves; dark rooms and
backgrounds; models and textures stay at their lowest detail for a long time
while the title streams (worst in the intro); trees flicker.

Earlier measurements with ray tracing off:

| | |
|---|---|
| City street, Series S profile, renderer patches 1, 2, 5, 6 and 7 on | about 45 ms a frame (about 22 fps, 44.9 ms in run merge-5) |
| Same, patches 1, 2, 5 and 6 | 45.6 ms, p95 47 ms |
| Same, patches 1, 2 and 5 | 47.3 ms (about 21 fps), p95 49 ms |
| Series X profile, patches 1+2 | about 53 ms (about 19 fps) |
| Same scene before the patches | 57-75 ms (13-17 fps) |
| Shaders the title uses that translate | all of them (25,790 of 25,985; the rest are unused geometry shaders) |
| Memory | about 30 GB of private memory, 10.5 GB of VRAM: **64 GB of RAM recommended** for now |

It started at 280 ms a frame in the same scenes three days earlier. What sets
the frame now is the one thread that submits the title's work to the card
(about 40 ms); the card itself needs about 36 ms and now waits about 7 ms a frame with nothing queued.

### Renderer patches (off by default while they are tested)

| Switch | What it does |
|---|---|
| `VICTORY_PORT_CONSTANTS=1` | The title's 174 MB constants heap is read by the card where the title writes it: no uploads, no fills |
| `VICTORY_PORT_DESCRIPTORS=1` | The title's descriptor-table stores are patched (byte-checked) to report the slots they write, instead of a scan of ~600,000 slots a frame |
| `VICTORY_PORT_CBV=1` | Translated shaders read constant buffers in place as raw bytes (~22,000 fills a frame to none; -4.7 ms) |
| `VICTORY_PORT_GUARD=1` | The write guards' lock is shared by faults and refreshes, with a claim per 64 KiB block; the submission's planning runs beside it (title threads' waits 29 ms to ~2 ms a frame; -2 ms) |
| `VICTORY_PORT_JOURNAL=1` | The heaps' records are asked for at the title's submit, and the journal's notes are split into 64 parts with locks of their own (the submit thread's waits ~5 ms + 1 ms to ~0.3 ms; -1 to -3 ms) |

### Other switches

| Switch | What it does |
|---|---|
| `--console series-s` | The Series S profile: 1080p output, about 5 ms faster, a softer picture |
| `VICTORY_OUTPUT=1920x1080` | The title renders to that output size itself (Series X profile; about 16 ms less card time) |
| `VICTORY_FRAME_INTERVAL_MS=16.667` | Lets the title go above 30 fps (menus and videos reach 60; the game's clock stays right) |
| `VICTORY_AMBIENT_SETTLE` | How fast light settles after walking indoors (on by default, `250,3`; `0` turns it off) |
| `VICTORY_TEXTURE_STORE=1` | Converted textures kept between runs (about 8 ms a frame for now: off) |

What is known to be wrong:

- Trees, signs and far objects flicker in the distance in some places.
- Lighting is too bright or too dark after walking indoors, until the title has
  run for a while; rooms with little direct light are too dark without `-RayTracing`.
- Textures of what just came into view appear late while the title streams.
- The first run of a new build stutters while the driver makes pipelines
  (`victory warm` makes them ahead).
- Recording a frame in the world makes the title's own watchdog end it.

## How to run it

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

On a hybrid-graphics PC the RX 7900 XTX fork prefers a high-performance
adapter, with a switch to name one; that choice lives in its translator
(`victory_shader::emit::host`), which this branch does not have: here the
runtime opens the default adapter, as VIctory does.

Before the first run, ahead of time (both can be stopped and go on later):

```powershell
target\diag\victory.exe shaders <the shader archives>   # every shader translated, minutes
target\diag\victory.exe warm --used                     # pipelines earlier runs drew with, for a new driver or build
```

Commands, debugging switches and upstream's own description:
[docs/README-fxceless.md](docs/README-fxceless.md). Every `VICTORY_...` switch is
described in a comment where it is read.

## What is different here

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
- This fork's changes were made with [Claude Code](https://claude.com/claude-code).

## License

GPL-3.0-or-later, as VIctory. See [LICENSE](LICENSE); the one file that is not
covered is named in [docs/asset-policy.md](docs/asset-policy.md).
