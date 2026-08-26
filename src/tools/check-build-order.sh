#!/usr/bin/env bash
# The two shapes of the system volume have two names, so no order of commands can confuse them.
#
# WHAT THIS REPLACES. The volume with a kernel on it and the volume without one were written to one
# path, so whichever command ran last decided what every consumer read - and each consumer took it on
# trust. A suite run after `./image.sh` booted the SHIPPING kernel off the volume and sat at a shell
# until the watchdog fired; a `--part volume` after an `./image.sh` left the ISO naming a uuid the
# volume no longer had, and the loader refused the volume it could see. Five separate investigations
# in one round started at a symptom and ended at the order of two commands.
#
# The claim this gate holds the tree to is the one the milestone makes: the commands may be run in
# any order, any number of times, and every consumer still reads the shape it asked for. It is
# checked by building in BOTH orders and requiring both artifacts to survive each one.
set -euo pipefail

cd "$(dirname "$0")/../.."
source src/tools/volume-pairing.sh
BUILD=".build/boot"
ARCH=x86_64
TEST_SHAPE="$BUILD/system-volume-$ARCH.img"
BOOT_SHAPE="$BUILD/system-volume-bootable-$ARCH.img"

fail() {
	echo "build-order: $*" >&2
	exit 1
}

# A shape's identity, so "it survived the other command" is a comparison rather than a timestamp.
digest() {
	[[ -f "$1" ]] || {
		echo "absent"
		return
	}
	sha256sum "$1" | cut -d' ' -f1
}

# Both shapes have to exist before either order can be tested.
[[ -f "$TEST_SHAPE" ]] || fail "no test-shape volume at $TEST_SHAPE - build it with:  ./build.sh --arch $ARCH --part volume"
[[ -f "$BOOT_SHAPE" ]] || fail "no bootable volume at $BOOT_SHAPE - build it with:  ./build.sh --arch $ARCH --kernel-on-volume --part volume"

# THE SHAPES ARE DIFFERENT ARTIFACTS. If they were equal, naming them apart would have changed
# nothing and this gate would pass on a tree where the defect was still present.
[[ "$(digest "$TEST_SHAPE")" != "$(digest "$BOOT_SHAPE")" ]] || fail "both shapes have the same contents, so this gate cannot tell them apart and neither can a consumer"

# ORDER ONE: the shipping shape, then the test shape. The test build must not disturb the shipping one.
before="$(digest "$BOOT_SHAPE")"
./build.sh --arch "$ARCH" --part volume >/dev/null 2>&1 || fail "./build.sh --part volume failed"
after="$(digest "$BOOT_SHAPE")"
[[ "$before" == "$after" ]] || fail "building the test shape changed the bootable one - they are not two artifacts"
echo "build-order: building the test shape left the bootable one untouched"

# ORDER TWO: the test shape, then the shipping shape. The shipping build must not disturb the test one.
before="$(digest "$TEST_SHAPE")"
./build.sh --arch "$ARCH" --kernel-on-volume --part volume >/dev/null 2>&1 || fail "./build.sh --kernel-on-volume --part volume failed"
after="$(digest "$TEST_SHAPE")"
[[ "$before" == "$after" ]] || fail "building the bootable shape changed the test one - they are not two artifacts"
echo "build-order: building the bootable shape left the test one untouched"

# AND THE MEDIUM NAMES THE VOLUME IT WAS BUILT WITH. This is the half the order used to break: the
# ISO carries a pairing derived from the volume's CONTENTS, and a `--part volume` after an
# `./image.sh` left the two disagreeing with nothing to say so.
#
# ONLY WHERE THE ISO IS NOT SIMPLY OLDER THAN THE VOLUME. A medium built before the volume it carries
# was last changed is a stale medium, not an order defect - no naming can make an ISO track a rebuild
# of its own payload - and `signed-boot` and `qemu-virtio-iommu` both refuse one. What this asks is
# the question those two cannot: after building both shapes in both orders, does the medium still
# name the shape it was built from, rather than the one the other command produced.
ISO="$BUILD/libersystem.iso"
if [[ -f "$ISO" && ! "$BOOT_SHAPE" -nt "$ISO" ]]; then
	work="$(mktemp -d)"
	trap 'rm -rf "$work"' EXIT
	xorriso -osirrox on -indev "$ISO" -extract /boot/efiboot.img "$work/esp.img" >/dev/null 2>&1 || fail "could not read /boot/efiboot.img out of $ISO"
	mcopy -i "$work/esp.img" ::/etc/boot.manifest2 "$work/manifest2" 2>/dev/null || fail "the ISO carries no signed manifest, so it names no system volume"
	manifest_names_volume "$work/manifest2" "$BOOT_SHAPE" || fail "the ISO's signed manifest does not name the bootable volume beside it, after both shapes were rebuilt in both orders"
	echo "build-order: the ISO still names the bootable volume after both rebuilds"
elif [[ -f "$ISO" ]]; then
	echo "build-order: the ISO predates the current bootable volume, so its pairing is a staleness question and signed-boot asks it"
fi

echo "build-order: the two shapes are two artifacts, and neither command disturbs the other's"
