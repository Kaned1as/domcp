# Arch Linux packaging

This directory contains upstream-maintained Arch packaging files for `domcp`.

## Layout

- `domcp-git/PKGBUILD` — VCS package definition intended for the AUR package `domcp-git`

`.SRCINFO` is intentionally **not** stored in this upstream repository. Generate it only in the actual AUR publishing repository.

## Local testing

From the package directory:

- syntax check: `bash -n PKGBUILD`
- build package: `makepkg -si`
- print AUR metadata: `makepkg --printsrcinfo`
