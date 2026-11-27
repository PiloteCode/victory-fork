# Roadmap

**The goal:** the Series X profile, at full quality, at a steady 30 frames a
second (33.3 ms a frame) on NVIDIA cards.

**Today:** 79-95 ms a frame (10.5-12.5 fps), with the picture right. That is
an RTX 5090 on a PCIe 5 link with an i9-14900KF, a diagnostics build, in the
prologue's first playable scene at about 4,800 draws a frame. The card is
what bounds the frame: it is 75-84 % busy, while preparing a frame takes
22-26 ms. So the first track is the card's time.

Each track below says what is measured today and on what. A figure without
its card and scene is not one. To add yours, run `pilotebox bench` and paste
its last line.

## 1. The card's time: 57.5 ms a frame

**Measured today** (timing run, RTX 5090, the scene above):

| Where it goes | Time a frame | How many |
|---|---|---|
| Compute dispatches | 31.7 ms | 3,812 |
| Indirect executions | 20.8 ms | 4,471 |
| The rest | about 5 ms | |

**The levers:**
- Find the passes that cost the most, frame by frame:
  `pilotebox replay --timing detailed` on a recorded frame, which needs a
  diagnostics build (docs/replay.md).
- Measure the compute programs' translated forms against each other, turn by
  turn, with replay's `--shaders` and `--shaders-b`. This applies to wave64
  programs on 32-lane hosts (paired, split, local) and to legacy multiplies.
- Find the indirect executions that run for nothing: empty records, and
  passes whose inputs are missing while the ray-traced light is off.
- Find the barriers between our own passes that the order does not need. Our
  older layer had a pass for this; it is in the history and is not ported
  yet.

## 2. Small buffers in the card's memory, and their sync cost

The console's CPU and GPU share memory; here they do not. So everything one
writes and the other reads must be noticed and moved.

**Measured today:** in the first intervals of the world, the frame lines show:
- 25-30 ms a frame of title memory copies;
- 23-26 ms a frame of copy-back (RTX 5090, diagnostics build, the scene
  above).

**The levers:**
- Keep small buffers' copies in the card's memory
  (`VICTORY_CARD_BUFFERS_MIN_KB`, after fxceless). Not measured on NVIDIA yet.
- Compare and hash those copies on the card, joining runs.
- Bring back only the dwords that changed.

## 3. The one settle a frame

Once a frame, the layer waits until what the card wrote is back in title
memory before the title reads it.

**Measured today:** the frame lines show 7-13 ms a frame waiting for the card
in the first world intervals (RTX 5090, diagnostics build). The card's clock
line shows how much of a frame the card stood idle behind our submitting
thread.

**The levers.** Our older layer tried three, behind switches; they are in the
history (0.8 to 0.10) and not yet ported to this layer:
- wait only for the producers of the pages the title reads;
- settle a page window at its first touch;
- cut long submissions while waits happen.

None of them was measured as a default on NVIDIA.

## 4. The Series S profile

**Measured today:**
- It shows a picture since 0.12.2: the upscaler's 20 programs translate.
- The scene is drawn at 960x540 in our captures, so this is the fast, soft
  mode.
- In our first measurement it ran about twice as fast as the Series X
  profile, with the upscaler's passes still missing.

**Next:**
- Give the upscaler's passes their pipelines.
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

## 6. Mouths: done in 0.12.1

16-bit typed buffer accesses through a table's places now move their bits as
unsigned integers (`VICTORY_HALF_BITS`, on; a port of fxceless #927). The
face network no longer reads zeros on NVIDIA, and mouths move in cutscenes.

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
