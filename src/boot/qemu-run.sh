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
#   QEMU_EXTRA= extra QEMU arguments
#   USB_HOST= vendorid:productid for USB passthrough (x86_64 interactive only)
#   UEFI=1    boot through own UEFI loader (aarch64/riscv64 only)
#   OVMF_*, AAVMF_*, BIOS=, UBOOT=, LOADER_EFI=, DTB_ADDR= arch-specific firmware

set -euo pipefail

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
# shellcheck source=qemu-common.sh
source "$HERE/qemu-common.sh"

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
	if [[ "$want_spice" == "1" ]]; then
		qemu_args+=(-audiodev "spice,id=snd0")
	else
		qemu_args+=(-audiodev "none,id=snd0")
	fi
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

	# Test mode: enable Arm semihosting, route serial to stdout.
	local test_args=()
	if [[ "${TEST:-0}" == "1" ]]; then
		test_args+=(-semihosting)
		serial="stdio"
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

	# Test mode: enable RISC-V semihosting, route serial to stdout.
	local test_args=()
	if [[ "${TEST:-0}" == "1" ]]; then
		test_args+=(-semihosting)
		serial="stdio"
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
