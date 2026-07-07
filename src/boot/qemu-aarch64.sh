#!/usr/bin/env bash
# Boot the aarch64 kernel under QEMU's `virt` machine (M116 bring-up).
#
# Used as the cargo runner for the aarch64-unknown-none target and standalone.
# The kernel is a low-linked static ELF; QEMU `-kernel` loads it into DRAM at
# 0x4008_0000 with the MMU off and enters `_start` with x0 = the DTB. Serial
# (PL011) is the console. Headless by default; SERIAL= overrides the serial sink.
set -euo pipefail

KERNEL="${1:?usage: qemu-aarch64.sh <kernel-elf>}"
SERIAL="${SERIAL:-mon:stdio}"
SMP="${SMP:-1}"
MEM="${MEM:-512M}"

exec qemu-system-aarch64 \
	-machine virt \
	-cpu cortex-a72 \
	-smp "$SMP" \
	-m "$MEM" \
	-kernel "$KERNEL" \
	-serial "$SERIAL" \
	-display none \
	-no-reboot
