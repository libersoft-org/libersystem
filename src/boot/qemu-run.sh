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

qemu_append_audio() {
	local -n arr="$1"
	if [[ -n "${AUDIO_WAV:-}" ]]; then
		arr+=(-audiodev "wav,id=snd0,path=$AUDIO_WAV")
	elif [[ "$want_spice" == "1" ]]; then
		arr+=(-audiodev "spice,id=snd0")
	else
		arr+=(-audiodev "none,id=snd0")
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
qemu_prepare_system_disk() {
	local volume_pkg="$1"
	local disk="$2"
	local size=$((128 * 1024 * 1024))
	local package_exists=0
	[[ -f "$volume_pkg" ]] && package_exists=1

	if [[ ! -f "$disk" || "$(stat -c%s "$disk")" -ne "$size" || ($package_exists == 1 && "$volume_pkg" -nt "$disk") ]]; then
		rm -f "$disk"
		truncate -s "$size" "$disk"
	fi
	if [[ "$package_exists" == "1" ]]; then
		dd if="$volume_pkg" of="$disk" bs=512 conv=notrunc status=none
		return 0
	fi
	return 1
}

# Build the reusable exFAT/FAT, ISO9660 and UDF images. The caller owns QEMU attachment
# order and transport flags because those are part of each architecture's device model.
qemu_prepare_media_images() {
	local suffix="$1"
	local mount_suffix="$2"
	local udf_mount_options="${3:-loop}"
	local allow_fallbacks="${4:-0}"
	local voldir="$QEMU_BOOT_DIR/../volume"

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

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
QEMU_BOOT_DIR="$HERE"
QEMU_BUILD_DIR="$HERE/.build"
mkdir -p "$QEMU_BUILD_DIR"

if [[ -z "$KERNEL_ELF" ]]; then
	case "$TARGET_ARCH" in
	x86_64) KERNEL_ELF="$HERE/../kernel/target/x86_64-unknown-none/debug/kernel" ;;
	aarch64) KERNEL_ELF="$HERE/../kernel/target/aarch64-unknown-none/debug/kernel" ;;
	riscv64) KERNEL_ELF="$HERE/../kernel/target/riscv64gc-unknown-none-elf/debug/kernel" ;;
	esac
fi

[[ -f "$KERNEL_ELF" ]] || {
	echo "qemu-run: kernel ELF not found: $KERNEL_ELF" >&2
	exit 1
}

qemu_run_x86_64() {
	local kernel="$1"
	# Build the own UEFI loader (its EFI binary is staged into the boot image as
	# BOOTX64.EFI); it lives in its own crate with its own UEFI target.
	(cd "$HERE/../loader" && cargo build) >&2

	# Build the bootable ISO (mkimage.sh prints its path on stdout)
	local iso
	iso="$("$HERE/mkimage.sh" iso "$kernel")"

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
		-serial "${SERIAL:-mon:stdio}"
	)

	# System volume disk: holds the factory archive at LBA 0.
	local volume_pkg="$QEMU_BUILD_DIR/volume.pkg"
	local virtio_disk="$QEMU_BUILD_DIR/virtio-blk.img"
	qemu_prepare_system_disk "$volume_pkg" "$virtio_disk" || true
	qemu_attach_virtio_blk qemu_args "$virtio_disk" vblk "disable-legacy=on"

	# Media volumes: FAT/ISO/UDF images seeded from volume/ directory.
	qemu_prepare_media_images "" "" loop,ro=0 1

	# Network: user-mode NIC with optional hostfwd for interactive runs.
	local hostfwd=""
	[[ "${TEST:-0}" != "1" ]] && hostfwd="hostfwd=tcp:127.0.0.1:5555-:80"
	qemu_attach_virtio_net qemu_args vnet0 "$hostfwd" "disable-legacy=on"

	# virtio-serial + virtconsole: mirrors a second console to a file.
	qemu_args+=(
		-device virtio-serial-pci,disable-legacy=on
		-device virtconsole,chardev=vcon
		-chardev "file,id=vcon,path=$QEMU_BUILD_DIR/virtio-console.out"
	)

	# xHCI USB host controller + hub with keyboard, tablet, and optional storage.
	qemu_prepare_usb_image ""
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
	local smp="${SMP:-$(nproc)}"
	local cpu_args=()
	qemu_select_cpu cpu_args x86_64 qemu64,+rdrand,+smep,+smap
	qemu_args+=("${cpu_args[@]}" -smp "$smp")

	qemu_append_debug_args qemu_args

	if [[ "${TEST:-0}" == "1" ]]; then
		qemu_args+=(-no-reboot -device isa-debug-exit,iobase=0xf4,iosize=0x04)
		set +e
		qemu-system-x86_64 "${qemu_args[@]}"
		local code=$?
		set -e
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
	local qemu_args=()
	local cpu_args=()
	qemu_select_cpu cpu_args aarch64 cortex-a72

	# System volume disk: virtio-blk holding the factory archive.
	local volume_pkg="$QEMU_BUILD_DIR/volume-aarch64.pkg"
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
	else
		# Interactive-only devices: ramfb, virtio-keyboard/tablet, sound, virtconsole.
		qemu_attach_virt_interactive qemu_args -aarch64 "disable-legacy=on"
	fi
	qemu_append_debug_args qemu_args

	if [[ "$uefi" == "1" ]]; then
		# Boot through the own UEFI loader under AAVMF.
		local loader_efi="${LOADER_EFI:-$HERE/../loader/target/aarch64-unknown-uefi/debug/libersystem-loader.efi}"
		[[ -f "$loader_efi" ]] || {
			echo "qemu-run: loader EFI not found: $loader_efi (run 'just loader-aarch64')" >&2
			exit 1
		}
		[[ -f "$aavmf_code" && -f "$aavmf_vars" ]] || {
			echo "qemu-run: AAVMF firmware not found ($aavmf_code / $aavmf_vars)" >&2
			exit 1
		}
		qemu_build_esp aarch64 "$kernel" "$loader_efi" BOOTAA64.EFI
		local vars="$QEMU_BUILD_DIR/aavmf-vars.fd"
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
	local volume_pkg="$QEMU_BUILD_DIR/volume-riscv64.pkg"
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
	else
		# Interactive-only devices: ramfb, virtio-keyboard/tablet, sound, virtconsole.
		qemu_attach_virt_interactive qemu_args -riscv64 ""
	fi
	qemu_append_debug_args qemu_args

	if [[ "$uefi" == "1" ]]; then
		# Boot through the own UEFI loader under U-Boot.
		local loader_efi="${LOADER_EFI:-$HERE/../loader/target/riscv64gc-unknown-none-elf/debug/libersystem-loader.efi}"
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
