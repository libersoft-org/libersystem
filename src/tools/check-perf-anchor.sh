#!/usr/bin/env bash
# THE LINE ADDRESSED TO A TOOL APPEARS ONLY WHERE A TOOL IS READING IT.
#
# `boot_main` publishes the calibrated TSC frequency as `\x1ePERF tsc_hz <n>` - a record-separator
# line that `perf-trace.py` converts ring-3 cycle markers with. It used to be emitted whenever ANY
# boot profile was named over fw_cfg, and the profile a PERSON boots for an interactive development
# instance is one of those - so the one line in the report meant for a program was shown to every
# operator who ever used that profile.
#
# TWO BOOTS, AND THE ABSENCE IS HALF THE CHECK. `development-trace` is what `perf-trace.py` boots and
# is the only profile that carries the anchor; an ordinary boot must not. Checking only the presence
# would pass on a kernel that printed it everywhere, which is the state this exists to keep out.
#
# This is M0158's M4 gate: without it the condition is a comment, and the day it stops holding the
# only witness is somebody trying to take a measurement.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../.."

fail() {
	echo "perf-anchor: $*" >&2
	exit 1
}

command -v qemu-system-x86_64 >/dev/null || fail "qemu-system-x86_64 is not installed"
KERNEL=".build/cargo/kernel/x86_64-unknown-none/debug/kernel"
[[ -f "$KERNEL" ]] || fail "no built kernel at $KERNEL - build first:  ./build.sh --arch x86_64"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# ONE ANCHOR IS ALL THIS NEEDS, so the boot is cut as soon as the report is out rather than run to a
# shell: the line is printed by `boot_main` before userspace starts, and waiting for a prompt would
# make this gate cost a full bring-up to read one line.
boot_with() {
	local profile="$1" out="$2"
	DEV_PROFILE=1 LIBER_BOOT_PROFILE="$profile" SERIAL="file:$out" QEMU_TIMEOUT=60 \
		timeout 90 src/harness/qemu-run.sh x86_64 "$KERNEL" >/dev/null 2>&1 || true
}

trace="$work/trace.log"
boot_with development-trace "$trace"
grep -aq "boot OK" "$trace" || fail "the development-trace boot did not reach the kernel's report at all; there is nothing to look for"
grep -aq "PERF tsc_hz" "$trace" || fail "the development-trace profile published no tsc_hz anchor - perf-trace.py converts every cycle marker with it, and without it a trace is a host-wall-clock estimate that looks identical"
echo "perf-anchor:   development-trace publishes the anchor: $(grep -a -m1 -o 'PERF tsc_hz [0-9]*' "$trace")"

plain="$work/plain.log"
boot_with development "$plain"
grep -aq "boot OK" "$plain" || fail "the development boot did not reach the kernel's report, so its absence of an anchor proves nothing"
if grep -aq "PERF tsc_hz" "$plain"; then
	fail "the ordinary development profile published the anchor - that is a raw record-separator line on the console of somebody who booted an interactive instance"
fi
echo "perf-anchor:   the interactive development profile does not"
echo "perf-anchor: the harness anchor is published to the harness and to nobody else"
