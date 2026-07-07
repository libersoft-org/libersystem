#!/usr/bin/env bash
# Boot the aarch64 kernel under QEMU's `virt` machine (M116 bring-up).
#
# Used as the cargo runner for the aarch64-unknown-none target and standalone.
# The kernel is a low-linked static ELF; QEMU `-kernel` loads it into DRAM at
# 0x4008_0000 with the MMU off and enters `_start`. QEMU's ELF path does not hand
# the kernel a device tree, so we dump the machine's generated DTB and load it at
# a fixed address (DTB_ADDR, which the kernel scans for). Serial (PL011) is the
# console. Headless by default; SERIAL= overrides the serial sink.
set -euo pipefail

KERNEL="${1:?usage: qemu-aarch64.sh <kernel-elf>}"
SERIAL="${SERIAL:-mon:stdio}"
SMP="${SMP:-1}"
MEM="${MEM:-512M}"
DTB_ADDR="${DTB_ADDR:-0x4A000000}"

MACHINE="virt,gic-version=2"
DTB_FILE="$(mktemp /tmp/qemu-virt-XXXXXX.dtb)"
trap 'rm -f "$DTB_FILE"' EXIT

# System volume disk: a virtio-blk disk holding the packed volume archive at LBA 0.
# StorageService reads that factory archive from LBA 0, formats a LiberFS past it,
# and seeds vol://system - so the userspace boot chain can reach the shell. The
# kernel build.rs writes volume-aarch64.pkg into this script's .build directory.
HERE="$(cd "$(dirname "$0")" && pwd)"
VOLUME_PKG="$HERE/.build/volume-aarch64.pkg"
VIRTIO_DISK="$HERE/.build/virtio-blk-aarch64.img"
DISK_ARGS=()
if [[ -f "$VOLUME_PKG" ]]; then
	VIRTIO_DISK_SIZE=$((128 * 1024 * 1024))
	if [[ ! -f "$VIRTIO_DISK" || "$(stat -c%s "$VIRTIO_DISK")" -ne "$VIRTIO_DISK_SIZE" ]]; then
		rm -f "$VIRTIO_DISK"
		truncate -s "$VIRTIO_DISK_SIZE" "$VIRTIO_DISK"
	fi
	# Re-lay the factory archive at LBA 0 every boot (conv=notrunc keeps the disk at
	# its full size); StorageService reformats and reseeds from it.
	dd if="$VOLUME_PKG" of="$VIRTIO_DISK" bs=512 conv=notrunc status=none
	DISK_ARGS=(-drive "if=none,id=vol0,format=raw,file=$VIRTIO_DISK" -device "virtio-blk-pci,drive=vol0,disable-legacy=on")
fi

# Media volumes for the storage StorageService instances - the same set the x86 runner
# builds, so the boot chain (shell hard-depends on media/iso/udf storage) reaches the
# shell here too. Each is a genuine filesystem image seeded from the volume/ dir; built
# once and reused. Skipped if its mkfs toolchain is missing (that instance then fails,
# like x86 without the tool). Files come from the shared volume/ seed directory.
VOLDIR="$HERE/../volume"
# exFAT media disk (vol://media), read-write.
FAT_DISK="$HERE/.build/fat-media-aarch64.img"
if [[ ! -f "$FAT_DISK" ]] && command -v mkfs.exfat >/dev/null; then
	truncate -s 16M "$FAT_DISK"
	if mkfs.exfat "$FAT_DISK" >/dev/null 2>&1; then
		FMNT="$HERE/.build/media-mnt-a64"
		mkdir -p "$FMNT"
		if mount -o loop "$FAT_DISK" "$FMNT" 2>/dev/null; then
			cp "$VOLDIR/hello.txt" "$VOLDIR/motd.txt" "$FMNT"/ 2>/dev/null || true
			umount "$FMNT" 2>/dev/null || true
		fi
		rmdir "$FMNT" 2>/dev/null || true
	else
		rm -f "$FAT_DISK"
	fi
fi
[[ -f "$FAT_DISK" ]] && DISK_ARGS+=(-drive "if=none,id=med0,format=raw,file=$FAT_DISK" -device "virtio-blk-pci,drive=med0,disable-legacy=on")
# ISO9660 disk (vol://iso), read-only.
ISO_DISK="$HERE/.build/iso-media-aarch64.iso"
if [[ ! -f "$ISO_DISK" ]] && command -v xorriso >/dev/null; then
	xorriso -as mkisofs -quiet -J -R -o "$ISO_DISK" "$VOLDIR" 2>/dev/null || true
fi
[[ -f "$ISO_DISK" ]] && DISK_ARGS+=(-drive "if=none,id=iso0,format=raw,file=$ISO_DISK" -device "virtio-blk-pci,drive=iso0,disable-legacy=on")
# UDF disk (vol://udf), read-only.
UDF_DISK="$HERE/.build/udf-media-aarch64.udf"
if [[ ! -f "$UDF_DISK" ]] && command -v mkfs.udf >/dev/null; then
	dd if=/dev/zero of="$UDF_DISK" bs=1M count=8 status=none 2>/dev/null || true
	if mkfs.udf --media-type=hd --blocksize=2048 "$UDF_DISK" >/dev/null 2>&1; then
		UMNT="$HERE/.build/udf-mnt-a64"
		mkdir -p "$UMNT"
		if mount -o loop "$UDF_DISK" "$UMNT" 2>/dev/null; then
			cp "$VOLDIR"/* "$UMNT"/ 2>/dev/null || true
			umount "$UMNT" 2>/dev/null || true
		fi
		rmdir "$UMNT" 2>/dev/null || true
	else
		rm -f "$UDF_DISK"
	fi
fi
[[ -f "$UDF_DISK" ]] && DISK_ARGS+=(-drive "if=none,id=udf0,format=raw,file=$UDF_DISK" -device "virtio-blk-pci,drive=udf0,disable-legacy=on")

# Dump the machine's device tree (same machine config as the boot below), then
# boot with it loaded at DTB_ADDR.
qemu-system-aarch64 \
	-machine "$MACHINE,dumpdtb=$DTB_FILE" \
	-cpu cortex-a72 \
	-smp "$SMP" \
	-m "$MEM" \
	-display none >/dev/null 2>&1

qemu-system-aarch64 \
	-machine "$MACHINE" \
	-cpu cortex-a72 \
	-smp "$SMP" \
	-m "$MEM" \
	-kernel "$KERNEL" \
	-device loader,file="$DTB_FILE",addr="$DTB_ADDR" \
	-serial "$SERIAL" \
	-display none \
	-no-reboot \
	"${DISK_ARGS[@]}" \
	${QEMU_EXTRA:-}
