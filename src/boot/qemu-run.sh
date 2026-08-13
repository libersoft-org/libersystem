#!/usr/bin/env bash
# Unified QEMU runner for all architectures: x86_64, aarch64, riscv64.
#
# Usage: qemu-run.sh [x86_64|aarch64|riscv64] [kernel-elf]
# No arguments: detect native arch from uname -m and use default kernel path.
# First arg matching an arch selects it; optional second arg overrides kernel ELF.
# First arg not matching an arch is treated as kernel ELF (backward-compatible).
#
# Environment variables (preserved across all architectures):
#   DEBUG=1   QEMU waits for GDB (-s -S) on port :1234
#   NOKVM=1   disable KVM (more reliable single-stepping under TCG)
#   TEST=1    test mode (isa-debug-exit or semihosting, maps exit code to pass/fail)
#   DEV_PROFILE=1 development profile: names it over fw_cfg so the guest reports it and
#             DeviceManager starts a control agent, and attaches the channel the agent
#             answers on. On x86_64 that is the persistent instance `just dev-up` owns; on
#             the other targets it is a one-shot guest a scenario runner drives cold, which
#             is why this is not x86_64-only. Refused together with TEST.
#   SERIAL=   QEMU serial backend (default mon:stdio; e.g. file:boot.log or stdio)
#   SMP=N     override core/hart count (default: nproc, with arch-specific caps)
#   MEM=      override RAM (default varies by arch)
#   DISPLAYS= space-separated list of vnc and/or spice (empty = headless)
#   VNC_ADDR= VNC bind address (default 0.0.0.0:0)
#   SPICE_PORT= SPICE TCP port (default 5930)
#   AUDIO_WAV= capture virtio-sound output to this WAV file (overrides spice/none)
#   QEMU_EXTRA= extra QEMU arguments
#   USB_HOST= vendorid:productid for USB passthrough (x86_64 interactive only)
#   UEFI=1    boot through own UEFI loader (aarch64/riscv64 only)
#   EL2=1     aarch64: start the guest at EL2 (`virtualization=on`), which is where the UEFI
#             specification puts AArch64 firmware on most server-class parts - and which the
#             loader's EL2 branch had never once executed under, because QEMU's `virt` starts at
#             EL1 by default. Only meaningful with UEFI=1: the branch is in the loader.
#   OVMF_*, AAVMF_*, BIOS=, UBOOT=, LOADER_EFI=, DTB_ADDR= arch-specific firmware

set -euo pipefail

qemu_parse_displays() {
	local runner="$1"
	want_vnc=0
	want_spice=0
	local display
	for display in ${DISPLAYS:-}; do
		case "$display" in
		vnc) want_vnc=1 ;;
		spice) want_spice=1 ;;
		none | "") ;;
		*)
			echo "$runner: unknown display '$display' (expected vnc and/or spice)" >&2
			return 1
			;;
		esac
	done

	DISPLAY_ARGS=()
	if [[ "$want_vnc" == "1" ]]; then
		DISPLAY_ARGS+=(-vnc "${VNC_ADDR:-0.0.0.0:0}")
	else
		DISPLAY_ARGS+=(-display none)
	fi
	if [[ "$want_spice" == "1" ]]; then
		DISPLAY_ARGS+=(-spice "port=${SPICE_PORT:-5930},addr=0.0.0.0,disable-ticketing=on")
	fi
}

# The nameref is deliberately not called `arr`: this is called from helpers whose own array
# nameref is, and bash refuses a nameref that points at itself - it warns and appends nothing,
# which left the sound card on the command line with the audio backend it names missing, and
# QEMU refuses to start on that. An interactive aarch64 or riscv64 boot could not come up.
qemu_append_audio() {
	local -n audio_args="$1"
	if [[ -n "${AUDIO_WAV:-}" ]]; then
		audio_args+=(-audiodev "wav,id=snd0,path=$AUDIO_WAV")
	elif [[ "$want_spice" == "1" ]]; then
		audio_args+=(-audiodev "spice,id=snd0")
	else
		audio_args+=(-audiodev "none,id=snd0")
	fi
}

qemu_append_debug_args() {
	local -n arr=$1
	if [[ "${DEBUG:-0}" == "1" ]]; then
		arr+=(-s -S)
		echo "[qemu-run] waiting for GDB on :1234 (run 'just gdb' in another panel)"
	fi
}

# Recreate the system disk when the factory package is newer. Merely overlaying LBA 0
# is insufficient: an older LiberFS backup GPT header at the disk end would remount the
# stale filesystem and stale userspace binaries.
# Lay the system volume onto the disk QEMU attaches.
#
# This used to copy `volume.pkg` - a factory ARCHIVE - to LBA 0, and the storage service formatted
# a filesystem 32 MiB further in and seeded it from that archive on every boot. The volume is now
# built as a real LiberFS image (P02M0108), so the disk carries the volume itself: the same bytes the
# loader reads and the storage service mounts, in one place rather than two.
#
# The image is copied rather than attached directly because a guest writes to its system volume,
# and a test run must not edit the build output it was given.
qemu_prepare_system_disk() {
	local volume_image="$1"
	local disk="$2"
	[[ -f "$volume_image" ]] || return 1

	# Larger than the image so the volume has room to grow; the storage service reports a volume
	# that spans less than its container rather than silently resizing it.
	local size=$((128 * 1024 * 1024))
	if [[ ! -f "$disk" || "$(stat -c%s "$disk")" -ne "$size" || "$volume_image" -nt "$disk" ]]; then
		rm -f "$disk"
		truncate -s "$size" "$disk"
		dd if="$volume_image" of="$disk" bs=1M conv=notrunc status=none
	fi
	return 0
}

# Build the reusable exFAT/FAT, ISO9660 and UDF images. The caller owns QEMU attachment
# order and transport flags because those are part of each architecture's device model.
qemu_prepare_media_images() {
	local suffix="$1"
	local mount_suffix="$2"
	local udf_mount_options="${3:-loop}"
	local allow_fallbacks="${4:-0}"
	local voldir="$QEMU_BOOT_DIR/../volume"
	if [[ "${TEST:-0}" == "1" ]] && ! command -v mkfs.udf >/dev/null; then
		echo "qemu-run: mkfs.udf is required for the test UDF fixture (install udftools)" >&2
		exit 1
	fi

	FAT_DISK="$QEMU_BUILD_DIR/fat-media${suffix}.img"
	if [[ ! -f "$FAT_DISK" ]] && command -v mkfs.exfat >/dev/null; then
		truncate -s 16M "$FAT_DISK"
		if mkfs.exfat "$FAT_DISK" >/dev/null 2>&1; then
			local fmnt="$QEMU_BUILD_DIR/media-mnt${mount_suffix}"
			mkdir -p "$fmnt"
			if mount -o loop "$FAT_DISK" "$fmnt" 2>/dev/null; then
				cp "$voldir/hello.txt" "$voldir/motd.txt" "$fmnt"/ 2>/dev/null || true
				umount "$fmnt" 2>/dev/null || true
			fi
			rmdir "$fmnt" 2>/dev/null || true
		else
			rm -f "$FAT_DISK"
		fi
	fi
	if [[ "$allow_fallbacks" == "1" && ! -f "$FAT_DISK" ]] && command -v mformat >/dev/null && command -v mcopy >/dev/null; then
		truncate -s 16M "$FAT_DISK"
		mformat -i "$FAT_DISK" -F ::
		mcopy -i "$FAT_DISK" "$voldir/hello.txt" ::hello.txt
		mcopy -i "$FAT_DISK" "$voldir/motd.txt" ::motd.txt
	fi

	ISO_DISK="$QEMU_BUILD_DIR/iso-media${suffix}.iso"
	if [[ ! -f "$ISO_DISK" ]]; then
		if command -v xorriso >/dev/null; then
			xorriso -as mkisofs -quiet -J -R -o "$ISO_DISK" "$voldir" 2>/dev/null || true
		elif [[ "$allow_fallbacks" == "1" ]] && command -v genisoimage >/dev/null; then
			genisoimage -quiet -J -R -o "$ISO_DISK" "$voldir" 2>/dev/null || true
		fi
	fi

	UDF_DISK="$QEMU_BUILD_DIR/udf-media${suffix}.udf"
	if [[ ! -f "$UDF_DISK" ]] && command -v mkfs.udf >/dev/null; then
		dd if=/dev/zero of="$UDF_DISK" bs=1M count=8 status=none 2>/dev/null || true
		if mkfs.udf --media-type=hd --blocksize=2048 "$UDF_DISK" >/dev/null 2>&1; then
			local umnt="$QEMU_BUILD_DIR/udf-mnt${mount_suffix}"
			mkdir -p "$umnt"
			if mount -o "$udf_mount_options" "$UDF_DISK" "$umnt" 2>/dev/null; then
				cp "$voldir"/* "$umnt"/ 2>/dev/null || true
				umount "$umnt" 2>/dev/null || true
			fi
			rmdir "$umnt" 2>/dev/null || true
		else
			rm -f "$UDF_DISK"
		fi
	fi
}

qemu_prepare_usb_image() {
	local suffix="$1"
	local voldir="$QEMU_BOOT_DIR/../volume"
	USB_DISK="$QEMU_BUILD_DIR/usb-media${suffix}.img"
	if [[ -f "$USB_DISK" ]]; then
		return
	fi
	truncate -s 16M "$USB_DISK"
	if command -v mformat >/dev/null && command -v mcopy >/dev/null; then
		mformat -i "$USB_DISK" -F ::
		mcopy -i "$USB_DISK" "$voldir/hello.txt" ::hello.txt 2>/dev/null || true
		mcopy -i "$USB_DISK" "$voldir/motd.txt" ::motd.txt 2>/dev/null || true
	fi
}

qemu_attach_virtio_blk() {
	local -n arr=$1
	local file="$2"
	local drive_id="$3"
	local legacy="${4:-}"
	arr+=(-drive "file=$file,if=none,id=$drive_id,format=raw")
	if [[ -n "$legacy" ]]; then
		arr+=(-device "virtio-blk-pci,drive=$drive_id,$legacy")
	else
		arr+=(-device "virtio-blk-pci,drive=$drive_id")
	fi
}

qemu_attach_virtio_net() {
	local -n arr=$1
	local net_id="$2"
	local hostfwd="${3:-}"
	local legacy="${4:-}"
	local net_user="user,id=$net_id"
	[[ -n "$hostfwd" ]] && net_user="$net_user,$hostfwd"
	arr+=(-netdev "$net_user")
	if [[ -n "$legacy" ]]; then
		arr+=(-device "virtio-net-pci,netdev=$net_id,$legacy")
	else
		arr+=(-device "virtio-net-pci,netdev=$net_id")
	fi
}

qemu_attach_xhci() {
	local -n arr=$1
	local usb_drive_id="${2:-}"
	arr+=(
		-device "qemu-xhci,id=usb"
		-device "usb-hub,bus=usb.0,port=1"
		-device "usb-kbd,bus=usb.0,port=1.1"
		-device "usb-tablet,bus=usb.0,port=1.2"
	)
	if [[ -n "$usb_drive_id" ]]; then
		arr+=(-device "usb-storage,bus=usb.0,drive=$usb_drive_id,id=usbstick")
	fi
}

# The development channel: a SECOND single-port virtio-serial device, not a second port on
# the existing one. The guest driver already binds one console port per device and the
# device manager already binds a driver per device, so this needs no MULTIPORT negotiation,
# no control-queue port discovery and no change to the emergency console. Console traffic
# keeps its own device and stays usable if this one fails.
#
# The address is pinned, and high enough that auto-assignment never lands there, so the
# guest identifies the channel by PCI address rather than by enumeration order. Nothing
# above the port depends on that: the framing is carried by the port itself, so a later
# multiport driver can host the same protocol on a named port unchanged.
DEV_CHANNEL_PCI_SLOT="0x1e"

qemu_attach_dev_channel() {
	local -n arr=$1
	local socket_path="$2"
	local legacy="${3:-}"
	rm -f "$socket_path"
	# QEMU creates the socket under the ordinary umask, and this one carries a protocol that
	# publishes executable code into a running guest. Narrow every file QEMU creates from here
	# on to its owner: on a shared machine the alternative is a control channel anyone logged
	# in can speak to.
	umask 0077
	arr+=(-chardev "socket,id=devchan,path=$socket_path,server=on,wait=off")
	if [[ -n "$legacy" ]]; then
		arr+=(-device "virtio-serial-pci,id=devser,addr=$DEV_CHANNEL_PCI_SLOT,$legacy")
	else
		arr+=(-device "virtio-serial-pci,id=devser,addr=$DEV_CHANNEL_PCI_SLOT")
	fi
	# A console port, not a generic one. Without MULTIPORT there is no control queue to open
	# a generic port with, so a `virtserialport` here never opens and the guest's writes go
	# nowhere - measured, not assumed. The cost is that UEFI firmware writes its console
	# output to every console-class device it enumerates, so on the x86_64 UEFI path the
	# channel carries a firmware preamble before the guest owns the port. Guest console
	# traffic never appears on it, and framing above the port begins at the guest's first
	# byte, so the preamble is skipped rather than negotiated away with MULTIPORT.
	arr+=(-device "virtconsole,chardev=devchan,bus=devser.0")
}

qemu_attach_virt_interactive() {
	local -n arr=$1
	local suffix="$2"
	local legacy="${3:-}"
	local vcon_out="$QEMU_BUILD_DIR/virtio-console${suffix}.out"
	arr+=(-device "ramfb")
	if [[ -n "$legacy" ]]; then
		arr+=(
			-device "virtio-keyboard-pci,$legacy"
			-device "virtio-tablet-pci,$legacy"
			-device "virtio-serial-pci,$legacy"
		)
	else
		arr+=(
			-device "virtio-keyboard-pci"
			-device "virtio-tablet-pci"
			-device "virtio-serial-pci"
		)
	fi
	arr+=(
		-device "virtconsole,chardev=vcon"
		-chardev "file,id=vcon,path=$vcon_out"
	)
	qemu_append_audio arr
	arr+=(-device "virtio-sound-pci,audiodev=snd0")
}

qemu_build_esp() {
	local arch="$1"
	local kernel="$2"
	local loader_efi="$3"
	local boot_name="$4"
	ESP="$QEMU_BUILD_DIR/esp-${arch}.img"
	STAGED_KERNEL="$QEMU_BUILD_DIR/kernel-${arch}.stripped"
	llvm-strip --strip-debug -o "$STAGED_KERNEL" "$kernel" 2>/dev/null || cp "$kernel" "$STAGED_KERNEL"
	local esp_mb=$((($(stat -c%s "$STAGED_KERNEL") + $(stat -c%s "$loader_efi")) / 1048576 + 16))
	rm -f "$ESP"
	truncate -s "${esp_mb}M" "$ESP"
	mformat -i "$ESP" ::
	mmd -i "$ESP" ::/EFI ::/EFI/BOOT
	mcopy -i "$ESP" "$loader_efi" "::/EFI/BOOT/$boot_name"
	mcopy -i "$ESP" "$STAGED_KERNEL" ::/kernel
	# The boot packages, so the loader has something to hand over. Without them a UEFI boot comes
	# up with no userspace at all - the device-tree architectures used to get theirs from an
	# archive the runner laid in RAM, which the loader path does not use.
	#
	# The bootstrap set as FILES, the same shape the system volume carries and the same shape the
	# shipping media use. Architecture-qualified: every build writes the unqualified name, so
	# taking that verbatim put x86_64 programs on a riscv64 ESP and the boot died at "failed to
	# load SystemManager", one step after a loader hand-off that had gone perfectly.
	local bootstrap="$QEMU_BUILD_DIR/bootstrap-${arch}"
	if [[ -d "$bootstrap" ]]; then
		mmd -i "$ESP" ::/etc ::/libexec
		mcopy -i "$ESP" "$bootstrap/etc/bootstrap.list" ::/etc/bootstrap.list
		mcopy -i "$ESP" "$bootstrap"/libexec/* ::/libexec/
	fi
	# The factory archive still travels for the tests that read it as a fixture.
	local volume_pkg="$QEMU_BUILD_DIR/volume-${arch}.pkg"
	[[ -f "$volume_pkg" ]] && mcopy -i "$ESP" "$volume_pkg" ::/volume.pkg
}

normalize_arch() {
	case "$1" in
	x86_64) echo "x86_64" ;;
	aarch64 | arm64) echo "aarch64" ;;
	riscv64) echo "riscv64" ;;
	*)
		echo "qemu-run: unknown architecture '$1'" >&2
		return 1
		;;
	esac
}

detect_native_arch() {
	local host
	host="$(uname -m)"
	case "$host" in
	x86_64) echo "x86_64" ;;
	aarch64 | arm64) echo "aarch64" ;;
	riscv64) echo "riscv64" ;;
	*)
		echo "qemu-run: unsupported host architecture '$host'" >&2
		exit 1
		;;
	esac
}

qemu_select_cpu() {
	local -n args=$1
	local target="$2"
	local emulated_cpu="$3"
	local host
	case "$(uname -m)" in
	x86_64) host=x86_64 ;;
	aarch64 | arm64) host=aarch64 ;;
	riscv64) host=riscv64 ;;
	*) host=other ;;
	esac
	if [[ "${NOKVM:-0}" != "1" && "$target" == "$host" && -e /dev/kvm ]]; then
		args=(-enable-kvm -cpu host)
	else
		args=(-cpu "$emulated_cpu")
	fi
}

TARGET_ARCH=""
KERNEL_ELF=""

if [[ $# -eq 0 ]]; then
	TARGET_ARCH="$(detect_native_arch)"
elif [[ $# -eq 1 ]]; then
	if normalize_arch "$1" >/dev/null 2>&1; then
		TARGET_ARCH="$(normalize_arch "$1")"
	else
		TARGET_ARCH="$(detect_native_arch)"
		KERNEL_ELF="$1"
	fi
elif [[ $# -eq 2 ]]; then
	TARGET_ARCH="$(normalize_arch "$1")"
	KERNEL_ELF="$2"
else
	echo "usage: qemu-run.sh [x86_64|aarch64|riscv64] [kernel-elf]" >&2
	exit 1
fi

# Reject an unsupported development profile before any image work: the profile changes
# which host workflow owns the instance, so a request it cannot honour must fail loudly
# rather than boot an ordinary guest that merely looks like a development one.
if [[ "${DEV_PROFILE:-0}" == "1" && "${TEST:-0}" == "1" ]]; then
	echo "qemu-run: DEV_PROFILE and TEST are mutually exclusive" >&2
	exit 1
fi

# Where a development guest's control channel lives, by target. The persistent instance keeps
# the unsuffixed name it has always had, so nothing that owns one has to learn a new path; the
# other targets get their own so a one-shot run on one of them cannot be mistaken for it, or
# collide with it while it is up.
dev_channel_socket() {
	if [[ "$TARGET_ARCH" == "x86_64" ]]; then
		printf '%s/dev-channel.sock' "$QEMU_BUILD_DIR"
	else
		printf '%s/dev-channel-%s.sock' "$QEMU_BUILD_DIR" "$TARGET_ARCH"
	fi
}

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
QEMU_BOOT_DIR="$HERE"
QEMU_BUILD_DIR="$REPO_ROOT/.build/boot"
mkdir -p "$QEMU_BUILD_DIR"

timing_event() {
	if [[ -n "${LIBER_TIMING_LOG:-}" ]]; then printf '%s\t%s\t%s\n' "$(date +%s%N)" "$1" "$2" >>"$LIBER_TIMING_LOG"; fi
}

watch_test_timing() {
	local qemu_pid="$1"
	local serial_path="${SERIAL#file:}"
	local kernel_seen=0 tests_seen=0 complete_seen=0
	[[ "${SERIAL:-}" == file:* ]] || return 0
	while kill -0 "$qemu_pid" 2>/dev/null || [[ "$complete_seen" == 0 ]]; do
		if [[ "$kernel_seen" == 0 ]] && grep -q 'kernel is starting' "$serial_path" 2>/dev/null; then
			timing_event kernel start
			kernel_seen=1
		fi
		if [[ "$tests_seen" == 0 ]] && grep -q '^running [0-9].* tests' "$serial_path" 2>/dev/null; then
			timing_event scenario start
			tests_seen=1
		fi
		if [[ "$complete_seen" == 0 ]] && grep -q '^test suite complete:' "$serial_path" 2>/dev/null; then
			timing_event scenario end
			complete_seen=1
		fi
		if ! kill -0 "$qemu_pid" 2>/dev/null && [[ "$complete_seen" == 0 ]]; then break; fi
		sleep 0.01
	done
}

if [[ -z "$KERNEL_ELF" ]]; then
	case "$TARGET_ARCH" in
	x86_64) KERNEL_ELF="$REPO_ROOT/.build/cargo/kernel/x86_64-unknown-none/debug/kernel" ;;
	aarch64) KERNEL_ELF="$REPO_ROOT/.build/cargo/kernel/aarch64-unknown-none/debug/kernel" ;;
	riscv64) KERNEL_ELF="$REPO_ROOT/.build/cargo/kernel/riscv64gc-unknown-none-elf/debug/kernel" ;;
	esac
fi

[[ -f "$KERNEL_ELF" ]] || {
	echo "qemu-run: kernel ELF not found: $KERNEL_ELF" >&2
	exit 1
}

qemu_run_x86_64() {
	local kernel="$1"
	local artifact_suffix=""
	[[ "${TEST:-0}" == "1" ]] && artifact_suffix="-test"
	timing_event runner start
	timing_event image start
	# Build the own UEFI loader (its EFI binary is staged into the boot image as
	# BOOTX64.EFI); it lives in its own crate with its own UEFI target.
	(cd "$HERE/../loader" && cargo build) >&2

	# Build the bootable ISO (mkimage.sh prints its path on stdout).
	#
	# The suite boots the TEST medium: it reads `volume.pkg` off it as its fixture source and as
	# the table of expected file contents, which the shipping ISO deliberately no longer carries.
	# BOOT_IMAGE names a medium to boot INSTEAD of building one.
	#
	# Without it this function always built an image, so "boot the thing I just built" was not
	# expressible: the build, the imaging and the run were one step, and what ended up on the medium
	# could not be inspected between them. That is how a disk image came to carry the TEST kernel -
	# nothing sat between assembling it and booting it.
	local iso iso_mode="iso"
	if [[ -n "${BOOT_IMAGE:-}" ]]; then
		[[ -f "$BOOT_IMAGE" ]] || {
			echo "qemu-run: no image at $BOOT_IMAGE" >&2
			exit 1
		}
		iso="$BOOT_IMAGE"
		echo "qemu-run: booting $iso (built elsewhere; nothing was rebuilt)" >&2
	else
		[[ "${TEST:-0}" == "1" ]] && iso_mode="testiso"
		iso="$("$HERE/mkimage.sh" "$iso_mode" "$kernel")"
	fi

	# UEFI firmware (OVMF): the platform boots through UEFI, not SeaBIOS - the ISO is
	# hybrid, and development deliberately exercises the UEFI path (the own UEFI-only
	# bootloader is the target; see the concept's bootloader choice). The CODE image is
	# read-only and shared; each run gets a private writable copy of the VARS store so
	# concurrent instances (a test suite next to a live run) never fight over NVRAM.
	# The script execs QEMU (no exit trap can clean up), so stale copies from earlier
	# runs are unlinked here instead - a still-running instance keeps its copy alive
	# through its open file descriptor.
	local ovmf_code="${OVMF_CODE:-/usr/share/OVMF/OVMF_CODE_4M.fd}"
	local ovmf_vars_src="${OVMF_VARS_SRC:-/usr/share/OVMF/OVMF_VARS_4M.fd}"
	[[ -f "$ovmf_code" && -f "$ovmf_vars_src" ]] || {
		echo "qemu-run: OVMF firmware not found (install the 'ovmf' package)" >&2
		exit 1
	}
	rm -f "$QEMU_BUILD_DIR/ovmf-vars."*.fd
	local ovmf_vars
	ovmf_vars="$(mktemp "$QEMU_BUILD_DIR/ovmf-vars.XXXXXX.fd")"
	cp "$ovmf_vars_src" "$ovmf_vars"

	local qemu_args=(
		-machine q35
		-m "${MEM:-4G}"
		-drive "if=pflash,format=raw,readonly=on,file=$ovmf_code"
		-drive "if=pflash,format=raw,file=$ovmf_vars"
		-cdrom "$iso"
		-boot d
		-serial "${SERIAL:-stdio}"
	)

	# System volume disk: carries the LiberFS volume itself.
	local volume_image="$QEMU_BUILD_DIR/system-volume-x86_64.img"
	local virtio_disk="$QEMU_BUILD_DIR/virtio-blk${artifact_suffix}.img"
	qemu_prepare_system_disk "$volume_image" "$virtio_disk" || true
	qemu_attach_virtio_blk qemu_args "$virtio_disk" vblk "disable-legacy=on"

	# Media volumes: FAT/ISO/UDF images seeded from volume/ directory.
	qemu_prepare_media_images "$artifact_suffix" "$artifact_suffix" loop,ro=0 1

	# An ad-hoc guest cannot run beside the persistent development instance, and the reason is
	# not the port - it is the disks. Both attach the same raw images, QEMU takes a write lock
	# on each, and the second guest dies on whichever it reaches first: a forwarding rule it
	# cannot bind, or `Failed to get "write" lock` naming an image. Neither message mentions the
	# instance that actually holds them, so the reader debugs QEMU instead of running one
	# command. Two guests writing one image is also the corruption this milestone refuses
	# outright, so the answer is to refuse early rather than to hand out parallel disks.
	if [[ "${TEST:-0}" != "1" && "${DEV_PROFILE:-0}" != "1" && -e "$QEMU_BUILD_DIR/dev-instance.lock" ]] && ! flock -n "$QEMU_BUILD_DIR/dev-instance.lock" true 2>/dev/null; then
		echo "qemu-run: a development instance is running and holds the system, media and USB images" >&2
		echo "qemu-run: release it with \`just dev-down\` (or \`just dev-status\` to see what it is)" >&2
		exit 1
	fi

	# Network: user-mode NIC with optional hostfwd for interactive runs.
	#
	# The persistent development instance forwards a different port from an ordinary run,
	# because otherwise the two cannot coexist: the port was hard-coded, so `just run` while a
	# `dev-up` instance was alive died on
	#   Could not set up host forwarding rule 'tcp:127.0.0.1:5555-:80'
	# which names the port and not the reason. That is the same rule the instance already
	# follows for its serial, control and log paths - it owns its own names so an ad-hoc boot
	# cannot collide with it - and the port was the one thing left out.
	#
	# `HOSTFWD_PORT` overrides both, for a second ad-hoc guest or a host where 5555 is taken.
	local hostfwd=""
	if [[ "${TEST:-0}" != "1" ]]; then
		local default_port=5555
		[[ "${DEV_PROFILE:-0}" == "1" ]] && default_port=5556
		local port="${HOSTFWD_PORT:-$default_port}"
		# Fail on the cause rather than leaving QEMU to fail on a rule nobody can read.
		#
		# Capture before matching: `grep -q` stops at its first match and closes the pipe, so
		# `ss` takes SIGPIPE and `pipefail` makes that the status of the pipeline - a port
		# that IS in use could read as a failed check. The same shape the source-hygiene gate
		# refuses everywhere else, which is how it was found.
		local listeners
		listeners="$(ss -ltn "sport = :$port" 2>/dev/null || true)"
		if grep -q LISTEN <<<"$listeners"; then
			echo "qemu-run: host port $port is already in use, so this guest cannot forward it" >&2
			echo "qemu-run: a persistent development instance is the usual holder - check \`just dev-status\`, release it with \`just dev-down\`" >&2
			echo "qemu-run: or run this guest on another port with HOSTFWD_PORT=<port>" >&2
			exit 1
		fi
		hostfwd="hostfwd=tcp:127.0.0.1:$port-:80"
	fi
	qemu_attach_virtio_net qemu_args vnet0 "$hostfwd" "disable-legacy=on"

	# virtio-serial + virtconsole: mirrors a second console to a file.
	qemu_args+=(
		-device virtio-serial-pci,disable-legacy=on
		-device virtconsole,chardev=vcon
		-chardev "file,id=vcon,path=$QEMU_BUILD_DIR/virtio-console${artifact_suffix}.out"
	)

	# xHCI USB host controller + hub with keyboard, tablet, and optional storage.
	qemu_prepare_usb_image "$artifact_suffix"
	local usb_storage_id=""
	if [[ "${TEST:-0}" == "1" || -z "${USB_HOST:-}" ]]; then
		usb_storage_id="vusb"
		qemu_args+=(-drive "file=$USB_DISK,if=none,id=vusb,format=raw")
	fi
	qemu_attach_xhci qemu_args "$usb_storage_id"

	# Keep media disks after USB in PCI discovery order, matching the historical
	# runner and the volume/device inventory expected by the boot chain.
	[[ -f "$FAT_DISK" ]] && qemu_attach_virtio_blk qemu_args "$FAT_DISK" vmedia "disable-legacy=on"
	[[ -f "$ISO_DISK" ]] && qemu_attach_virtio_blk qemu_args "$ISO_DISK" viso "disable-legacy=on"
	[[ -f "$UDF_DISK" ]] && qemu_attach_virtio_blk qemu_args "$UDF_DISK" vudf "disable-legacy=on"

	# Display backends: parse DISPLAYS env for vnc/spice.
	qemu_parse_displays qemu-run
	qemu_args+=("${DISPLAY_ARGS[@]}")

	# CPU and SMP: KVM for a matching host, otherwise the emulated x86 model.
	local smp
	if [[ "${TEST:-0}" == "1" ]]; then smp="${SMP:-4}"; else smp="${SMP:-$(nproc)}"; fi
	local cpu_args=()
	qemu_select_cpu cpu_args x86_64 qemu64,+rdrand,+smep,+smap
	qemu_args+=("${cpu_args[@]}" -smp "$smp")

	qemu_append_debug_args qemu_args
	timing_event image end

	if [[ "${TEST:-0}" == "1" ]]; then
		# The development channel is present in the cold test configuration too: the same
		# second port on every target is what lets a scenario runner drive a boot over
		# identical framing, including where the persistent profile does not exist.
		qemu_attach_dev_channel qemu_args "$QEMU_BUILD_DIR/dev-channel-x86_64-test.sock" "disable-legacy=on"
		# A BRIDGE WITH SOMETHING BEHIND IT, so the PCI walk has a second bus to find. The x86
		# enumeration followed no bridges and the q35 default topology puts everything on bus 0,
		# so recursive enumeration could be written and never executed - it is the topology, not
		# the code, that decided the test passed. `pci-testdev` is inert: nothing in this kernel
		# binds it, so what it proves is exactly that the walk reached a bus firmware did not
		# place on the root.
		qemu_args+=(
			-device "pcie-pci-bridge,id=liberbr,bus=pcie.0,addr=0x1c"
			-device "pci-testdev,bus=liberbr,addr=0x1"
		)
		qemu_args+=(-no-reboot -device isa-debug-exit,iobase=0xf4,iosize=0x04)
		timing_event qemu start
		set +e
		# `QEMU_EXTRA` reaches test mode too. It is documented at the top of this file as extra
		# QEMU arguments and did not apply here, which is the one configuration where it is
		# most wanted: diagnosing a guest that resets needs `-d int,cpu_reset` on the run that
		# reproduces it, and a test run is what reproduces it.
		if [[ -n "${LIBER_TIMING_LOG:-}" ]]; then
			qemu-system-x86_64 "${qemu_args[@]}" ${QEMU_EXTRA:-} &
			local qemu_pid=$!
			watch_test_timing "$qemu_pid" &
			local watcher_pid=$!
			wait "$qemu_pid"
			local code=$?
			wait "$watcher_pid" || true
		else
			qemu-system-x86_64 "${qemu_args[@]}" ${QEMU_EXTRA:-}
			local code=$?
		fi
		set -e
		timing_event qemu end
		# 33 is the debug-exit device reporting a passing suite (the guest wrote 0x10 to
		# port 0xf4, and QEMU reports (0x10<<1)|1); 35 is the same device reporting failure.
		# Anything else is QEMU ending for a reason the guest never asked for. Mapping 33 to 0
		# here without saying which of the two happened is what made a broken run read as a
		# clean one: the caller sees 0 for a passing suite AND for a QEMU that quit on its own.
		echo "qemu-run: test guest ended with QEMU code $code (33 = suite passed, 35 = suite failed)" >&2
		[[ "$code" -eq 33 ]] && exit 0
		exit "$code"
	fi

	# Interactive-only devices: virtio-input keyboard/tablet, virtio-vga, virtio-sound.
	qemu_args+=(-device virtio-keyboard-pci,disable-legacy=on)
	qemu_args+=(-device virtio-tablet-pci,disable-legacy=on)
	qemu_args+=(-vga none -device virtio-vga)
	qemu_append_audio qemu_args
	qemu_args+=(-device virtio-sound-pci,audiodev=snd0)

	# USB passthrough: real USB device (interactive only).
	if [[ -n "${USB_HOST:-}" ]]; then
		qemu_args+=(-device "usb-host,bus=usb.0,vendorid=0x${USB_HOST%%:*},productid=0x${USB_HOST##*:}")
	fi

	# Development profile: name it over fw_cfg, which the guest reads at boot and prints.
	# This sits below the test early-exit above, so test mode cannot reach it by
	# construction, and it adds no device and rewrites no image, so the profile changes
	# nothing a normal or production boot is built from.
	if [[ "${DEV_PROFILE:-0}" == "1" ]]; then
		qemu_args+=(-fw_cfg "name=opt/org.libersystem/profile,string=development")
		qemu_attach_dev_channel qemu_args "$(dev_channel_socket)" "disable-legacy=on"
	fi

	# Interactive control sockets used by screenshot.sh and lab.py.
	local monitor_socket="$QEMU_BUILD_DIR/qemu-monitor.sock"
	local qmp_socket="$QEMU_BUILD_DIR/qemu-qmp.sock"
	rm -f "$monitor_socket" "$qmp_socket"
	qemu_args+=(-monitor "unix:$monitor_socket,server,nowait")
	qemu_args+=(-qmp "unix:$qmp_socket,server,nowait")

	exec qemu-system-x86_64 "${qemu_args[@]}" ${QEMU_EXTRA:-}
}

qemu_run_aarch64() {
	local kernel="$1"
	local serial="${SERIAL:-mon:stdio}"
	local smp="${SMP:-$(nproc | awk '{print ($1 > 8) ? 8 : $1}')}"
	local mem="${MEM:-512M}"
	local dtb_addr="${DTB_ADDR:-0x4A000000}"
	local uefi="${UEFI:-0}"
	local aavmf_code="${AAVMF_CODE:-/usr/share/AAVMF/AAVMF_CODE.fd}"
	local aavmf_vars="${AAVMF_VARS:-/usr/share/AAVMF/AAVMF_VARS.fd}"

	qemu_parse_displays qemu-run

	local machine="virt,gic-version=2"
	# EL2, the level the specification says AArch64 firmware runs at. The loader reads `CurrentEL`
	# and drops to EL1 before entering the kernel; that path is written from the architecture manual
	# and, until this switch existed, had never been executed on any machine this project has run on.
	if [[ "${EL2:-0}" == "1" ]]; then
		machine="$machine,virtualization=on"
	fi
	local qemu_args=()
	local cpu_args=()
	qemu_select_cpu cpu_args aarch64 cortex-a72

	# System volume disk: virtio-blk holding the factory archive.
	local volume_pkg="$QEMU_BUILD_DIR/system-volume-aarch64.img"
	local virtio_disk="$QEMU_BUILD_DIR/virtio-blk-aarch64.img"
	if qemu_prepare_system_disk "$volume_pkg" "$virtio_disk"; then
		qemu_attach_virtio_blk qemu_args "$virtio_disk" vol0 "disable-legacy=on"
	fi

	# Media volumes: FAT/ISO/UDF images seeded from volume/ directory.
	qemu_prepare_media_images -aarch64 -a64
	[[ -f "$FAT_DISK" ]] && qemu_attach_virtio_blk qemu_args "$FAT_DISK" med0 "disable-legacy=on"
	[[ -f "$ISO_DISK" ]] && qemu_attach_virtio_blk qemu_args "$ISO_DISK" iso0 "disable-legacy=on"
	[[ -f "$UDF_DISK" ]] && qemu_attach_virtio_blk qemu_args "$UDF_DISK" udf0 "disable-legacy=on"

	# Network: user-mode virtio-net.
	qemu_attach_virtio_net qemu_args vnet0 "" "disable-legacy=on"

	# xHCI USB host controller + hub with keyboard, tablet, and storage.
	qemu_prepare_usb_image -aarch64
	qemu_args+=(-drive "if=none,id=vusb,format=raw,file=$USB_DISK")
	qemu_attach_xhci qemu_args vusb

	# Test mode: enable Arm semihosting while retaining the selected serial backend.
	local test_args=()
	if [[ "${TEST:-0}" == "1" ]]; then
		test_args+=(-semihosting)
		# The boot-chain test includes DisplayService and its Console/Shell dependents.
		# Unlike x86, the virt machine has no default VGA device, so test mode supplies
		# the same discoverable GPU path without enabling the interactive peripherals.
		qemu_args+=(-device virtio-gpu-pci,disable-legacy=on)
		qemu_attach_dev_channel qemu_args "$QEMU_BUILD_DIR/dev-channel-aarch64-test.sock" "disable-legacy=on"
	else
		# Interactive-only devices: ramfb, virtio-keyboard/tablet, sound, virtconsole.
		qemu_attach_virt_interactive qemu_args -aarch64 "disable-legacy=on"
		# The development profile is not x86_64's alone: a scenario has to be runnable against
		# a cold boot of every target, and what that needs is a guest that names the profile
		# (so DeviceManager starts an agent) and a channel for the agent to answer on.
		if [[ "${DEV_PROFILE:-0}" == "1" ]]; then
			qemu_args+=(-fw_cfg "name=opt/org.libersystem/profile,string=development")
			qemu_attach_dev_channel qemu_args "$(dev_channel_socket)" "disable-legacy=on"
			# The same discoverable GPU the test configuration supplies, and for the same
			# reason: the virt machine has no VGA device, the interactive set offers ramfb
			# instead, and nothing drives ramfb - so DisplayService never comes up and takes
			# ConsoleService and the shell down with it. A driven guest needs all three.
			qemu_args+=(-device "virtio-gpu-pci,disable-legacy=on")
			# The monitor and QMP sockets a driven guest needs: `key` and `pointer` steps go through
			# QMP, which is how a scenario reaches the emulated keyboard and tablet rather than the
			# console. Per target, so a one-shot run cannot be mistaken for the persistent instance's
			# or collide with it. Without these a `key` step reaches nothing and quietly does nothing.
			local dev_monitor="$QEMU_BUILD_DIR/qemu-monitor-$TARGET_ARCH.sock"
			local dev_qmp="$QEMU_BUILD_DIR/qemu-qmp-$TARGET_ARCH.sock"
			rm -f "$dev_monitor" "$dev_qmp"
			qemu_args+=(-monitor "unix:$dev_monitor,server,nowait")
			qemu_args+=(-qmp "unix:$dev_qmp,server,nowait")
		fi
	fi
	qemu_append_debug_args qemu_args

	if [[ "$uefi" == "1" ]]; then
		# Boot through the own UEFI loader under AAVMF.
		local loader_efi="${LOADER_EFI:-$REPO_ROOT/.build/cargo/loader/aarch64-unknown-uefi/debug/libersystem-loader.efi}"
		[[ -f "$loader_efi" ]] || {
			echo "qemu-run: loader EFI not found: $loader_efi (run 'just loader-aarch64')" >&2
			exit 1
		}
		[[ -f "$aavmf_code" && -f "$aavmf_vars" ]] || {
			echo "qemu-run: AAVMF firmware not found ($aavmf_code / $aavmf_vars)" >&2
			exit 1
		}
		qemu_build_esp aarch64 "$kernel" "$loader_efi" BOOTAA64.EFI
		# A private copy per run, like the OVMF path above. One shared file means two aarch64 runs
		# write each other's firmware variables, and the script `exec`s QEMU so no trap can clean up
		# afterwards - stale copies from earlier runs are unlinked here instead, while a still-
		# running instance keeps its own alive through its open descriptor.
		rm -f "$QEMU_BUILD_DIR/aavmf-vars."*.fd
		local vars
		vars="$(mktemp "$QEMU_BUILD_DIR/aavmf-vars.XXXXXX.fd")"
		cp "$aavmf_vars" "$vars"
		# ESP goes last so system volume enumerates ahead of it.
		qemu_attach_virtio_blk qemu_args "$ESP" esp "disable-legacy=on"
		exec qemu-system-aarch64 \
			-machine "$machine" \
			"${cpu_args[@]}" \
			-smp "$smp" \
			-m "$mem" \
			-drive "if=pflash,format=raw,file=$aavmf_code,readonly=on" \
			-drive "if=pflash,format=raw,file=$vars" \
			-serial "$serial" \
			"${DISPLAY_ARGS[@]}" \
			-no-reboot \
			"${test_args[@]}" \
			"${qemu_args[@]}" \
			${QEMU_EXTRA:-}
	fi

	# The boot modules, handed over as an initrd. This machine has no bootloader module
	# hand-off, so the kernel receives the archive here and finds its range in the device tree -
	# which is why the DTB below is dumped WITH `-initrd` and `-kernel` present: dumping it
	# without them yields a tree whose /chosen carries no initrd range, and the kernel would
	# come up with no userspace at all.

	# Direct -kernel boot: dump DTB and load it at DTB_ADDR.
	local dtb_file
	dtb_file="$(mktemp /tmp/qemu-virt-XXXXXX.dtb)"
	trap 'rm -f "$dtb_file"' EXIT
	qemu-system-aarch64 \
		-machine "$machine,dumpdtb=$dtb_file" \
		"${cpu_args[@]}" \
		-smp "$smp" \
		-m "$mem" \
		-display none >/dev/null 2>&1

	exec qemu-system-aarch64 \
		-machine "$machine" \
		"${cpu_args[@]}" \
		-smp "$smp" \
		-m "$mem" \
		-kernel "$kernel" \
		-device "loader,file=$dtb_file,addr=$dtb_addr" \
		-serial "$serial" \
		"${DISPLAY_ARGS[@]}" \
		-no-reboot \
		"${test_args[@]}" \
		"${qemu_args[@]}" \
		${QEMU_EXTRA:-}
}

qemu_run_riscv64() {
	local kernel="$1"
	local serial="${SERIAL:-mon:stdio}"
	local smp="${SMP:-$(nproc)}"
	local mem="${MEM:-512M}"
	local bios="${BIOS:-default}"
	local uefi="${UEFI:-0}"
	local uboot="${UBOOT:-/usr/lib/u-boot/qemu-riscv64_smode/u-boot.bin}"

	qemu_parse_displays qemu-run

	local qemu_args=()
	local cpu_args=()
	qemu_select_cpu cpu_args riscv64 rv64

	# System volume disk: virtio-blk holding the factory archive.
	local volume_pkg="$QEMU_BUILD_DIR/system-volume-riscv64.img"
	local virtio_disk="$QEMU_BUILD_DIR/virtio-blk-riscv64.img"
	if qemu_prepare_system_disk "$volume_pkg" "$virtio_disk"; then
		qemu_attach_virtio_blk qemu_args "$virtio_disk" vol0 ""
	fi

	# Media volumes: FAT/ISO/UDF images seeded from volume/ directory.
	qemu_prepare_media_images -riscv64 -rv64
	[[ -f "$FAT_DISK" ]] && qemu_attach_virtio_blk qemu_args "$FAT_DISK" med0 ""
	[[ -f "$ISO_DISK" ]] && qemu_attach_virtio_blk qemu_args "$ISO_DISK" iso0 ""
	[[ -f "$UDF_DISK" ]] && qemu_attach_virtio_blk qemu_args "$UDF_DISK" udf0 ""

	# Network: user-mode virtio-net (no disable-legacy for riscv64).
	qemu_attach_virtio_net qemu_args vnet0 "" ""

	# xHCI USB host controller + hub with keyboard, tablet, and storage.
	qemu_prepare_usb_image -riscv64
	qemu_args+=(-drive "if=none,id=vusb,format=raw,file=$USB_DISK")
	qemu_attach_xhci qemu_args vusb

	# Test mode: enable RISC-V semihosting while retaining the selected serial backend.
	local test_args=()
	if [[ "${TEST:-0}" == "1" ]]; then
		test_args+=(-semihosting)
		# The RISC-V virt machine has no default VGA device, while the boot-chain test
		# requires DisplayService and its Console/Shell dependents.
		qemu_args+=(-device virtio-gpu-pci)
		qemu_attach_dev_channel qemu_args "$QEMU_BUILD_DIR/dev-channel-riscv64-test.sock" ""
	else
		# Interactive-only devices: ramfb, virtio-keyboard/tablet, sound, virtconsole.
		qemu_attach_virt_interactive qemu_args -riscv64 ""
		if [[ "${DEV_PROFILE:-0}" == "1" ]]; then
			qemu_args+=(-fw_cfg "name=opt/org.libersystem/profile,string=development")
			qemu_attach_dev_channel qemu_args "$(dev_channel_socket)" ""
			# The same discoverable GPU the test configuration supplies, and for the same
			# reason: the virt machine has no VGA device, the interactive set offers ramfb
			# instead, and nothing drives ramfb - so DisplayService never comes up and takes
			# ConsoleService and the shell down with it. A driven guest needs all three.
			qemu_args+=(-device "virtio-gpu-pci")
			# The monitor and QMP sockets a driven guest needs: `key` and `pointer` steps go through
			# QMP, which is how a scenario reaches the emulated keyboard and tablet rather than the
			# console. Per target, so a one-shot run cannot be mistaken for the persistent instance's
			# or collide with it. Without these a `key` step reaches nothing and quietly does nothing.
			local dev_monitor="$QEMU_BUILD_DIR/qemu-monitor-$TARGET_ARCH.sock"
			local dev_qmp="$QEMU_BUILD_DIR/qemu-qmp-$TARGET_ARCH.sock"
			rm -f "$dev_monitor" "$dev_qmp"
			qemu_args+=(-monitor "unix:$dev_monitor,server,nowait")
			qemu_args+=(-qmp "unix:$dev_qmp,server,nowait")
		fi
	fi
	qemu_append_debug_args qemu_args

	if [[ "$uefi" == "1" ]]; then
		# Boot through the own UEFI loader under U-Boot.
		#
		# U-Boot is clamped to at most 8 harts, and this is not tidiness - it is the whole reason
		# this path "produced no output at all". `-smp` defaults to `nproc`, and on a host with
		# enough cores that number is passed straight through: measured 2026-08-03, this U-Boot
		# build prints its banner at 50 harts and prints NOTHING at 51, so a 52-core host got a
		# silent failure that looked like the loader or the hand-rolled EFI image was at fault.
		# OpenSBI still ran and still logged, which is what made it read as a boot that got
		# further than it did.
		#
		# The clamp is announced rather than silent: overriding an explicit SMP= without saying so
		# is how a measurement gets attributed to the wrong core count.
		if ((smp > 8)); then
			echo "qemu-run: riscv64 UEFI: capping -smp $smp to 8 (U-Boot stops booting above ~50 harts)" >&2
			smp=8
		fi
		local loader_efi="${LOADER_EFI:-$REPO_ROOT/.build/cargo/loader/riscv64gc-unknown-none-elf/debug/libersystem-loader.efi}"
		[[ -f "$loader_efi" ]] || {
			echo "qemu-run: loader EFI not found: $loader_efi (run 'just loader-riscv64')" >&2
			exit 1
		}
		[[ -f "$uboot" ]] || {
			echo "qemu-run: U-Boot not found: $uboot (install the u-boot-qemu package)" >&2
			exit 1
		}
		qemu_build_esp riscv64 "$kernel" "$loader_efi" BOOTRISCV64.EFI
		# ESP is NVMe so U-Boot's default boot order tries nvme0 first.
		qemu_args+=(-drive "if=none,id=esp,format=raw,file=$ESP" -device "nvme,serial=libersystem-esp,drive=esp")
		exec qemu-system-riscv64 \
			-machine "virt,aia=aplic-imsic" \
			"${cpu_args[@]}" \
			-smp "$smp" \
			-m "$mem" \
			-bios "$bios" \
			-kernel "$uboot" \
			-serial "$serial" \
			"${DISPLAY_ARGS[@]}" \
			-no-reboot \
			"${qemu_args[@]}" \
			"${test_args[@]}" \
			${QEMU_EXTRA:-}
	fi

	# Direct -kernel boot: OpenSBI jumps to kernel entry.
	exec qemu-system-riscv64 \
		-machine "virt,aia=aplic-imsic" \
		"${cpu_args[@]}" \
		-smp "$smp" \
		-m "$mem" \
		-bios "$bios" \
		-kernel "$kernel" \
		-serial "$serial" \
		"${DISPLAY_ARGS[@]}" \
		-no-reboot \
		"${qemu_args[@]}" \
		"${test_args[@]}" \
		${QEMU_EXTRA:-}
}

case "$TARGET_ARCH" in
x86_64) qemu_run_x86_64 "$KERNEL_ELF" ;;
aarch64) qemu_run_aarch64 "$KERNEL_ELF" ;;
riscv64) qemu_run_riscv64 "$KERNEL_ELF" ;;
esac
