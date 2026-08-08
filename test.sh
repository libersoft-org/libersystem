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

"What do I run after changing X" is ./verify.sh, not this script. It plans over every kind of
check - builds, host suites, gates, conformance runs and guest runs on the targets that can be
affected - from a derived dependency graph, and this script is one of the things it calls:

  ./verify.sh --for src/user/libs/audio/flac --plan
  ./verify.sh --for-change

The three suites share the disk images under .build/boot, so two runs at once fail with a QEMU
write-lock error naming an image rather than the run that holds it. --arch all runs them in turn.

x86_64 runs under KVM; aarch64 and riscv64 are emulated instruction by instruction, so a loaded
host slows them by far more than it slows x86_64. Measured on 2026-08-08 with the machine at load
115: the ten image-tagged tests took 63 s on x86_64 and 1518 s on aarch64 - twenty-four times -
and the default --timeout was not the problem it looked like. If an emulated run stops on a heavy test,
check the load average before the diff.
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
# A QEMU already holding this architecture's disk images.
#
# Two runs at once fail deep inside QEMU with `Is another process using the image [...]`, naming
# the FILE and not the run that holds it - which reads as a test failure and is not one. It has
# cost four runs in this tree, every time from an instance someone (usually me) left behind. The
# check belongs here because this script's job is to refuse with a reason rather than to discover
# the reason at minute thirty.
require_no_stray_qemu() {
	local arch="$1" pattern pids
	case "$arch" in
	x86_64) pattern="qemu-system-x86_64" ;;
	aarch64) pattern="qemu-system-aarch64" ;;
	riscv64) pattern="qemu-system-riscv64" ;;
	esac
	# ANCHORED at the start of the command line.
	#
	# Plain `-f "$pattern"` matches anything that merely MENTIONS qemu - a wrapper script, a grep,
	# the shell that typed the kill command - so the check fired on itself and named a different PID
	# every time. `-x` on the process name cannot work either: the kernel truncates it to 15
	# characters and `qemu-system-riscv64` is 19, so it matches nothing at all and the check silently
	# never fires. Anchoring the command line catches the real process and nothing that talks about
	# it.
	pids="$(pgrep -f "^([^ ]*/)?$pattern " 2>/dev/null || true)"
	[[ -z "$pids" ]] && return 0
	die "a $pattern is already running (PID ${pids//$'\n'/, }) and holds this architecture's disk images.
    It is probably a run left behind. Stop it and try again:  kill ${pids//$'\n'/ }"
}

require_built() {
	local arch="$1" volume="$BUILD_DIR/boot/system-volume-$arch.img"
	[[ -f "$volume" ]] || die "no system volume for $arch - run: ./build.sh --arch $arch"
	# STALE is as bad as missing, and harder to see.
	#
	# This script builds nothing so that a suite cannot quietly test something other than what was
	# meant. The other half of that bargain is refusing to test what is there when it is older than
	# the sources - `./build.sh` defaults to x86_64, so `./build.sh --part user` followed by
	# `./test.sh --arch aarch64` ran the previous binary and said nothing. That cost two
	# twenty-minute runs in one afternoon, and both times the failure looked like a defect in the
	# code under test.
	#
	# Measured against the stamp `./build.sh` writes, not against the artifacts. An artifact's
	# age understates the build's: `mkpackages` declines to rewrite a file whose bytes are
	# unchanged, so a kernel-only edit - or a `git checkout` restoring a file to what was already
	# built - leaves the sources newer than an image that is byte-for-byte current, and the suite
	# refused to run while asking for the build that had just succeeded. The stamp records what
	# the artifacts cannot: that a build looked at these sources.
	# `volume` is the part that produces the image the suite boots, and it is the last of the
	# chain that feeds it, so its stamp is the one that dates the userspace.
	local stamp="$BUILD_DIR/state/built-$arch-volume"
	[[ -f "$stamp" ]] || die "no build stamp for $arch - run: ./build.sh --arch $arch"
	if [[ "$(cat "$stamp")" != "$(source_digest "${VOLUME_SOURCES[@]}")" ]]; then
		die "the $arch build does not match the sources
    Nothing here rebuilds it, so the suite would test the previous one:  ./build.sh --arch $arch"
	fi
	if [[ "$arch" != x86_64 ]]; then
		local efi="$BUILD_DIR/cargo/loader/$(loader_triple "$arch")/debug/libersystem-loader.efi"
		[[ -f "$efi" ]] || die "no loader for $arch - run: ./build.sh --arch $arch --part loader"
		local loader_stamp="$BUILD_DIR/state/built-$arch-loader"
		if [[ ! -f "$loader_stamp" || "$(cat "$loader_stamp")" != "$(source_digest loader)" ]]; then
			die "the $arch loader does not match its sources
    Run:  ./build.sh --arch $arch --part loader"
		fi
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

# --arch all runs the architectures CONCURRENTLY.
#
# They no longer share anything: the ESP, the system disk, the FAT/ISO/UDF/USB media and the
# firmware variable stores are all per-architecture, and the package archives stopped being written
# under one name. Cargo still serialises the compile through its target-directory lock, which is
# fine - the compile is short and cached, and the two emulated boots are what took the hour.
run_arch() {
	local arch="$1"
	require_no_stray_qemu "$arch"
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
}

if [[ ${#archs[@]} -eq 1 ]]; then
	run_arch "${archs[0]}"
	exit 0
fi

# Started together, waited for individually, so one architecture's failure does not hide another's
# result - and every one of them is reported.
pids=()
for arch in "${archs[@]}"; do
	run_arch "$arch" &
	pids+=("$!")
done
failed=()
for i in "${!pids[@]}"; do
	wait "${pids[$i]}" || failed+=("${archs[$i]}")
done
if [[ ${#failed[@]} -gt 0 ]]; then
	die "failed: ${failed[*]}"
fi
note "all architectures passed: ${archs[*]}"
