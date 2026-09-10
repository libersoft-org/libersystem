#!/usr/bin/env bash
# What DMA mode an x86_64 boot medium is SIGNED for, read off the medium itself.
#
#   image-dma-mode.sh ISO        prints `enforcing-required`, `no-iommu` or `harness` (a tag-0 set,
#                                whose boot takes the mode from the harness carrier), or `legacy`
#                                for a medium signed before the record existed
#
# The value is a signed header field FROZEN AT IMAGE ASSEMBLY, and `run.sh --no-iommu` is a flag at
# boot: the flag selects an image and builds a machine, and it has to know what the image it is
# about to boot was signed for, so it can refuse a pairing the kernel would refuse anyway - an
# enforcing image on a machine with no controller, or a degraded one with a controller - at the host,
# with both values named, rather than booting into a refusal the operator has to decode from a
# guest log. Read from the medium rather than from a sidecar, because the medium is what boots.
#
# THE MANIFEST IS ON THE EL TORITO ESP inside the ISO - OVMF has no ISO9660 driver, so everything
# the loader reads lives on that FAT image - which is why this extracts `boot/efiboot.img` and reads
# `etc/boot.manifest2` out of it. No signature is checked here: this answers what the record CLAIMS,
# and the loader is what decides whether the claim is believed.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

die() {
	echo "image-dma-mode: $*" >&2
	exit 1
}

[[ $# -eq 1 ]] || die "usage: image-dma-mode.sh ISO"
iso="$1"
[[ -f "$iso" ]] || die "no image at $iso"
command -v xorriso >/dev/null || die "xorriso is not installed"
command -v mcopy >/dev/null || die "mtools is not installed"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
# `-osirrox on` allows extraction; the ESP image is the one El Torito entry a medium this tree
# assembles carries.
xorriso -osirrox on -indev "$iso" -extract /boot/efiboot.img "$work/efiboot.img" >/dev/null 2>&1 || die "$iso carries no boot/efiboot.img - not a medium this tree assembled"
mcopy -i "$work/efiboot.img" ::/etc/boot.manifest2 "$work/manifest2" 2>/dev/null || die "$iso carries no signed manifest on its ESP"
# The parse is the loader's, through the signing tool's inspect mode; the field of interest is the
# one line.
inspect="$(cd "$HERE/../tools/sign-manifest" && cargo run --quiet -- --inspect "$work/manifest2")" || die "the signed manifest on $iso is not one this tree reads"
if grep -q '^version: legacy$' <<<"$inspect"; then
	echo legacy
	exit 0
fi
mode="$(sed -n 's/^dma-mode: //p' <<<"$inspect")"
[[ -n "$mode" ]] || die "the signed manifest on $iso names no DMA mode field"
echo "$mode"
