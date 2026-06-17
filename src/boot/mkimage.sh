#!/usr/bin/env bash
# mkimage.sh - assemble bootable OS images from the kernel ELF.
#
# Subcommands:
#   mkimage.sh iso <kernel-elf>          build a hybrid BIOS+UEFI CD image (.iso)
#   mkimage.sh img <kernel-elf> [size]   build a raw BIOS+UEFI disk image (.img)
#
# Both images lay down the same files: the kernel at /boot/kernel, the Limine
# config at /boot/limine/limine.conf, and the Limine BIOS + UEFI stages, so they
# boot the same way on real hardware and in QEMU. The disk image is a GPT disk
# with a small BIOS boot partition (carrying the Limine BIOS stage) plus an EFI
# System Partition (FAT) holding everything else. No root or loop mount is needed.
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
LIMINE_DIR="${LIMINE_DIR:-$HOME/.local/share/limine}"

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

# render the bootloader config from its template, injecting the product name
render_conf() {
	sed "s|@PRODUCT_NAME@|$PRODUCT_NAME|g" "$HERE/limine.conf.in" >"$1"
}

# resolve the strip level (STRIP=debug|all) to an objcopy flag, once and up front
STRIP="${STRIP:-debug}"
case "$STRIP" in
debug) STRIP_FLAG="--strip-debug" ;;
all) STRIP_FLAG="--strip-all" ;;
*) die "invalid STRIP='$STRIP' (expected 'debug' or 'all')" ;;
esac

# stage the kernel for an image: strip it per STRIP_FLAG. Limine loads only the
# PT_LOAD segments and GDB reads symbols from the on-disk build, so the stripped
# sections are dead weight in a bootable image. Prints the staged path on stdout.
stage_kernel() {
	local src="$1" out="$BUILD/kernel"
	mkdir -p "$BUILD"
	objcopy "$STRIP_FLAG" "$src" "$out"
	info "kernel: stripped ($STRIP) $(stat -c %s "$src") -> $(stat -c %s "$out") bytes"
	echo "$out"
}

# build a hybrid ISO (BIOS El Torito + UEFI), bootable as a CD or off a USB stick
make_iso() {
	local kernel="$1" out="$BUILD/$SLUG.iso"
	local iso_root="$BUILD/iso_root"

	local staged
	staged="$(stage_kernel "$kernel")"

	rm -rf "$iso_root"
	mkdir -p "$iso_root/boot/limine" "$iso_root/EFI/BOOT"
	cp "$staged" "$iso_root/boot/kernel"
	render_conf "$iso_root/boot/limine/limine.conf"
	cp "$LIMINE_DIR/limine-bios.sys" "$iso_root/boot/limine/"
	cp "$LIMINE_DIR/limine-bios-cd.bin" "$iso_root/boot/limine/"
	cp "$LIMINE_DIR/limine-uefi-cd.bin" "$iso_root/boot/limine/"
	cp "$LIMINE_DIR/BOOTX64.EFI" "$iso_root/EFI/BOOT/"
	cp "$LIMINE_DIR/BOOTIA32.EFI" "$iso_root/EFI/BOOT/"

	xorriso -as mkisofs -quiet \
		-b boot/limine/limine-bios-cd.bin \
		-no-emul-boot -boot-load-size 4 -boot-info-table \
		--efi-boot boot/limine/limine-uefi-cd.bin \
		-efi-boot-part --efi-boot-image \
		--protective-msdos-label \
		"$iso_root" -o "$out" 2>/dev/null

	# embed the Limine BIOS stage so the ISO also boots when dd'd to a USB stick
	"$LIMINE_DIR/limine" bios-install "$out" >/dev/null 2>&1

	info "wrote $out"
	echo "$out"
}

# build a raw GPT disk image for a USB stick / SD card / hard disk
make_img() {
	local kernel="$1" size="${2:-64M}" out="$BUILD/$SLUG.img"

	mkdir -p "$BUILD"
	rm -f "$out"
	truncate -s "$size" "$out"

	# GPT layout: a small BIOS boot partition (ef02) carries the Limine BIOS stage
	# (a GPT disk leaves no room in the post-MBR gap), and an EFI System Partition
	# (ef00, FAT) holds the kernel, the Limine config and the UEFI stage.
	sgdisk "$out" \
		-n 1:2048:+1M -t 1:ef02 -c 1:"BIOS boot" \
		-n 2:0:0 -t 2:ef00 -c 2:ESP >/dev/null

	# BIOS boot: install the Limine stage into the BIOS boot partition
	"$LIMINE_DIR/limine" bios-install "$out" >/dev/null 2>&1

	# read back the ESP's exact start and length (mtools cannot parse GPT, so we
	# build the FAT filesystem as a standalone image and splice it into place).
	local esp_start esp_sectors
	esp_start="$(sgdisk -i 2 "$out" | awk '/^First sector:/ {print $3}')"
	esp_sectors="$(sgdisk -i 2 "$out" | awk '/^Partition size:/ {print $3}')"

	local esp="$BUILD/esp.img" rc="$BUILD/mtoolsrc"
	rm -f "$esp"
	truncate -s "$((esp_sectors * 512))" "$esp"
	printf 'drive z:\n  file="%s"\n' "$esp" >"$rc"
	export MTOOLSRC="$rc"

	local staged conf="$BUILD/limine.conf"
	staged="$(stage_kernel "$kernel")"
	render_conf "$conf"

	mformat z:
	mmd z:/EFI z:/EFI/BOOT z:/boot z:/boot/limine
	mcopy "$staged" z:/boot/kernel
	mcopy "$conf" z:/boot/limine/limine.conf
	mcopy "$LIMINE_DIR/limine-bios.sys" z:/boot/limine/
	mcopy "$LIMINE_DIR/BOOTX64.EFI" z:/EFI/BOOT/
	mcopy "$LIMINE_DIR/BOOTIA32.EFI" z:/EFI/BOOT/

	# splice the populated FAT filesystem into the ESP region of the disk
	dd if="$esp" of="$out" bs=512 seek="$esp_start" conv=notrunc status=none
	rm -f "$esp"

	info "wrote $out ($size, GPT: BIOS boot + ESP)"
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
