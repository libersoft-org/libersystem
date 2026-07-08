#!/usr/bin/env bash
# qemu-riscv64.sh - boot the riscv64 kernel under `qemu-system-riscv64 -machine virt`
# over OpenSBI (M117 bring-up).
#
# QEMU loads OpenSBI (the -bios firmware, M-mode) and the kernel ELF (-kernel);
# OpenSBI's fw_dynamic jumps to the kernel entry `_start` in S-mode with a0=hartid,
# a1=DTB. This is the minimal direct-boot runner; disks + virtio devices are added in
# later increments (the aarch64 runner `qemu-aarch64.sh` is the template).

set -euo pipefail

KERNEL="${1:?usage: qemu-riscv64.sh <kernel-elf>}"
SERIAL="${SERIAL:-mon:stdio}"
SMP="${SMP:-1}"
MEM="${MEM:-512M}"
# `default` uses QEMU's bundled OpenSBI (fw_dynamic); override with BIOS=<path>.
BIOS="${BIOS:-default}"

# System volume disk: a virtio-blk disk holding the packed volume archive at LBA 0.
# StorageService reads that factory archive, formats a LiberFS past it, and seeds
# vol://system - so the userspace boot chain reaches the pinned service set. The kernel
# build.rs writes volume-riscv64.pkg into this script's .build directory. The userspace
# virtio-blk driver polls the disk (no interrupt needed), so this works over the PLIC
# without the per-device interrupt path. Attach only when the volume package exists.
HERE="$(cd "$(dirname "$0")" && pwd)"
VOLUME_PKG="$HERE/.build/volume-riscv64.pkg"
VIRTIO_DISK="$HERE/.build/virtio-blk-riscv64.img"
DISK_ARGS=()
if [[ -f "$VOLUME_PKG" ]]; then
	VIRTIO_DISK_SIZE=$((128 * 1024 * 1024))
	if [[ ! -f "$VIRTIO_DISK" || "$(stat -c%s "$VIRTIO_DISK")" -ne "$VIRTIO_DISK_SIZE" ]]; then
		rm -f "$VIRTIO_DISK"
		truncate -s "$VIRTIO_DISK_SIZE" "$VIRTIO_DISK"
	fi
	dd if="$VOLUME_PKG" of="$VIRTIO_DISK" bs=512 conv=notrunc status=none
	DISK_ARGS=(-drive "if=none,id=vol0,format=raw,file=$VIRTIO_DISK" -device "virtio-blk-pci,drive=vol0")
fi

exec qemu-system-riscv64 \
	-machine virt \
	-cpu rv64 \
	-smp "$SMP" \
	-m "$MEM" \
	-bios "$BIOS" \
	-kernel "$KERNEL" \
	-serial "$SERIAL" \
	-display none \
	-no-reboot \
	"${DISK_ARGS[@]}" \
	${QEMU_EXTRA:-}
