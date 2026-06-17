#!/usr/bin/env bash
# qemu-run.sh - cargo runner: builds a bootable ISO (Limine) from the kernel ELF
# and launches QEMU.
#
# Usage: qemu-run.sh <kernel-elf>
# Env variables:
#   DEBUG=1   QEMU waits for GDB (-s -S) on port :1234
#   NOKVM=1   disable KVM (more reliable single-stepping under TCG)
#   TEST=1    test mode (isa-debug-exit, maps exit code to pass/fail)
#   SERIAL=   QEMU serial backend (default mon:stdio; e.g. file:boot.log)

set -euo pipefail

KERNEL="${1:?path to kernel ELF is missing}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# build the bootable ISO (mkimage.sh prints its path on stdout)
ISO="$("$HERE/mkimage.sh" iso "$KERNEL")"

# build the QEMU arguments
QEMU_ARGS=(
	-machine q35
	-m 512M
	-cdrom "$ISO"
	-boot d
	-serial "${SERIAL:-mon:stdio}"
	-display none
	-no-reboot
)

if [[ "${NOKVM:-0}" != "1" && -e /dev/kvm ]]; then
	QEMU_ARGS+=(-enable-kvm -cpu host -smp 4)
else
	QEMU_ARGS+=(-cpu qemu64 -smp 4)
fi

if [[ "${DEBUG:-0}" == "1" ]]; then
	QEMU_ARGS+=(-s -S)
	echo "[qemu-run] waiting for GDB on :1234 (run 'just gdb' in another panel)"
fi

if [[ "${TEST:-0}" == "1" ]]; then
	QEMU_ARGS+=(-device isa-debug-exit,iobase=0xf4,iosize=0x04)
	set +e
	qemu-system-x86_64 "${QEMU_ARGS[@]}"
	code=$?
	set -e
	# isa-debug-exit: success = (0x10 << 1) | 1 = 33
	[[ "$code" -eq 33 ]] && exit 0
	exit "$code"
fi

exec qemu-system-x86_64 "${QEMU_ARGS[@]}"
