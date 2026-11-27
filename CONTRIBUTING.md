# Contributing to PiloteBox

Thank you for helping. This page is short on purpose.

## Three rules that have no exceptions

1. **Nothing from a game goes into this repository.** No game files, code,
   keys, assets, dumps, screenshots or recordings, in commits, issues, pull
   requests or comments. `cargo run -p xtask -- policy` checks the files; the
   rest is on you. See [docs/asset-policy.md](docs/asset-policy.md).
2. **No game is named here.** Write "the title". Do not name a game, its
   developer, its publisher or anything of theirs in code, comments, commit
   messages, issues or pull requests. Text that does is edited or removed.
3. **We do not help anyone obtain a game.** Questions about where to get
   files, keys or dumps are closed without an answer.

## A good pull request

One change, with a text that says:

- **What was wrong**, and how you know.
- **What changes**, in a few lines.
- **Measured**: the numbers, with the card, the profile (Series X or S), the
  scene and the build. `pilotebox bench` writes a report you can paste.
- **Tested**: exactly what you ran, and the result.
- **Not tested**: said plainly. "Not run on an AMD card" is a good sentence.

A number without its card and scene is not a number. One run on a quiet
machine beats five beside a build.

A change in behaviour comes behind a switch (`VICTORY_*`), with the old way
reachable, until it is measured on more than one machine. Say the switch and
its default in the text.

## The checks

Every pull request runs these on GitHub; run them yourself first.

```powershell
cargo run --locked -p xtask -- policy
cargo build --release --locked -p victory
cargo clippy --locked -p victory-core -p victory-pe -p victory-game -p victory-pipeline -p victory-shader -p xtask --all-targets -- -D warnings
cargo test --locked -p victory-core -p victory-pe -p victory-formats -p victory-loader -p victory-game -p victory-pipeline -p xtask -p victory
$env:VICTORY_WARP = "1";       cargo test --locked -p victory-shader -- --test-threads=1
$env:VICTORY_D3D12_WARP = "1"; cargo test --locked -p victory-gpu --features diagnostics -- --test-threads=1
```

The last two run on the software adapter (WARP); a few tests are known to
fail there and are skipped by name in `.github/workflows/ci.yml`, each with
its reason. If your change is in the shader translator, also run the census
(`pilotebox translate <archives> --validate --list`) and say whether the DXIL
of programs you did not mean to change is byte-identical.

A source file is limited to 524,288 bytes (the policy checks it): new code
goes in new files rather than into the largest ones.

## Bugs and numbers

- **A bug:** the output of `pilotebox doctor`, what you did, what you saw,
  and the log lines around it. No screenshots of a game: describe the
  picture in words.
- **A performance report:** the files `pilotebox bench` wrote, or its last
  line.

## What to expect from us

We aim to answer every issue and pull request within a day. Small fixes with
their checks green are merged quickly. Larger changes may be asked to go
behind a switch first.

Upstream trees are merged here regularly (see [CREDITS.md](CREDITS.md)), which
is why crates, modules and switches keep their upstream names. If your change
belongs upstream too, say so: we send it there with your name.

## Licence

By contributing you agree that your work is licensed GPL-3.0-or-later, as the
rest of the repository.
