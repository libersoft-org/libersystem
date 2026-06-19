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
)

# virtio devices for the driver milestones (M23/M24): a scratch block disk, a
# user-mode NIC, and a virtio serial/console. The kernel's PCI scan discovers them
# and userspace drivers drive them. `disable-legacy=on` forces the modern virtio
# transport (MMIO BARs + PCI capabilities, device id 0x1040 + virtio type), which
# fits the userspace capability-driver model. The scratch disk is created once.
VIRTIO_DISK="$HERE/.build/virtio-blk.img"
mkdir -p "$HERE/.build"
[[ -f "$VIRTIO_DISK" ]] || truncate -s 16M "$VIRTIO_DISK"
QEMU_ARGS+=(
	-drive "file=$VIRTIO_DISK,if=none,id=vblk,format=raw"
	-device virtio-blk-pci,drive=vblk,disable-legacy=on
	-netdev user,id=vnet0
	-device virtio-net-pci,netdev=vnet0,disable-legacy=on
	-device virtio-serial-pci,disable-legacy=on
	-device virtconsole,chardev=vcon
	-chardev "file,id=vcon,path=$HERE/.build/virtio-console.out"
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
	QEMU_ARGS+=(-cpu qemu64,+rdrand -smp 4)
fi

if [[ "${DEBUG:-0}" == "1" ]]; then
	QEMU_ARGS+=(-s -S)
	echo "[qemu-run] waiting for GDB on :1234 (run 'just gdb' in another panel)"
fi

if [[ "${TEST:-0}" == "1" ]]; then
	# -no-reboot: a triple fault in a test exits QEMU instead of reboot-looping
	QEMU_ARGS+=(-no-reboot -device isa-debug-exit,iobase=0xf4,iosize=0x04)
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
