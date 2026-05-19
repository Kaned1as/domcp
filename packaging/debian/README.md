# Debian packaging

This directory contains upstream-maintained Debian packaging files for `domcp`.

## Layout

- `debian/` — Debian source package metadata intended to be copied to the project root before building

## Notes

This packaging builds `domcp` directly with `cargo` from the upstream checkout.
That is convenient for local builds, but it still fetches Rust dependencies during the
package build.

At runtime, `domcp` expects either `podman` or `docker` to be installed separately.
The package only recommends a container engine and does not bundle one.

## Local testing

From the project root:

- copy packaging into place: `cp -a packaging/debian/debian ./debian`
- build source/binary package: `dpkg-buildpackage -us -uc -b`
- inspect package metadata: `dpkg-parsechangelog`
