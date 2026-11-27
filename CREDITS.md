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
- **codec-d/cool_fox** (an archive of its commit `f4b48ad7`; the repository
  was not public when we looked): work on an RTX 5090 on the same base.
  From it: a submission gives its tables' dynamic descriptor slots back when
  the card has run it, a settle and a second try before a table is refused,
  one oversized indirect record no longer empties the draws after it, the
  two presented planes of the Series S profile composed into one image,
  host-memory admission for texture staging, bake uploads done at once. Its
  own journal is kept as `docs/bug-journal.md`.

## Through those trees

The trees above carry work that their own credits name; we repeat them here
because that code is in this tree:

- **[DexNom/VIctory](https://github.com/DexNom/VIctory)**: ray-tracing fixes,
  cube gradients, sparse textures.
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
  that fix to fxceless/victory-fork (pull request #1407 there).

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
