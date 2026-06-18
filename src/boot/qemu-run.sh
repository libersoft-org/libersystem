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
	-no-reboot
)

# display backends: the DISPLAYS env is a space-separated list, any of `vnc` and
# `spice` (both may be given at once; empty = headless, serial only). The
# framebuffer is always rendered and can also be screenshotted via screenshot.sh.
#   vnc    a VNC server; VNC_ADDR sets the bind/display, default 0.0.0.0:0 (port 5900)
#   spice  a SPICE server; SPICE_PORT sets the TCP port, default 5930
want_vnc=0
want_spice=0
for display in ${DISPLAYS:-}; do
	case "$display" in
	vnc) want_vnc=1 ;;
	spice) want_spice=1 ;;
	none) ;;
	*) echo "qemu-run: unknown display '$display' (expected vnc and/or spice)" >&2 && exit 1 ;;
	esac
done

# A VNC server doubles as the local display; otherwise suppress the default UI
# (so spice-only and headless do not try to open a GTK/SDL window).
if [[ "$want_vnc" == "1" ]]; then
	QEMU_ARGS+=(-vnc "${VNC_ADDR:-0.0.0.0:0}")
else
	QEMU_ARGS+=(-display none)
fi
if [[ "$want_spice" == "1" ]]; then
	QEMU_ARGS+=(-spice "port=${SPICE_PORT:-5930},addr=0.0.0.0,disable-ticketing=on")
fi

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

# Expose a control monitor on a unix socket (alongside the stdio monitor) so
# boot/screenshot.sh can attach to this running instance and snap the live
# framebuffer at any time. Only for interactive runs (not the test path above).
MON_SOCK="$HERE/.build/qemu-monitor.sock"
mkdir -p "$HERE/.build"
rm -f "$MON_SOCK"
QEMU_ARGS+=(-monitor "unix:$MON_SOCK,server,nowait")

exec qemu-system-x86_64 "${QEMU_ARGS[@]}"
