#!/usr/bin/env bash
# mkimage.sh - assemble bootable OS images from the kernel ELF.
#
# Subcommands:
#   mkimage.sh iso <kernel-elf>          build a UEFI-only bootable CD image (.iso)
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
# The artifact is written to boot/.build/<product-slug>.{iso,img}; its path is
# printed to stdout (progress goes to stderr) so callers can capture it.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
# The own UEFI loader's EFI binary, staged into the boot image as BOOTX64.EFI.
LOADER_EFI="${LOADER_EFI:-$REPO_ROOT/src/loader/target/x86_64-unknown-uefi/debug/libersystem-loader.efi}"

# product metadata (single source of truth)
# shellcheck source=/dev/null
source "$REPO_ROOT/product.conf"

BUILD="$HERE/.build"
SLUG="$(echo "$PRODUCT_NAME" | tr '[:upper:]' '[:lower:]')"

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

# build a hybrid ISO (BIOS El Torito + UEFI), bootable as a CD or off a USB stick
# build a UEFI-only ISO, bootable as a CD or off a USB stick
make_iso() {
	local kernel="$1" out="$BUILD/$SLUG.iso"
	local iso_root="$BUILD/iso_root"

	local staged
	staged="$(stage_kernel "$kernel")"
	[[ -f "$LOADER_EFI" ]] || die "loader EFI not found: $LOADER_EFI (build the loader first)"
	[[ -f "$BUILD/$INIT_PACKAGE" ]] || die "init package not found: $BUILD/$INIT_PACKAGE (build the kernel first)"
	[[ -f "$BUILD/$VOLUME_PACKAGE" ]] || die "volume package not found: $BUILD/$VOLUME_PACKAGE (build the kernel first)"

	# The FAT El Torito boot image. OVMF has no ISO9660 driver, so everything the
	# loader reads (the kernel and the packages) must live on this FAT filesystem -
	# the volume the loader is booted from - alongside the loader itself. The ISO
	# around it only carries this image as its UEFI El Torito boot entry.
	local efi_img="$BUILD/efiboot.img"
	local bytes total
	bytes=$(($(stat -c%s "$staged") + $(stat -c%s "$BUILD/$INIT_PACKAGE") + $(stat -c%s "$BUILD/$VOLUME_PACKAGE") + $(stat -c%s "$LOADER_EFI")))
	# FAT overhead + slack, rounded up to a whole MiB (min 32 MiB).
	total=$(((bytes + 16 * 1024 * 1024) / (1024 * 1024) + 1))
	((total < 32)) && total=32
	rm -f "$efi_img"
	truncate -s "${total}M" "$efi_img"
	mformat -i "$efi_img" ::
	mmd -i "$efi_img" ::/EFI ::/EFI/BOOT
	mcopy -i "$efi_img" "$LOADER_EFI" ::/EFI/BOOT/BOOTX64.EFI
	mcopy -i "$efi_img" "$staged" ::/kernel
	mcopy -i "$efi_img" "$BUILD/$INIT_PACKAGE" "::/$INIT_PACKAGE"
	mcopy -i "$efi_img" "$BUILD/$VOLUME_PACKAGE" "::/$VOLUME_PACKAGE"

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

	info "wrote $out"
	echo "$out"
}

# build a raw GPT disk image for a USB stick / SD card / hard disk
make_img() {
	local kernel="$1" size="${2:-64M}" out="$BUILD/$SLUG.img"

	mkdir -p "$BUILD"
	rm -f "$out"
	truncate -s "$size" "$out"

	# GPT with a single EFI System Partition (ef00, FAT) holding the loader, the
	# kernel and the packages. No BIOS boot partition: the platform is UEFI-only.
	sgdisk "$out" -n 1:2048:0 -t 1:ef00 -c 1:ESP >/dev/null

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
	[[ -f "$LOADER_EFI" ]] || die "loader EFI not found: $LOADER_EFI (build the loader first)"
	[[ -f "$BUILD/$INIT_PACKAGE" ]] || die "init package not found: $BUILD/$INIT_PACKAGE (build the kernel first)"
	[[ -f "$BUILD/$VOLUME_PACKAGE" ]] || die "volume package not found: $BUILD/$VOLUME_PACKAGE (build the kernel first)"

	mformat -i "$esp" ::
	mmd -i "$esp" ::/EFI ::/EFI/BOOT
	mcopy -i "$esp" "$LOADER_EFI" ::/EFI/BOOT/BOOTX64.EFI
	mcopy -i "$esp" "$staged" ::/kernel
	mcopy -i "$esp" "$BUILD/$INIT_PACKAGE" "::/$INIT_PACKAGE"
	mcopy -i "$esp" "$BUILD/$VOLUME_PACKAGE" "::/$VOLUME_PACKAGE"

	# splice the populated FAT filesystem into the ESP region of the disk
	dd if="$esp" of="$out" bs=512 seek="$esp_start" conv=notrunc status=none
	rm -f "$esp"

	info "wrote $out ($size, GPT: ESP)"
	echo "$out"
}

cmd="${1:-}"
[[ $# -ge 2 ]] || die "usage: mkimage.sh {iso|img} <kernel-elf> [size]"
kernel="$2"
[[ -f "$kernel" ]] || die "kernel ELF not found: $kernel"
kernel="$(realpath -m "$kernel")"

case "$cmd" in
iso) make_iso "$kernel" ;;
img) make_img "$kernel" "${3:-64M}" ;;
*) die "unknown subcommand '$cmd' (expected 'iso' or 'img')" ;;
esac
