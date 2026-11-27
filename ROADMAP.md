# Roadmap

**The goal:** the Series X profile, at full quality, at a steady 30 frames a
second (33.3 ms a frame) on NVIDIA cards.

**Today (0.13.1):** 46.6 and 48.8 ms a frame, 21.5 and 20.5 fps, not 30. On an RTX 5090
with an i9-14900KF, the Series X profile, a diagnostics build, in the
prologue's first playable scene at about 4,850 draws a frame; two runs of four minutes each, alternated, the median frame of each run's last minute, the process at high priority, no captures, another program drawing on the card.

| Version | A frame | The thread that prepares submissions |
|---|---|---|
| 0.12.2, and 0.13.0 with the placement of before | about 85 ms, with captures | worked about 80 ms |
| 0.13.0 | 53.7 and 53.8 ms | |
| 0.13.1 | 46.6 and 48.8 ms | worked 43.6 and 45.5 ms |

Runs before 0.13.1 captured one frame in eight for pictures, which costs
about 9 ms a frame on that thread; the rows above say which had them.

The card is no longer the bound: it waits for us. The processor is: the
frame is that one thread's work plus about 4 ms.

Each track below says what is measured and on what. A figure without its card
and scene is not one. To add yours, run `pilotebox bench` and paste its last
line.

## 1. The processor's side: the thread that prepares the title's submissions

**Measured** (0.13.1):
- One thread of ours prepares the title's submissions and sends them. It
  works 43.6 and 45.5 ms a frame and sets the frame; the title's threads wait
  at the present for it.
- Its parts, a frame: the write watch 4, the title's bytes put under flights'
  writes 6, the lists' steps 16 (the refreshes of host copies 10), copies in
  the card's memory 12, planning the steps done on the card 4, sending 2.
- Of 10,000 refreshes a frame, 1,245 copy something (75 MB a frame). About a
  third of their cost is the guards over memory the system cannot watch.
- Of 80,000 ranges a frame that go up to the card's copies, nearly all are a
  few bytes of the title's put under a flight's write.

**For 30 frames a second that thread must come under about 29 ms.** No sum
of small cuts gets there, so the plan is to prepare two submissions at once:
the first half of the next one (the write watch, the bytes under flights, the
refreshes: about 20 ms) on a second thread while this one's steps run. The
first step overlaps only this submission's tail (about 14 ms).

**The other levers,** measured or not:
- **The other trees' processor-side work.** It is in this tree since 0.13.0,
  as defaults or as switches.
  - TaynanGT's tree: Vladimir's bitmask of uncopied pages, lock-free mirrors
    and AVX2 `Shadow::take`, and the AVX2 detiler, both on.
  - avatarmanz's renderer ports 2, 6 and 7 (`VICTORY_PORT_DESCRIPTORS`,
    `VICTORY_PORT_GUARD`, `VICTORY_PORT_JOURNAL`), off. Port 6 measured: 57.3
    ms a frame against 61.0 and 62.8 (the wait for the guards' lock goes from
    8 ms to 0.3); to be confirmed, then made the default. Port 7 adds nothing
    to the frame. Port 1 (`VICTORY_PORT_CONSTANTS`) costs 301 ms a frame on a
    discrete card.
- **The thread counts.** TaynanGT's tree runs an RTX 4090 with
  `VICTORY_THIN_THREADS=12`, `VICTORY_UNDER_THREADS=8`,
  `VICTORY_LAND_THREADS=6` and `VICTORY_MAP_THREADS=auto`. Our defaults are
  tuned on an AMD card (`VICTORY_MAP_THREADS=1`, `VICTORY_CARD_AWAKE=1`).
  One run with that tree's counts on our card: inside the spread of ours.
- **Where the time goes**, step by step: the timeline (`VICTORY_TIMELINE`)
  and a profile of both threads.
- **Windows through the bus** are the placement's choice again since 0.13.1
  (see the CHANGELOG's correction): the faster or the equal way in every
  pair of runs, and no second copy in the PC's memory.

## 2. The card's time: halved by placement

**Measured** (the card's work by command, RTX 5090, the scene above):
- 53.8 ms a frame with the placement of before;
- 26.6 ms with every title buffer and the windows of title memory on the
  card's side.

| Where it went | Placement of before | On the card's side |
|---|---|---|
| Dispatches | 29.9 ms | 7.5 ms |
| The two shadow-mask programs | 5.6 and 2.1 ms | 0.2 and 0.3 ms |

Loops are answered: of the 746 programs drawn with counted loops, one pixel
program reaches its iteration limit. No compute program does.

`VICTORY_CARD_SIDE` is the default on NVIDIA cards since 0.13.0. Its sizes
follow the card's budget.

**Next:**
- Measure the 8 GB and 12 GB sizes, which are guesses.
- Try the placement on AMD and Intel cards. It is off there unless
  `VICTORY_CARD_SIDE=1`.
- Measure the translator's forms of wave64 programs against each other, with
  replay's `--shaders` and `--shaders-b`. On the console, a wave64
  `ds_bpermute` permutes within each 32-lane half; using that would let more
  programs run as halves (TaynanGT's tree found it).

## 3. The one settle a frame: its cause found

The cause was a 240-byte buffer copy at its place in a list, read as
constants by a draw of the same list. The fill of upload memory for those
constants waited for every flight.

`VICTORY_FILL_FORWARD` (on) makes such a fill read the copy's source instead,
on the card with the other steps.

**Measured** (the run above, the placement on):
- 19 fills a frame forwarded;
- 0.14 submissions a frame still refused and waited;
- the settle's wait is 1.2 to 1.5 ms a frame, where it was 38.7 ms in 0.12.2.

**Next:** the submissions that are still refused, and their reasons, which
the frame line names.

## 4. The Series S profile

**Measured today:**
- It shows a picture since 0.12.2: the upscaler's 20 programs translate.
- The scene is drawn at 960x540 in our captures, so this is the fast, soft
  mode.
- In 0.13.0, one run in the same scene (the process at high priority while
  other builds ran): 60.6 ms a frame (16.5 fps), about 4,440 draws a frame,
  the card with work queued 60 % of the frame. The thread of ours that
  prepares submissions worked 54.4 ms: the same bound as the Series X
  profile, so this profile gains little until that thread is cut (track 1).
- One poster in the scene shows as a flat patch of colour where the Series X
  profile shows its print: a texture's detail is missing there.

**Next:**
- Track 1 first: it bounds both profiles.
- Compare the picture with the Series X profile's, texture by texture.
- PC upscaling in place of the title's own (switches from TaynanGT's tree,
  off; they need libraries the user supplies).

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

## 6. Mouths: fixed in 0.12.1, confirmed in 0.13.0

16-bit typed buffer accesses through a table's places now move their bits as
unsigned integers (`VICTORY_HALF_BITS`, on; a port of fxceless #927). On the
card every one of the 65,536 patterns now comes back, where the old form
read zeros. Confirmed in 0.13.0 on an RTX 5090: in a new game's opening
cutscene, three captures eight frames apart show a face with its mouth
closed, open in speech, and closed again, with the eyes moving too and clean
skin. `VICTORY_HALF_BITS=0` gives the old translation for comparison.

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
