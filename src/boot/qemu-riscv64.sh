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
	${QEMU_EXTRA:-}
