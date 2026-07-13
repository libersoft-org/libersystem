#!/usr/bin/env bash
# qemu-run.sh - cargo runner: builds the own UEFI loader, assembles a UEFI-only
# bootable ISO from the kernel ELF, and launches QEMU through OVMF.
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
QEMU_BOOT_DIR="$HERE"
QEMU_BUILD_DIR="$HERE/.build"
mkdir -p "$QEMU_BUILD_DIR"
# shellcheck source=qemu-common.sh
source "$HERE/qemu-common.sh"

# build the own UEFI loader (its EFI binary is staged into the boot image as
# BOOTX64.EFI); it lives in its own crate with its own UEFI target.
(cd "$HERE/../loader" && cargo build) >&2

# build the bootable ISO (mkimage.sh prints its path on stdout)
ISO="$("$HERE/mkimage.sh" iso "$KERNEL")"

# UEFI firmware (OVMF): the platform boots through UEFI, not SeaBIOS - the ISO is
# hybrid, and development deliberately exercises the UEFI path (the own UEFI-only
# bootloader is the target; see the concept's bootloader choice). The CODE image is
# read-only and shared; each run gets a private writable copy of the VARS store so
# concurrent instances (a test suite next to a live run) never fight over NVRAM.
# The script execs QEMU (no exit trap can clean up), so stale copies from earlier
# runs are unlinked here instead - a still-running instance keeps its copy alive
# through its open file descriptor.
OVMF_CODE="${OVMF_CODE:-/usr/share/OVMF/OVMF_CODE_4M.fd}"
OVMF_VARS_SRC="${OVMF_VARS_SRC:-/usr/share/OVMF/OVMF_VARS_4M.fd}"
[[ -f "$OVMF_CODE" && -f "$OVMF_VARS_SRC" ]] || {
	echo "OVMF firmware not found (install the 'ovmf' package)" >&2
	exit 1
}
mkdir -p "$HERE/.build"
rm -f "$HERE/.build/ovmf-vars."*.fd
OVMF_VARS="$(mktemp "$HERE/.build/ovmf-vars.XXXXXX.fd")"
cp "$OVMF_VARS_SRC" "$OVMF_VARS"

# build the QEMU arguments
QEMU_ARGS=(
	-machine q35
	-m 4G
	-drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE"
	-drive if=pflash,format=raw,file="$OVMF_VARS"
	-cdrom "$ISO"
	-boot d
	-serial "${SERIAL:-mon:stdio}"
)

# virtio devices for the userspace drivers: a scratch block disk, a
# user-mode NIC, and a virtio serial/console. The kernel's PCI scan discovers them
# and userspace drivers drive them. `disable-legacy=on` forces the modern virtio
# transport (MMIO BARs + PCI capabilities, device id 0x1040 + virtio type), which
# fits the userspace capability-driver model. The scratch disk is created once.
VIRTIO_DISK="$HERE/.build/virtio-blk.img"
VOLUME_PKG="$HERE/.build/volume.pkg"
# The system volume disk. It must hold the factory archive at LBA 0 (now a few megabytes
# of staged program binaries) followed by the LiberFS region, so it is sized
# well past both. A raw sparse image costs only the blocks actually written. Recreate it
# when missing, the wrong size (e.g. after a filesystem-geometry change), or OLDER than
# the freshly staged volume archive: overlaying the archive at LBA 0 is not enough,
# because a LiberFS formatted by an earlier boot leaves its backup GPT header at the END
# of the disk - StorageService then mounts the old filesystem (with the old staged
# binaries) instead of reseeding from the new archive. Recreating forces a clean
# reformat and reseed, so a rebuilt volume.pkg always reaches vol://system.
qemu_prepare_system_disk "$VOLUME_PKG" "$VIRTIO_DISK" || true
# A second virtio-blk disk holding a real exFAT volume (read and write): the media
# StorageService instance mounts it read-write as `vol://media`. Built once with mkfs.exfat
# (a genuine exFAT image, not a fixture) and seeded via a loopback mount; falls back to an
# mtools FAT32 image when mkfs.exfat / loop mount is unavailable, and is skipped entirely
# if neither toolchain is present. Files come from the volume/ seed dir.
qemu_prepare_media_images "" "" loop,ro=0 1
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
	# xHCI USB host controller: the kernel's PCI scan discovers it by class
	# (0x0C/0x03/0x30) and records its MMIO BAR in the device table; the userspace
	# xhci driver maps it and runs the USB stack. A hub hangs off port 1 with a USB
	# keyboard and a USB tablet behind it, so enumeration always exercises the hub
	# expansion path (port power, hub-request port reset, route strings) and the HID
	# report-descriptor path for both a keyboard and a pointing device. Attached on
	# the test path too, so the kernel tests can assert the controller is discovered
	# and its bus - hub included - enumerated.
	-device qemu-xhci,id=usb
	-device usb-hub,bus=usb.0,port=1
	-device usb-kbd,bus=usb.0,port=1.1
	-device usb-tablet,bus=usb.0,port=1.2
)

# A USB mass-storage stick on the xHCI bus: the xhci driver speaks SCSI over
# the Bulk-Only Transport to it and serves the same block-channel protocol as
# driver.virtio-blk, so a StorageService instance mounts it as vol://usb. The image
# always exists (a bare truncate suffices - the driver's bring-up needs no
# filesystem), so the test path's device set stays deterministic; when mtools is
# present it is seeded as FAT with the volume/ files so vol://usb mounts with
# content. Recreated only when missing. Skipped when a real stick is passed through
# (USB_HOST, interactive only), so that stick is the one storage device on the bus.
qemu_prepare_usb_image ""
if [[ "${TEST:-0}" == "1" || -z "${USB_HOST:-}" ]]; then
	QEMU_ARGS+=(
		-drive "file=$USB_DISK,if=none,id=vusb,format=raw"
		-device usb-storage,bus=usb.0,drive=vusb,id=usbstick
	)
fi
# The second virtio-blk disk (FAT vol://media), discovered after the system disk; only
# attached when the FAT image was built (mtools present).
if [[ -f "$FAT_DISK" ]]; then
	QEMU_ARGS+=(
		-drive "file=$FAT_DISK,if=none,id=vmedia,format=raw"
		-device virtio-blk-pci,drive=vmedia,disable-legacy=on
	)
fi
# The third virtio-blk disk (ISO9660 vol://iso), discovered after the media disk; only
# attached when the ISO image was built (xorriso / genisoimage present).
if [[ -f "$ISO_DISK" ]]; then
	QEMU_ARGS+=(
		-drive "file=$ISO_DISK,if=none,id=viso,format=raw"
		-device virtio-blk-pci,drive=viso,disable-legacy=on
	)
fi
# The fourth virtio-blk disk (UDF vol://udf), discovered after the iso disk; only
# attached when the UDF image was built (mkfs.udf present and loop mount succeeded).
if [[ -f "$UDF_DISK" ]]; then
	QEMU_ARGS+=(
		-drive "file=$UDF_DISK,if=none,id=vudf,format=raw"
		-device virtio-blk-pci,drive=vudf,disable-legacy=on
	)
fi

# display backends: the DISPLAYS env is a space-separated list, any of `vnc` and
# `spice` (both may be given at once; empty = headless, serial only). The
# framebuffer is always rendered and can also be screenshotted via screenshot.sh.
#   vnc    a VNC server; VNC_ADDR sets the bind/display, default 0.0.0.0:0 (port 5900)
#   spice  a SPICE server; SPICE_PORT sets the TCP port, default 5930
qemu_parse_displays qemu-run
QEMU_ARGS+=("${DISPLAY_ARGS[@]}")

# Match the guest's core count to the host's (overridable with SMP=<n>), so SMP
# runs exercise everything the machine has instead of a fixed number.
SMP="${SMP:-$(nproc)}"
if [[ "${NOKVM:-0}" != "1" && -e /dev/kvm ]]; then
	QEMU_ARGS+=(-enable-kvm -cpu host -smp "$SMP")
else
	QEMU_ARGS+=(-cpu qemu64,+rdrand,+smep,+smap -smp "$SMP")
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

# virtio-input keyboard: interactive runs only. The userspace virtio_input
# driver takes this device's interrupt and feeds key presses to the console shell,
# so typing in the SPICE/VNC window drives the system. Left out of the test path to
# keep that device set deterministic (the test boot exercises only blk/net/console).
QEMU_ARGS+=(-device virtio-keyboard-pci,disable-legacy=on)

# A real USB device passed through from the host onto the guest's xHCI bus
# (interactive runs only): USB_HOST=vendorid:productid (hex, as `lsusb` prints them,
# e.g. USB_HOST=0951:1666). The device detaches from the host for the run and the
# xhci driver enumerates it like the emulated ones - a real mass-storage stick
# replaces the emulated image (skipped above) and mounts as vol://usb, testing the
# BOT/SCSI path against genuine hardware. Needs access to the USB device node.
if [[ -n "${USB_HOST:-}" ]]; then
	QEMU_ARGS+=(-device "usb-host,bus=usb.0,vendorid=0x${USB_HOST%%:*},productid=0x${USB_HOST##*:}")
fi

# virtio-input tablet: interactive runs only. An absolute pointer device the
# same userspace virtio_input driver self-identifies and drives, delivering text-cell
# pointer/button events to InputService. A tablet (absolute coordinates) maps cleanly
# to screen cells; left out of the test path with the keyboard to keep that set
# deterministic (InputService's stream is proven by a kernel test instead).
QEMU_ARGS+=(-device virtio-tablet-pci,disable-legacy=on)

# virtio-vga: interactive runs only. A virtio-gpu device that also presents a
# VGA-compatible boot framebuffer, so the loader still renders the boot log while
# driver.virtio-gpu drives the display (a 2D scanout, and a resize event when the host
# window changes). It replaces the default std VGA (-vga none) here; the test path
# keeps std VGA (no virtio-gpu device, so ConsoleService falls back to the boot
# framebuffer and the deterministic 4-device set is unchanged).
QEMU_ARGS+=(-vga none -device virtio-vga)

# virtio-sound: interactive runs only. A virtio-sound device the userspace
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
QEMU_ARGS+=(-qmp "unix:$HERE/.build/qemu-qmp.sock,server,nowait")

exec qemu-system-x86_64 "${QEMU_ARGS[@]}"
