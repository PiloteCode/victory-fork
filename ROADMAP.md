# Roadmap

**The goal:** the Series X profile, at full quality, at a steady 30 frames a
second (33.3 ms a frame) on NVIDIA cards.

**Today:**
- 0.12.2 took 79-95 ms a frame (10.5-12.5 fps), with the picture right.
- In 0.13.0 the card's work is halved by where title buffers live, the one
  settle a frame has its cause and its answer, and loops are ruled out.
- What bounds the frame now is the processor's side. In the first run with
  the placement on, the thread that prepares submissions worked 64 ms of a
  67 ms frame.
- All of this is on an RTX 5090 on a PCIe 5 link with an i9-14900KF, a
  diagnostics build, in the prologue's first playable scene at about 4,800
  draws a frame.
- 0.13.0's frame figures follow its runs.

Each track below says what is measured and on what. A figure without its card
and scene is not one. To add yours, run `pilotebox bench` and paste its last
line.

## 1. The processor's side: the thread that prepares submissions

**Measured:** in the first run with the placement on, the thread that
prepares submissions worked 64 ms of a 67 ms frame. That run was on an RTX
5090, Series X profile, `VICTORY_CARD_SIDE=1`, a diagnostics build.

**The levers.** None of these is measured on our card yet:
- **The other trees' processor-side work.** It is in this tree since 0.13.0,
  as defaults or as switches.
  - TaynanGT's tree: Vladimir's bitmask of uncopied pages, lock-free mirrors
    and AVX2 `Shadow::take`, and the AVX2 detiler, both on.
  - avatarmanz's renderer ports 2, 6 and 7 (`VICTORY_PORT_DESCRIPTORS`,
    `VICTORY_PORT_GUARD`, `VICTORY_PORT_JOURNAL`), off.
- **The thread counts.** TaynanGT's tree runs an RTX 4090 with
  `VICTORY_THIN_THREADS=12`, `VICTORY_UNDER_THREADS=8`,
  `VICTORY_LAND_THREADS=6` and `VICTORY_MAP_THREADS=auto`. Our defaults are
  tuned on an AMD card (`VICTORY_MAP_THREADS=1`, `VICTORY_CARD_AWAKE=1`).
- **Where the 64 ms go.** The timeline (`VICTORY_TIMELINE`) and a profile of
  that thread, step by step.

## 2. The card's time: halved by placement

**Measured** (the card's work by command, RTX 5090, the scene above):
- 53.8 ms a frame with the default placement;
- 26.6 ms with every title buffer and the windows of title memory on the
  card's side (`VICTORY_CARD_SIDE=1`).

| Where it went | Default placement | On the card's side |
|---|---|---|
| Dispatches | 29.9 ms | 7.5 ms |
| The two shadow-mask programs | 5.6 and 2.1 ms | 0.2 and 0.3 ms |

Loops are answered: of 746 programs drawn with counted loops, one pixel
program reaches its iteration limit, and no compute program does.

**Next:**
- Make the placement the default once the runs say yes.
- Measure the translator's forms of wave64 programs against each other with
  replay's `--shaders` and `--shaders-b`. A wave64 `ds_bpermute` permutes
  within each 32-lane half on the console, which would let more programs run
  as halves (TaynanGT's tree found it).

## 3. The one settle a frame: its cause found

The cause: a 240-byte buffer copy at its place in a list, which a draw of the
same list reads as constants. The fill of upload memory for those constants
waited for every flight. `VICTORY_FILL_FORWARD` (on) makes such a fill read
the copy's source instead, on the card with the other steps. The frame line
says why a fill was left to wait.

**Next:** confirm on the runs that the settle is gone in the world, not only
in the prologue's first scene.

## 4. The Series S profile

**Measured today:**
- It shows a picture since 0.12.2: the upscaler's 20 programs translate.
- The scene is drawn at 960x540 in our captures, so this is the fast, soft
  mode.
- Before the upscaler's passes ran it took 44 ms a frame; with them it is
  not measured on a quiet machine yet.

**Next:**
- Measure the profile on a quiet machine.
- Compare its picture with the Series X profile's.

## 5. The ray-traced light

**Measured today:**
- It is off by default; `--rt` turns the structure builder on.
- While it does not run, node tests answer no hit, so rooms with little
  direct light are darker.
- With it on, a tester on an RTX 4070 SUPER measured 333-425 ms a frame once
  the walk really runs (fxceless #994).
- So `--rt` is for looking, not for playing, today.

**Next:**
- Find where the walk's time goes.
- Look at the structure builder's own passes.
- Try a cheaper stand-in for the indirect light (`VICTORY_AMBIENT`).

## 6. Mouths: fixed in 0.12.1, to confirm in a cutscene

16-bit typed buffer accesses through a table's places now move their bits as
unsigned integers (`VICTORY_HALF_BITS`, on; a port of fxceless #927). On the
card every one of the 65,536 patterns now comes back, where the old form
read zeros. Still to confirm in a cutscene.

## Also on the way

- **The first run after a build or a driver update stutters** while the driver
  makes pipelines.
  - `pilotebox setup` translates the shaders ahead.
  - `pilotebox warm` makes the pipelines earlier runs used.
  - Next: make the pipelines a scene needs before it is shown.
- **Textures of what just came into view appear late.** Skin and clothes can
  stay grey for seconds.
- **Memory.** A run takes 40 GB or more of private memory; on the RX 7900 XTX
  it took 40-46 GB.
