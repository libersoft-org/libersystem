#!/usr/bin/env bash
# check-qemu-2d-account.sh - the frame account: where a 2D frame's time goes, layer by layer.
#
# WHAT IT KEEPS TRUE. The live 2D demo's interval is made of an application, a transport, DisplayService,
# the virtio-gpu driver, the device and a frame loop's own pacing - and the account in `docs/PERF.md`
# names each of those terms with a number. This gate is the thing that takes those numbers again after
# the next change to any layer in it, and refuses a run that does not measure what the account says it
# measures: a residue past five percent in any damage shape, a term with no work/wait pair, a record the
# kernel's buffer refused, no `tsc_hz` anchor, an account that disagrees with the demo's own armed
# report, or a second thread of the demo on a CPU inside a draw.
#
# ON THE REAL DRIVER, NOT THE GUEST SUITE'S STAND-IN. The suite's demo test has a 192x128 stand-in GPU
# with no driver and no device; this boots the development ISO through the lab path - the ordinary
# image, the instrument dormant in it until a `development-trace` boot arms it - and all its boots boot
# that one image, by digest.
#
# THREE BOOTS, SMP=4, HEADLESS:
#   1. `development-trace`, the default 1280x800 scanout: the account at 640x480 (scaled - the old
#      baseline's condition), at the screen's own size (direct), and at 640x480 with fifteen hidden
#      surfaces; then the same scene offscreen at 640x480 and 1280x800;
#   2. `development-trace` with GPU_SIZE=640x480: the account at the screen's own size (direct);
#   3. `development` (every site dormant): the 640x480 run without the account - the instrument's cost -
#      the same run with the core kept busy before each draw, and `--primitives`, so the dormant site is
#      measured dormant.
#
# THE OPTIMISED ROW. With CARGO_PROFILE_DEV_OPT_LEVEL exported, and ACCOUNT_REFERENCE naming the
# results directory of an ordinary run on the same tree, the gate takes the row only if the image digest
# and the staged kernel and driver differ from the reference's and the staged DisplayService and demo do
# not - a reproducible build that booted the same image would otherwise be reported as optimised.
#
# IT REFUSES TO START WHILE ANOTHER GUEST IS UP, rather than reuse an instance booted under conditions it
# cannot state or take down one it did not start.

SCRIPT_NAME=check-qemu-2d-account.sh
source "$(dirname "${BASH_SOURCE[0]}")/../../lib.sh"

RESULTS="${ACCOUNT_RESULTS:-$BUILD_DIR/logs/qemu-2d-account/$(date -u +%Y%m%dT%H%M%SZ)${CARGO_PROFILE_DEV_OPT_LEVEL:+-opt$CARGO_PROFILE_DEV_OPT_LEVEL}}"
# A RELATIVE DIRECTORY IS THE REPOSITORY'S, not the working directory's: `check.sh` runs gates from
# `src/`, and results written under it would move the build digest between this gate's own boots.
[[ "$RESULTS" == /* ]] || RESULTS="$REPO_ROOT/$RESULTS"
mkdir -p "$RESULTS"
SERIAL_LOG="$BUILD_DIR/boot/lab-serial.log"
IMAGE="$BUILD_DIR/boot/libersystem-dev.iso"
COLLECTOR="$SRC_DIR/harness/frame_account.py"
TARGET=x86_64-unknown-none
BOOTED=0
cleanup() {
	if ((BOOTED)); then
		"$REPO_ROOT/lab.sh" quit >/dev/null 2>&1 || true
	fi
}
trap cleanup EXIT

# THE COLLECTOR FIRST, against fixture logs, before anything is booted: a collector that cannot refuse a
# refused record, a missing site or a second thread in a draw would pass every run below.
python3 "$SRC_DIR/tools/check-frame-account-collector.py" >"$RESULTS/collector-fixtures.log" 2>&1 || die "the collector failed its fixture logs - see $RESULTS/collector-fixtures.log"
note "the collector joins, groups, splits and refuses as its fixtures require"

# ONE GUEST AT A TIME. A process name is enough here: this is a refusal, never a kill.
if ps -eo comm= | grep -q '^qemu-system'; then
	die "a QEMU guest is already running - this gate boots its own under stated conditions and will not share or take down another"
fi

# WHEN THE DEMO HAS A WORKER POOL, every run pins one worker so the draw is the serial walk the account
# describes (the demo has no `--workers` yet, so this list is empty).
PIN=()

# The staged artifact of a program, where `mkpackages` takes it from: a static program from cargo's
# `debug` output, a dynamic one from the release PIE tree `build-shared` fills.
staged() {
	python3 - "$SRC_DIR/user/services/manifest.toml" "$1" "$BUILD_DIR" "$TARGET" <<'EOF'
import sys, tomllib
manifest, name, build, target = sys.argv[1:]
for program in tomllib.load(open(manifest, "rb"))["programs"]:
	if program["name"] == name:
		if program.get("linkage") == "dynamic":
			print(f"{build}/image/{target}/{program['destination'].removesuffix('.lsexe')}\trelease PIE (build-shared)")
		else:
			print(f"{build}/cargo/user/{target}/debug/{name}\tcargo dev profile (build.sh)")
		break
else:
	sys.exit(f"no program named {name}")
EOF
}

record_conditions() {
	local boot="$1"
	{
		printf 'image\t%s\n' "$(sha256sum "$IMAGE" | cut -d' ' -f1)"
		printf 'kernel\t%s\t%s\tcargo dev profile (build.sh, image.sh)\n' "$BUILD_DIR/cargo/kernel/$TARGET/debug/kernel" "$(sha256sum "$BUILD_DIR/cargo/kernel/$TARGET/debug/kernel" | cut -d' ' -f1)"
		local program line path how
		for program in virtio_gpu display_service test2d-sw; do
			line="$(staged "$program")" || die "the manifest names no program $program"
			path="${line%%$'\t'*}"
			how="${line#*$'\t'}"
			printf '%s\t%s\t%s\t%s\n' "$program" "$path" "$(sha256sum "$path" | cut -d' ' -f1)" "$how"
		done
		printf 'cargo-profile-dev-opt-level\t%s\n' "${CARGO_PROFILE_DEV_OPT_LEVEL:-unset}"
		printf 'qemu\t%s\n' "$(qemu-system-x86_64 --version | head -1)"
		printf 'host-cpu\t%s\n' "$(lscpu | sed -n 's/^Model name: *//p' | head -1)"
		printf 'qemu-args\t%s\n' "$(ps -eo args= | grep -m1 '^[^ ]*qemu-system-x86_64' || true)"
		# A `development` boot prints no anchor and attaches no buffer, so a line that is absent is
		# recorded as absent rather than ending the gate.
		local pattern
		for pattern in 'boot profile' 'cores online' 'dma: boot DMA mode' 'tsc_hz' 'frame-account buffer'; do
			printf 'guest\t%s\n' "$(grep -a -m1 "$pattern" "$SERIAL_LOG" | tr -d '\036\r' || echo "absent: $pattern")"
		done
	} >"$RESULTS/conditions-$boot.tsv"
}

boot() {
	local name="$1"
	shift
	"$REPO_ROOT/lab.sh" quit >/dev/null 2>&1 || true
	# THE GUEST IS OURS FROM THE MOMENT IT IS STARTED, not from the moment it answers: a boot whose wait
	# fails leaves its QEMU running, and the exit handler has to take that one down as well.
	BOOTED=1
	# Headless and four cores, whatever the caller's environment says.
	env -u DISPLAYS "$@" SMP=4 "$REPO_ROOT/lab.sh" boot >"$RESULTS/boot-$name.log" 2>&1 || die "boot $name failed - see $RESULTS/boot-$name.log"
	record_conditions "$name"
}

run() {
	local name="$1"
	shift
	"$REPO_ROOT/lab.sh" sh --timeout 900 "test2d-sw $*" >"$RESULTS/run-$name.out" 2>&1 || die "run $name did not complete - see $RESULTS/run-$name.out"
	grep -q "test2d-sw: done" "$RESULTS/run-$name.out" || die "run $name did not reach its end - see $RESULTS/run-$name.out"
}

ACCOUNT=(--account --no-input --no-second-surface --frames=160 --phase-frames=40)

boot trace DEV_PROFILE=1 LIBER_BOOT_PROFILE=development-trace
run scaled "${ACCOUNT[@]}" "${PIN[@]}" --size=640x480
run direct "${ACCOUNT[@]}" "${PIN[@]}"
run hidden "${ACCOUNT[@]}" "${PIN[@]}" --size=640x480 --hidden-surfaces=15
run offscreen-640x480 --offscreen --frames=160 --phase-frames=40 --size=640x480 "${PIN[@]}"
run offscreen-1280x800 --offscreen --frames=160 --phase-frames=40 --size=1280x800 "${PIN[@]}"
cp "$SERIAL_LOG" "$RESULTS/serial-trace.log"

boot small DEV_PROFILE=1 LIBER_BOOT_PROFILE=development-trace GPU_SIZE=640x480
run small-direct "${ACCOUNT[@]}" "${PIN[@]}"
cp "$SERIAL_LOG" "$RESULTS/serial-small.log"

boot dormant DEV_PROFILE=1
run dormant --no-input --no-second-surface --frames=160 --phase-frames=40 --size=640x480 "${PIN[@]}"
# THE SAME RUN WITH THE CORE KEPT BUSY BEFORE EACH DRAW: a draw that falls to the offscreen figure behind
# the spin was paying for starting on a core that had been idle, which is the host's and not the draw's.
run warm-core --no-input --no-second-surface --frames=160 --phase-frames=40 --size=640x480 --warm-core=60 "${PIN[@]}"
run primitives --primitives
cp "$SERIAL_LOG" "$RESULTS/serial-dormant.log"
"$REPO_ROOT/lab.sh" quit >/dev/null 2>&1 || true
BOOTED=0

# ONE IMAGE FOR EVERY BOOT. The build is reproducible, so a digest that moved means something changed
# between two boots of one measurement - and the comparisons below would be across it.
digests="$(cut -f2 <(grep -h '^image' "$RESULTS"/conditions-*.tsv) | sort -u | wc -l)"
((digests == 1)) || die "the three boots booted $digests different images - the runs are not on one tree"

# THE ACCOUNTS, each against the demo's own armed report.
account() {
	local name="$1" log="$2" drain="$3"
	python3 "$COLLECTOR" "$log" --drain "$drain" --demo-report "$RESULTS/run-$name.out" --json "$RESULTS/account-$name.json" >"$RESULTS/account-$name.txt" 2>&1 || die "the $name account was refused: $(tail -n 1 "$RESULTS/account-$name.txt")"
	note "account $name: $(head -n 1 "$RESULTS/account-$name.txt")"
}
account scaled "$RESULTS/serial-trace.log" 1
account direct "$RESULTS/serial-trace.log" 2
account hidden "$RESULTS/serial-trace.log" 3
account small-direct "$RESULTS/serial-small.log" 1

# THE OPTIMISED ROW'S PROVENANCE, when this run is the optimised one.
if [[ -n "${CARGO_PROFILE_DEV_OPT_LEVEL:-}" && -n "${ACCOUNT_REFERENCE:-}" ]]; then
	# The repository's, as RESULTS is: `check.sh` runs this from `src/`.
	[[ "$ACCOUNT_REFERENCE" == /* ]] || ACCOUNT_REFERENCE="$REPO_ROOT/$ACCOUNT_REFERENCE"
	reference="$ACCOUNT_REFERENCE/conditions-trace.tsv"
	[[ -f "$reference" ]] || die "ACCOUNT_REFERENCE names no ordinary run's conditions ($reference)"
	digest_of() { awk -F'\t' -v key="$1" '$1 == key { print ($1 == "image") ? $2 : $3 }' "$2"; }
	for key in image kernel virtio_gpu; do
		[[ "$(digest_of "$key" "$reference")" != "$(digest_of "$key" "$RESULTS/conditions-trace.tsv")" ]] || die "the optimised run's $key is the ordinary run's - the variable did not reach it, and this is not an optimised row"
	done
	for key in display_service test2d-sw; do
		[[ "$(digest_of "$key" "$reference")" == "$(digest_of "$key" "$RESULTS/conditions-trace.tsv")" ]] || die "the optimised run's $key differs from the ordinary run's - a release PIE moved, so the comparison is not of the kernel and the driver alone"
	done
	note "the optimised row is from another image: kernel and driver differ, DisplayService and the demo do not"
fi

note "PASS - four accounts closed within five percent, each against the demo's own report; results in $RESULTS"
