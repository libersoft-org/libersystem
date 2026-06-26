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
	-m 4G
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
VOLUME_PKG="$HERE/.build/volume.pkg"
mkdir -p "$HERE/.build"
[[ -f "$VIRTIO_DISK" ]] || truncate -s 16M "$VIRTIO_DISK"
# StorageService (M26) backs its `vol://system` volume with this block device, so
# lay the packed volume archive down at LBA 0 on every boot (conv=notrunc keeps the
# disk at its full size; the kernel's build.rs produces volume.pkg next to it).
if [[ -f "$VOLUME_PKG" ]]; then
	dd if="$VOLUME_PKG" of="$VIRTIO_DISK" bs=512 conv=notrunc status=none
fi
# Forward host 127.0.0.1:5555 to the guest's port 80, so a host HTTP client can reach
# the guest's httpd (passive open / inbound) - SLIRP gives no inbound route otherwise.
# Interactive runs only: the test path keeps a fixed device set and binds no host port.
NET_USER="user,id=vnet0"
if [[ "${TEST:-0}" != "1" ]]; then
	NET_USER="$NET_USER,hostfwd=tcp:127.0.0.1:5555-:80"
fi
QEMU_ARGS+=(
	-drive "file=$VIRTIO_DISK,if=none,id=vblk,format=raw"
	-device virtio-blk-pci,drive=vblk,disable-legacy=on
	-netdev "$NET_USER"
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
	QEMU_ARGS+=(-enable-kvm -cpu host -smp 8)
else
	QEMU_ARGS+=(-cpu qemu64,+rdrand -smp 8)
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

# virtio-input keyboard (M31): interactive runs only. The userspace virtio_input
# driver takes this device's interrupt and feeds key presses to the console shell,
# so typing in the SPICE/VNC window drives the system. Left out of the test path to
# keep that device set deterministic (the test boot exercises only blk/net/console).
QEMU_ARGS+=(-device virtio-keyboard-pci,disable-legacy=on)

# virtio-input tablet (M36): interactive runs only. An absolute pointer device the
# same userspace virtio_input driver self-identifies and drives, delivering text-cell
# pointer/button events to InputService. A tablet (absolute coordinates) maps cleanly
# to screen cells; left out of the test path with the keyboard to keep that set
# deterministic (InputService's stream is proven by a kernel test instead).
QEMU_ARGS+=(-device virtio-tablet-pci,disable-legacy=on)

# virtio-vga (M44): interactive runs only. A virtio-gpu device that also presents a
# VGA-compatible boot framebuffer, so Limine still renders the boot log while
# driver.virtio-gpu drives the display (a 2D scanout, and a resize event when the host
# window changes). It replaces the default std VGA (-vga none) here; the test path
# keeps std VGA (no virtio-gpu device, so ConsoleService falls back to the Limine
# framebuffer and the deterministic 4-device set is unchanged).
QEMU_ARGS+=(-vga none -device virtio-vga)

# virtio-sound (M45): interactive runs only. A virtio-sound device the userspace
# driver.virtio-snd drives for PCM playback (the shell `beep` command, via
# AudioService). Its host audio backend is the SPICE server when a SPICE display is
# requested (so a connected SPICE client hears it), else a null sink (the guest still
# plays, nothing is emitted). Left out of the test path to keep that device set
# deterministic (the test boot exercises only blk/net/console).
if [[ "$want_spice" == "1" ]]; then
	QEMU_ARGS+=(-audiodev spice,id=snd0)
else
	QEMU_ARGS+=(-audiodev none,id=snd0)
fi
QEMU_ARGS+=(-device virtio-sound-pci,audiodev=snd0)

# Expose a control monitor on a unix socket (alongside the stdio monitor) so
# boot/screenshot.sh can attach to this running instance and snap the live
# framebuffer at any time. Only for interactive runs (not the test path above).
MON_SOCK="$HERE/.build/qemu-monitor.sock"
mkdir -p "$HERE/.build"
rm -f "$MON_SOCK"
QEMU_ARGS+=(-monitor "unix:$MON_SOCK,server,nowait")

exec qemu-system-x86_64 "${QEMU_ARGS[@]}"
