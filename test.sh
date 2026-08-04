#!/usr/bin/env bash
# Run the in-kernel test suites.
#
# This replaces thirty Justfile recipes, twelve of which were `test-tags` alone: architecture,
# build-only and fast are three independent switches, and spelling every combination into a name
# is what produces `test-tags-build-fast-aarch64`. They are flags here.

SCRIPT_NAME=test.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

help() {
	usage_and_exit <<EOF
usage: test.sh [--arch ARCH[,ARCH...]] [--tags TAG[,TAG...]] [--fast] [--build-only]

Runs the in-kernel test suite: a kernel is built, booted in QEMU, and the tests run INSIDE the
running system. Host-side gates that inspect artifacts without booting anything live in check.sh.

With no arguments: every test, x86_64.

  --arch ARCH    x86_64 | aarch64 | riscv64 | all   (default: x86_64)
  --tags TAGS    run only these tags, plus the smoke set (--list-tags to see them)
  --list-tags    print the tags this kernel defines and exit
  --fast         reuse a content-verified userspace preflight instead of rebuilding it
  --build-only   compile the test kernel without booting QEMU
  --smp N        cores given to the guest
  --timeout SEC  per-suite wall-clock limit
  -h, --help     this text

examples:
  ./test.sh
  ./test.sh --arch all
  ./test.sh --arch riscv64 --tags filesystem,storage
  ./test.sh --fast --build-only

The three suites share the disk images under .build/boot, so two runs at once fail with a QEMU
write-lock error naming an image rather than the run that holds it. --arch all runs them in turn.
EOF
}

archs=()
tags=""
fast=0
build_only=0

while [[ $# -gt 0 ]]; do
	case "$1" in
	-h | --help) help ;;
	--arch)
		[[ $# -ge 2 ]] || die "--arch needs a value"
		picked_raw="$(parse_list "$2" architecture "${ARCHS_ALL[*]}")"
		mapfile -t picked <<<"$picked_raw"
		archs+=("${picked[@]}")
		shift 2
		;;
	--list-tags)
		sed -n '/^define_test_tags! {/,/^}/p' "$SRC_DIR/kernel/tests.rs" |
			grep -oP '=> "\K[a-z0-9_]+' | sort -u | tr '\n' ' '
		echo
		exit 0
		;;
	--tags)
		[[ $# -ge 2 ]] || die "--tags needs a value"
		tags="$2"
		shift 2
		;;
	--fast)
		fast=1
		shift
		;;
	--build-only)
		build_only=1
		shift
		;;
	--smp)
		[[ $# -ge 2 ]] || die "--smp needs a count"
		[[ "$2" =~ ^[0-9]+$ ]] || die "--smp takes a number, got '$2'"
		export SMP="$2"
		shift 2
		;;
	--timeout)
		[[ $# -ge 2 ]] || die "--timeout needs seconds"
		[[ "$2" =~ ^[0-9]+[smh]?$ ]] || die "--timeout takes seconds (or 5m, 1h), got '$2'"
		export TEST_TIMEOUT="$2"
		shift 2
		;;
	*) die "unexpected argument '$1' (try --help)" ;;
	esac
done

[[ ${#archs[@]} -eq 0 ]] && archs=(x86_64)

# THIS SCRIPT BUILDS NOTHING. It checks that what the suite boots is there, and says what to run
# if it is not.
#
# Testing and building are separate because a test that quietly builds cannot tell you it tested
# something other than what you meant - and because a suite that rebuilds is a suite you cannot
# point at an artifact you already have. The test KERNEL is still compiled here, by `cargo test`,
# because building and running it is one operation as far as cargo is concerned.
require_built() {
	local arch="$1" volume="$BUILD_DIR/boot/system-volume-$arch.img"
	[[ -f "$volume" ]] || die "no system volume for $arch - run: ./build.sh --arch $arch"
	if [[ "$arch" != x86_64 ]]; then
		local efi="$BUILD_DIR/cargo/loader/$(loader_triple "$arch")/debug/libersystem-loader.efi"
		[[ -f "$efi" ]] || die "no loader for $arch - run: ./build.sh --arch $arch --part loader"
	fi
}

# The full preflight: verify and record the staged image the suite will boot.
preflight_full() {
	local arch="$1"
	require_built "$arch"
	(cd "$SRC_DIR" && boot/check-test-tags.sh)
	(cd "$SRC_DIR" && tools/check-staged-image.sh "$arch")
	(cd "$SRC_DIR" && boot/test-preflight.sh write "$arch")
}

# The fast preflight: verify the recorded userspace still matches instead of rebuilding it.
preflight_fast() {
	local arch="$1"
	require_built "$arch"
	(cd "$SRC_DIR" && boot/check-test-tags.sh)
	(cd "$SRC_DIR" && boot/test-preflight.sh check "$arch")
}

for arch in "${archs[@]}"; do
	if [[ $fast -eq 1 ]]; then preflight_fast "$arch"; else preflight_full "$arch"; fi
	args=("$arch")
	[[ -n "$tags" ]] && args+=("$tags")
	[[ $build_only -eq 1 ]] && args+=(--build-only)
	# UEFI=1 on the device-tree architectures: they have no other way in since the packaged
	# bootstrap archive and the magic scan that found it were retired.
	if [[ "$arch" == x86_64 ]]; then
		(cd "$SRC_DIR" && boot/test-kernel.sh "${args[@]}")
	else
		(cd "$SRC_DIR" && UEFI="${UEFI:-1}" boot/test-kernel.sh "${args[@]}")
	fi
done
