#!/usr/bin/env bash
# Build and boot the system in QEMU.
#
# One command instead of five (`run`, `run-x86_64`, `run-aarch64`, `run-riscv64`, and the two
# `-uefi` variants that are gone): the architecture and the displays are what vary, so they are
# flags.

SCRIPT_NAME=run.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

DISPLAYS_ALL="vnc spice"

help() {
	usage_and_exit <<EOF
usage: run.sh [--arch ARCH] [--image PATH] [--attach PATH[,PATH...]] [--display D[,D...]] [--debug]

Boots the system in QEMU, headless, with the serial console on your terminal.

  --arch ARCH     x86_64 | aarch64 | riscv64   (default: the host's architecture)
  --image PATH    boot THIS medium and build nothing (from ./image.sh)
  --attach PATH   attach an extra disk or CD image, repeatable or comma-separated
  --display D     vnc | spice | all - attach a live display server (they combine)
  --smp N         cores given to the guest (default: the host's, capped at 8 on aarch64/riscv64)
  --mem SIZE      guest memory, QEMU-style: 512M, 4G
  --serial SPEC   QEMU serial backend, e.g. --serial file:boot.log
  --vnc-addr A    VNC bind address and display (default 0.0.0.0:0 - EVERY interface, port 5900, and
                  with no password and no TLS: use 127.0.0.1:0 on a network you do not trust)
  --spice-addr A  SPICE bind address (default 0.0.0.0 - EVERY interface, and with no password and
                  no TLS: use 127.0.0.1 on a network you do not trust)
  --spice-port P  SPICE port (default 5930)
  --debug         wait for GDB on :1234, no KVM
  --gdb           ATTACH gdb to a guest already waiting - run in a second panel after --debug,
                  and it boots nothing itself
  -h, --help      this text

three steps, not one:

  Without --image this builds the system, assembles a medium and boots it, which is convenient and
  is also how a disk image came to carry the wrong kernel: nothing sits between assembling and
  booting to look at what went on. The steps are separable, and for anything you intend to keep or
  inspect they should be separate:

    ./build.sh                          # 1. compile
    ./image.sh --format iso             # 2. assemble a medium
    ./run.sh --image .build/boot/libersystem.iso   # 3. boot exactly that

All three architectures boot the same way a real machine does: firmware runs the system's own
loader, which reads the kernel and the bootstrap programs off the system volume. aarch64 and
riscv64 are emulated on an x86_64 host, so they are slower than the native run.

examples:
  ./run.sh
  ./run.sh --arch riscv64
  ./run.sh --display vnc,spice
EOF
}

arch=""
attach_gdb=0
displays=()
debug=0
image=""
attach=()

while [[ $# -gt 0 ]]; do
	case "$1" in
	-h | --help) help ;;
	--arch)
		[[ $# -ge 2 ]] || die "--arch needs a value"
		picked_raw="$(parse_list "$2" architecture "${ARCHS_ALL[*]}")"
		mapfile -t picked <<<"$picked_raw"
		[[ ${#picked[@]} -eq 1 ]] || die "--arch takes one architecture here; a run boots one machine"
		arch="${picked[0]}"
		shift 2
		;;
	--display)
		[[ $# -ge 2 ]] || die "--display needs a value"
		picked_raw="$(parse_list "$2" display "$DISPLAYS_ALL")"
		mapfile -t picked <<<"$picked_raw"
		displays+=("${picked[@]}")
		shift 2
		;;
	--image)
		[[ $# -ge 2 ]] || die "--image needs a path"
		[[ -f "$2" ]] || die "no image at '$2'"
		# Absolute: qemu-run.sh runs from src/, so a path relative to where you typed it would
		# resolve somewhere else entirely - and the failure would read as a missing file.
		image="$(realpath "$2")"
		shift 2
		;;
	--attach)
		[[ $# -ge 2 ]] || die "--attach needs a path"
		IFS=', ' read -r -a paths <<<"$2"
		for path in "${paths[@]}"; do
			[[ -f "$path" ]] || die "no medium at '$path'"
			attach+=("$path")
		done
		shift 2
		;;
	# These become environment variables for qemu-run.sh, which is the layer that speaks to QEMU.
	# The flag is the interface; the variable is plumbing. Passing them as bare variables worked and
	# still does, but nothing announced them - they were not in --help and a typo was silent.
	--smp)
		[[ $# -ge 2 ]] || die "--smp needs a count"
		[[ "$2" =~ ^[0-9]+$ ]] || die "--smp takes a number, got '$2'"
		export SMP="$2"
		shift 2
		;;
	--mem)
		[[ $# -ge 2 ]] || die "--mem needs a size"
		[[ "$2" =~ ^[0-9]+[KMG]?$ ]] || die "--mem takes a QEMU size like 512M or 4G, got '$2'"
		export MEM="$2"
		shift 2
		;;
	--serial)
		[[ $# -ge 2 ]] || die "--serial needs a spec"
		export SERIAL="$2"
		shift 2
		;;
	--vnc-addr)
		[[ $# -ge 2 ]] || die "--vnc-addr needs an address"
		export VNC_ADDR="$2"
		shift 2
		;;
	--spice-addr)
		[[ $# -ge 2 ]] || die "--spice-addr needs an address"
		export SPICE_ADDR="$2"
		shift 2
		;;
	--spice-port)
		[[ $# -ge 2 ]] || die "--spice-port needs a port"
		[[ "$2" =~ ^[0-9]+$ ]] || die "--spice-port takes a number, got '$2'"
		export SPICE_PORT="$2"
		shift 2
		;;
	--debug)
		debug=1
		shift
		;;
	--gdb)
		attach_gdb=1
		shift
		;;
	*) die "unexpected argument '$1' (try --help)" ;;
	esac
done

# The host's architecture by default, so `run.sh` on an ARM64 machine is native.
if [[ -z "$arch" ]]; then
	case "$(uname -m)" in
	x86_64) arch=x86_64 ;;
	aarch64 | arm64) arch=aarch64 ;;
	riscv64) arch=riscv64 ;;
	*) arch=x86_64 ;;
	esac
fi

# THIS SCRIPT BUILDS NOTHING. `./build.sh` builds; this boots what is there.
#
# It used to build, assemble a medium and boot in one step, and that is how a disk image came to
# carry the wrong kernel: with the three joined, nothing sits between them to look at what went on.
[[ -n "$image" ]] && export BOOT_IMAGE="$image"

kernel="$BUILD_DIR/cargo/kernel/$(target_triple "$arch")/debug/kernel"
[[ -f "$kernel" ]] || die "no kernel for $arch - run: ./build.sh --arch $arch"

# THE OTHER HALF OF --debug, and it boots nothing.
#
# `--debug` starts QEMU stopped on :1234; this attaches to it with the kernel's symbols, from a
# second panel. Placed after the kernel-exists check and before everything that assembles or boots,
# because attaching needs the ELF and nothing else.
if ((attach_gdb)); then
	[[ $debug -eq 0 ]] || die "--gdb attaches to a waiting guest; --debug starts one. Use them in two panels, not in one command"
	exec gdb -x "$SRC_DIR/boot/gdb-init" "$kernel"
fi
if [[ -z "$image" && ! -f "$BUILD_DIR/boot/system-volume-$arch.img" ]]; then
	die "no system volume for $arch - run: ./build.sh --arch $arch (or boot a medium with --image)"
fi

# Extra media, as read-only drives QEMU attaches beside the boot medium.
if [[ ${#attach[@]} -gt 0 ]]; then
	extra=""
	for path in "${attach[@]}"; do
		extra+=" -drive file=$(realpath "$path"),format=raw,media=disk,readonly=on"
	done
	export QEMU_EXTRA="${QEMU_EXTRA:-}$extra"
fi

export DISPLAYS="${displays[*]:-}"
export SERIAL="${SERIAL:-stdio}"
# The device-tree architectures have no non-UEFI way in since the packaged bootstrap archive was
# retired; x86_64 boots its ISO through OVMF either way.
[[ "$arch" != x86_64 ]] && export UEFI="${UEFI:-1}"
if [[ $debug -eq 1 ]]; then
	export DEBUG=1 NOKVM=1
fi

cd "$SRC_DIR"
exec boot/qemu-run.sh "$arch" "$kernel"
