#!/usr/bin/env bash
# mkimage.sh - assemble bootable OS images from the kernel ELF.
#
# Subcommands:
#   mkimage.sh iso <kernel-elf>          build the shipping CD image (.iso): system volume, no
#                                        factory archive
#   mkimage.sh testiso <kernel-elf>      build the test CD image (-test.iso): the factory archive
#                                        the kernel suite needs as its fixture
#   mkimage.sh img <kernel-elf> [size]   build a raw UEFI-only disk image (.img)
#
# The platform is UEFI-only and boots through the own loader. Both images carry a
# FAT boot filesystem holding the loader at /EFI/BOOT/BOOTX64.EFI plus the files it
# reads from that same volume: the kernel at /kernel and the init/volume packages
# at their product.conf names. The ISO exposes the FAT image as its UEFI El Torito
# boot entry (OVMF has no ISO9660 driver, so the loader can only read the FAT
# volume); the disk image is a GPT disk with a single EFI System Partition. No
# root or loop mount is needed.
#
# `size` (img only) accepts truncate-style suffixes (e.g. 64M, 1G); default 64M.
#
# STRIP env var selects how much is stripped from the staged kernel:
#   STRIP=debug  (default) drop only the DWARF debug info (keeps the symbol table)
#   STRIP=all              also drop the symbol table for the smallest image
# Both only remove non-loadable sections, so booting is unaffected either way.
#
# The artifact is written to .build/boot/<product-slug>.{iso,img}; its path is
# printed to stdout (progress goes to stderr) so callers can capture it.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
# The own UEFI loader's EFI binary, staged into the boot image as BOOTX64.EFI.
LOADER_EFI="${LOADER_EFI:-$REPO_ROOT/.build/cargo/loader/x86_64-unknown-uefi/debug/libersystem-loader.efi}"

# product metadata (single source of truth)
# shellcheck source=/dev/null
source "$REPO_ROOT/product.conf"

BUILD="$REPO_ROOT/.build/boot"
SLUG="$(echo "$PRODUCT_NAME" | tr '[:upper:]' '[:lower:]')"

cleanup() {
	rm -f "$BUILD/$SLUG.iso.$$.candidate" "$BUILD/$SLUG.img.$$.candidate"
	rm -f "$BUILD/$SLUG.iso.build-key.tmp.$$" "$BUILD/$SLUG.img.build-key.tmp.$$"
}
trap cleanup EXIT

# operate on raw partition offsets without tripping mtools' geometry checks
export MTOOLS_SKIP_CHECK=1

info() { echo "mkimage: $*" >&2; }
die() {
	echo "mkimage: $*" >&2
	exit 1
}

# resolve the strip level (STRIP=debug|all) to an objcopy flag, once and up front
STRIP="${STRIP:-debug}"
case "$STRIP" in
debug) STRIP_FLAG="--strip-debug" ;;
all) STRIP_FLAG="--strip-all" ;;
*) die "invalid STRIP='$STRIP' (expected 'debug' or 'all')" ;;
esac

# stage the kernel for an image: strip it per STRIP_FLAG. The loader loads only
# the PT_LOAD segments and GDB reads symbols from the on-disk build, so the
# stripped sections are dead weight in a bootable image. Prints the staged path on stdout.
stage_kernel() {
	local src="$1" out="$BUILD/kernel"
	mkdir -p "$BUILD"
	objcopy "$STRIP_FLAG" "$src" "$out"
	info "kernel: stripped ($STRIP) $(stat -c %s "$src") -> $(stat -c %s "$out") bytes"
	echo "$out"
}

# Check every boot artifact the manifest names against what is actually on disk. The manifest is
# the single statement of what an image contains; this is how packaging enforces it rather than
# restating the list. Packaging compiles nothing - it verifies and assembles - so a missing
# artifact is an error here, unlike the compile phase where a missing optional library warns and
# the build carries on.
verify_boot_artifacts() {
	local staged_kernel="$1"
	local kind name destination source missing=0
	while read -r kind name destination; do
		[[ -n "$kind" ]] || continue
		case "$kind" in
		kernel) source="$staged_kernel" ;;
		loader) source="$LOADER_EFI" ;;
		init-package | volume-package) source="$BUILD/$destination" ;;
		*) die "manifest names boot artifact kind '$kind', which this image builder cannot stage" ;;
		esac
		if [[ ! -f "$source" ]]; then
			echo "mkimage: missing $kind '$name': $source" >&2
			missing=1
		fi
	done < <(cd "$REPO_ROOT/src/tools/system-manifest" && cargo run --quiet -- boot-artifacts)
	((missing == 0)) || die "packaging needs every artifact built first - run 'just build'"
}

# build a hybrid ISO (BIOS El Torito + UEFI), bootable as a CD or off a USB stick
# build a UEFI-only ISO, bootable as a CD or off a USB stick
make_iso() {
	local kernel="$1" test_medium="${2:-0}"
	local final="$BUILD/$SLUG.iso" out="$BUILD/$SLUG.iso.$$.candidate"
	if [[ "$test_medium" == "1" ]]; then
		final="$BUILD/$SLUG-test.iso"
		out="$BUILD/$SLUG-test.iso.$$.candidate"
	fi
	local iso_root="$BUILD/iso_root"
	rm -f "$out"

	local staged
	staged="$(stage_kernel "$kernel")"
	verify_boot_artifacts "$staged"

	# The FAT El Torito boot image. OVMF has no ISO9660 driver, so everything the
	# loader reads (the kernel and the packages) must live on this FAT filesystem -
	# the volume the loader is booted from - alongside the loader itself. The ISO
	# around it only carries this image as its UEFI El Torito boot entry.
	local efi_img="$BUILD/efiboot.img"
	local bytes total
	# TWO media, one builder. The SHIPPING ISO carries the system VOLUME - a LiberFS image the
	# running system copies into memory, because a CD cannot be written - and no factory archive.
	# The TEST ISO carries the archive instead, because the x86_64 kernel suite boots this exact
	# artifact and reads `volume.pkg` twice: as the source its fixture volume is built from, and as
	# the table of expected file contents.
	#
	# The split is the point of the item, and it was briefly collapsed on the belief that nothing
	# booted the archive-carrying ISO. The device-tree runners do build their own ESP - but the
	# x86_64 runner calls this script and boots what it returns, which is how a shipping medium and
	# a test medium had become one artifact in the first place.
	local payload="$BUILD/system-volume-x86_64.img" payload_name="system-volume.img"
	if [[ "$test_medium" == "1" ]]; then
		payload="$BUILD/$VOLUME_PACKAGE"
		payload_name="$VOLUME_PACKAGE"
		[[ -f "$payload" ]] || die "testiso: no volume package at $payload (run \`just packages\`)"
	else
		[[ -f "$payload" ]] || die "iso: no system volume at $payload (run \`just system-volume\`)"
	fi
	# The init package is counted whether or not it is staged: it is the smaller of the two payloads
	# and over-sizing a FAT image by a few megabytes is cheaper than getting it wrong.
	bytes=$(($(stat -c%s "$staged") + $(stat -c%s "$BUILD/$INIT_PACKAGE") + $(stat -c%s "$payload") + $(stat -c%s "$LOADER_EFI")))
	# FAT overhead + slack, rounded up to a whole MiB (min 32 MiB).
	total=$(((bytes + 16 * 1024 * 1024) / (1024 * 1024) + 1))
	((total < 32)) && total=32
	rm -f "$efi_img"
	truncate -s "${total}M" "$efi_img"
	mformat -i "$efi_img" ::
	mmd -i "$efi_img" ::/EFI ::/EFI/BOOT
	mcopy -i "$efi_img" "$LOADER_EFI" ::/EFI/BOOT/BOOTX64.EFI
	mcopy -i "$efi_img" "$staged" ::/kernel
	# The SHIPPING medium carries no `init.pkg`: its system volume names its own bootstrap programs
	# in `etc/bootstrap.list` and the loader assembles the set from there, which is what this
	# milestone set out to do. The TEST medium still needs it - it carries the factory archive
	# rather than a volume, so there is no list to read.
	if [[ "$test_medium" == "1" ]]; then
		mcopy -i "$efi_img" "$BUILD/$INIT_PACKAGE" "::/$INIT_PACKAGE"
	fi
	mcopy -i "$efi_img" "$payload" "::/$payload_name"

	rm -rf "$iso_root"
	mkdir -p "$iso_root/boot"
	cp "$efi_img" "$iso_root/boot/efiboot.img"

	# UEFI-only El Torito: no BIOS boot entry. The EFI image is also exposed as a
	# GPT partition so the ISO boots when dd'd to a USB stick.
	xorriso -as mkisofs -quiet \
		--efi-boot boot/efiboot.img \
		-efi-boot-part --efi-boot-image \
		--protective-msdos-label \
		"$iso_root" -o "$out" 2>/dev/null
	mv "$out" "$final"

	info "wrote $final"
	echo "$final"
}

# build a raw GPT disk image for a USB stick / SD card / hard disk
make_img() {
	local kernel="$1" size="${2:-64M}" final="$BUILD/$SLUG.img" out="$BUILD/$SLUG.img.$$.candidate"

	mkdir -p "$BUILD"
	rm -f "$out"
	truncate -s "$size" "$out"

	# GPT with two partitions: an EFI System Partition (ef00, FAT) holding the loader and the
	# boot fallback copies, and a LiberFS system volume holding everything else.
	#
	# The second partition is what makes this an installed system rather than a boot medium. The
	# storage service finds it by the LiberFS type GUID rather than by device order, and the
	# loader finds it by its superblock - firmware exposes a partition as its own block handle, so
	# LBA 0 of that handle is the volume's own start (M0138).
	#
	# The ESP is fixed at 32 MiB: it needs the loader, the kernel and the init fallback, and every
	# byte beyond that is a byte the system volume does not get.
	local esp_end=$((2048 + 32 * 1024 * 1024 / 512 - 1))
	sgdisk "$out" -n "1:2048:$esp_end" -t 1:ef00 -c 1:ESP >/dev/null
	sgdisk "$out" -n 2:0:0 -t 2:4C424653-0001-4000-8000-4C6962657246 -c 2:system >/dev/null

	# read back the ESP's exact start and length (mtools cannot parse GPT, so we
	# build the FAT filesystem as a standalone image and splice it into place).
	local esp_start esp_sectors
	esp_start="$(sgdisk -i 1 "$out" | awk '/^First sector:/ {print $3}')"
	esp_sectors="$(sgdisk -i 1 "$out" | awk '/^Partition size:/ {print $3}')"

	local esp="$BUILD/esp.img"
	rm -f "$esp"
	truncate -s "$((esp_sectors * 512))" "$esp"

	local staged
	staged="$(stage_kernel "$kernel")"
	verify_boot_artifacts "$staged"

	mformat -i "$esp" ::
	mmd -i "$esp" ::/EFI ::/EFI/BOOT
	mcopy -i "$esp" "$LOADER_EFI" ::/EFI/BOOT/BOOTX64.EFI
	mcopy -i "$esp" "$staged" ::/kernel
	mcopy -i "$esp" "$BUILD/$INIT_PACKAGE" "::/$INIT_PACKAGE"
	# No volume archive: the system volume is a filesystem in partition 2, and the archive exists
	# only as the kernel test suite's fixture (M0138).

	# splice the populated FAT filesystem into the ESP region of the disk
	dd if="$esp" of="$out" bs=512 seek="$esp_start" conv=notrunc status=none
	rm -f "$esp"

	# Lay the system volume into partition 2. Built by `just system-volume`, which runs after the
	# kernel so the image carries the kernel that was just linked.
	local volume="$BUILD/system-volume-x86_64.img"
	if [[ -f "$volume" ]]; then
		local sys_start sys_sectors
		sys_start="$(sgdisk -i 2 "$out" | awk '/^First sector:/ {print $3}')"
		sys_sectors="$(sgdisk -i 2 "$out" | awk '/^Partition size:/ {print $3}')"
		local volume_sectors=$(($(stat -c%s "$volume") / 512))
		if ((volume_sectors > sys_sectors)); then
			echo "mkimage: the system volume ($((volume_sectors / 2048)) MiB) does not fit the partition ($((sys_sectors / 2048)) MiB); build a larger image" >&2
			exit 1
		fi
		dd if="$volume" of="$out" bs=512 seek="$sys_start" conv=notrunc status=none
	else
		echo "mkimage: no system volume at $volume; the image has an empty system partition" >&2
	fi
	mv "$out" "$final"

	info "wrote $final ($size, GPT: ESP)"
	echo "$final"
}

cmd="${1:-}"
[[ $# -ge 2 ]] || die "usage: mkimage.sh {iso|img} <kernel-elf> [size]"
kernel="$2"
[[ -f "$kernel" ]] || die "kernel ELF not found: $kernel"
kernel="$(realpath -m "$kernel")"

mkdir -p "$BUILD"
command -v flock >/dev/null
command -v sha256sum >/dev/null
exec 9>"$BUILD/$SLUG.image.lock"
flock 9

case "$cmd" in
iso)
	output="$BUILD/$SLUG.iso"
	mode_input="iso"
	;;
testiso)
	output="$BUILD/$SLUG-test.iso"
	mode_input="testiso"
	;;
img)
	output="$BUILD/$SLUG.img"
	mode_input="img:${3:-64M}"
	;;
*) die "unknown subcommand '$cmd' (expected 'iso', 'testiso' or 'img')" ;;
esac

# The same manifest-driven check as the image builders below, run before the cache key is
# computed - hashing inputs that do not all exist would cache a decision made on a partial tree.
# The kernel is checked here as the ELF it was given; the builders re-check the stripped copy.
verify_boot_artifacts "$kernel"
key_file="$output.build-key"
key="$({
	printf 'format=liber-boot-image-input-v1\n'
	printf 'mode=%s\n' "$mode_input"
	printf 'strip=%s\n' "$STRIP"
	# BOTH payloads are hashed, whichever medium is being built: the shipping ISO carries the
	# system volume and the test ISO the archive, and keying on only one would serve a stale image
	# whenever the other changed. `mode=` above already separates the two outputs.
	sha256sum "$0" "$REPO_ROOT/product.conf" "$kernel" "$LOADER_EFI" "$BUILD/$INIT_PACKAGE" "$BUILD/$VOLUME_PACKAGE" "$BUILD/system-volume-x86_64.img"
} | sha256sum | awk '{print $1}')"
if [[ -f "$output" && -f "$key_file" && "$(<"$key_file")" == "$key" ]]; then
	info "cache hit $output"
	echo "$output"
	exit 0
fi
info "cache miss $output; rebuilding"

case "$cmd" in
iso) make_iso "$kernel" 0 ;;
testiso) make_iso "$kernel" 1 ;;
img) make_img "$kernel" "${3:-64M}" ;;
esac
printf '%s\n' "$key" >"$key_file.tmp.$$"
mv "$key_file.tmp.$$" "$key_file"
