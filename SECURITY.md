# Security

PiloteBox loads and runs a title's native code in its own process, with the
rights of the user who starts it. Treat anything you run with it as you would
any program you run yourself.

## Reporting a problem

If you find a way for a crafted file, save or archive to make PiloteBox itself
do something it should not (write outside its cache and data folders, run code
that is not the title's, read files it has no business reading), please report
it privately:

- use **"Report a vulnerability"** on this repository's Security tab
  (GitHub's private advisories).

Say what the input is, what happens, and the version (`pilotebox --version`).
Do not attach game files: describe the bytes that matter, or send a file you
made yourself.

We aim to answer within two days and to fix confirmed problems before talking
about them in public.

## Builds

This repository publishes source only. We do not publish executables; a
"PiloteBox" download from anywhere else is not ours. Build from this
repository and check the commit you build.
