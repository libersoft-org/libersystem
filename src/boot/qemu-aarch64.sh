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
	${QEMU_EXTRA:-}
