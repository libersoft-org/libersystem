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
