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
# A SHAPE IS THREE FILES, not one. The image is what a consumer boots, the uuid beside it is what a
# medium's pairing is checked against, and the stamp is the receipt `test.sh` reads to decide the
# userspace is current. Naming the images apart while the other two stayed shared left the same
# defect in the two files nobody looks at: an `./image.sh` that refreshed the test shape's receipt
# vouches for a build it never ran.
TEST_UUID="$BUILD/system-volume-$ARCH.uuid"
BOOT_UUID="$BUILD/system-volume-bootable-$ARCH.uuid"
TEST_STAMP=".build/state/built-$ARCH-volume-test"
BOOT_STAMP=".build/state/built-$ARCH-volume"

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

# THE STAMP IS THE ONE FILE WHOSE CONTENT CANNOT ANSWER THIS. Both shapes are built from the same
# sources, so both receipts hold the same digest - and a command that rewrites the OTHER shape's
# receipt writes bytes identical to the ones already there. A content comparison sees nothing and
# passes, while the harm is exactly that a receipt was issued for a build that did not happen. What
# distinguishes them is whether the file was written at all, so the stamp is compared by
# modification time and the two artifacts, whose bytes do differ when they are wrong, by content.
written_at() {
	[[ -f "$1" ]] || {
		echo "absent"
		return
	}
	stat -c '%y' "$1"
}

# Print each of a shape's three files with the identity that can answer for it.
shape_state() {
	printf 'img=%s uuid=%s stamp=%s' "$(digest "$1")" "$(digest "$2")" "$(written_at "$3")"
}

# Both shapes have to exist before either order can be tested - all three files of each, because a
# shape with a missing receipt is a shape whose consumer refuses to run rather than one that is fine.
[[ -f "$TEST_SHAPE" ]] || fail "no test-shape volume at $TEST_SHAPE - build it with:  ./build.sh --arch $ARCH --part volume"
[[ -f "$BOOT_SHAPE" ]] || fail "no bootable volume at $BOOT_SHAPE - build it with:  ./build.sh --arch $ARCH --kernel-on-volume --part volume"
[[ -f "$TEST_UUID" ]] || fail "no pairing beside the test shape at $TEST_UUID - build it with:  ./build.sh --arch $ARCH --part volume"
[[ -f "$BOOT_UUID" ]] || fail "no pairing beside the bootable shape at $BOOT_UUID - build it with:  ./build.sh --arch $ARCH --kernel-on-volume --part volume"
[[ -f "$TEST_STAMP" ]] || fail "no build stamp for the test shape at $TEST_STAMP - build it with:  ./build.sh --arch $ARCH --part volume"
[[ -f "$BOOT_STAMP" ]] || fail "no build stamp for the bootable shape at $BOOT_STAMP - build it with:  ./build.sh --arch $ARCH --kernel-on-volume --part volume"

# AND THE TWO PAIRINGS DIFFER, for the same reason the two images must. The uuid is derived from the
# volume's contents, so two shapes that share one are two names over one artifact.
[[ "$(digest "$TEST_UUID")" != "$(digest "$BOOT_UUID")" ]] || fail "both shapes carry the same pairing, so a medium built for one names the other just as well"

# AND EACH PAIRING NAMES ITS OWN IMAGE, which "they differ" does not say.
#
# CHECKED HERE AND AGAIN AFTER EACH BUILD. Asking only at the start says the tree was consistent
# BEFORE this gate did anything, which is the one moment the gate did not create - a build that
# writes a sidecar for a volume it did not write would go straight past. Each order below re-asks it
# about the shape it just built.
#
# Two sidecars swapped between the shapes are still different from each other, both still exist, and
# every check above passes - while each medium names the volume it is not paired with, which is the
# defect this milestone is about wearing a different hat. `pairing_matches_volume` is the check M4
# made shared for exactly this question, so this is two calls rather than a second copy of it.
pairing_matches_volume "$(cat "$TEST_UUID")" "$TEST_SHAPE" || fail "the test shape's pairing at $TEST_UUID does not name the volume beside it"
pairing_matches_volume "$(cat "$BOOT_UUID")" "$BOOT_SHAPE" || fail "the bootable shape's pairing at $BOOT_UUID does not name the volume beside it"
echo "build-order: each shape's pairing names its own image"

# THE SHAPES ARE DIFFERENT ARTIFACTS. If they were equal, naming them apart would have changed
# nothing and this gate would pass on a tree where the defect was still present.
[[ "$(digest "$TEST_SHAPE")" != "$(digest "$BOOT_SHAPE")" ]] || fail "both shapes have the same contents, so this gate cannot tell them apart and neither can a consumer"

# ORDER ONE: the shipping shape, then the test shape. The test build must not disturb the shipping one.
before="$(shape_state "$BOOT_SHAPE" "$BOOT_UUID" "$BOOT_STAMP")"
./build.sh --arch "$ARCH" --part volume >/dev/null 2>&1 || fail "./build.sh --part volume failed"
after="$(shape_state "$BOOT_SHAPE" "$BOOT_UUID" "$BOOT_STAMP")"
[[ "$before" == "$after" ]] || fail "building the test shape disturbed the bootable one - they are not two artifacts
    before: $before
    after:  $after"
pairing_matches_volume "$(cat "$TEST_UUID")" "$TEST_SHAPE" || fail "the test build left a pairing at $TEST_UUID that does not name the volume it just wrote"
echo "build-order: building the test shape left the bootable image, pairing and stamp untouched"

# ORDER TWO: the test shape, then the shipping shape. The shipping build must not disturb the test one.
before="$(shape_state "$TEST_SHAPE" "$TEST_UUID" "$TEST_STAMP")"
./build.sh --arch "$ARCH" --kernel-on-volume --part volume >/dev/null 2>&1 || fail "./build.sh --kernel-on-volume --part volume failed"
after="$(shape_state "$TEST_SHAPE" "$TEST_UUID" "$TEST_STAMP")"
[[ "$before" == "$after" ]] || fail "building the bootable shape disturbed the test one - they are not two artifacts
    before: $before
    after:  $after"
pairing_matches_volume "$(cat "$BOOT_UUID")" "$BOOT_SHAPE" || fail "the bootable build left a pairing at $BOOT_UUID that does not name the volume it just wrote"
echo "build-order: building the bootable shape left the test image, pairing and stamp untouched"

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

echo "build-order: the two shapes are two artifacts of three files each, and neither command disturbs the other's"
