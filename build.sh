#!/usr/bin/env bash
# Build the system, or the parts of it you name.
#
# `--part` names a CLOSED SET. It deliberately does not name individual libraries or executables:
# that would be a second source of truth beside the manifest and it would drift, exactly as
# INSTALL.md drifted from the recipes within hours. Cargo already selects at that granularity, so
# finer choices pass through: `./build.sh --part user -- -p imgconv`.

SCRIPT_NAME=build.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
# EVERY RUN ENDS WITH A VERDICT, from the one EXIT dispatcher in lib.sh - and cleanups REGISTER with
# it rather than installing traps of their own. A run that fails is otherwise indistinguishable from
# a run that is still going, which is exactly how a failed build came to be waited on for half an
# hour. `docs/TESTING.md` states what the line and the terminal record do and do not promise.
arm_run_verdict

# The build steps live here rather than in a file of their own.
#
# They were split out when `test.sh` and `run.sh` also built things and needed the same
# definitions. Neither builds any more - building is this script's job alone - so the split had one
# caller left and was just a file to open twice.

# The shared libraries come from the MANIFEST, which calls itself the only hand-edited source of
# truth. They were written out by hand three times in the Justfile - once per architecture, in a
# 900-character line - and this file briefly carried a fourth copy. A list of what the system
# contains belongs in one place; every copy is a chance to disagree with it.
shared_libs() {
	(cd "$SRC_DIR" && tools/system-manifest.sh library-crates)
}

# `--features development` when LIBER_DEVELOPMENT=1, matching the Justfile's `dev_features`.
dev_features() {
	[[ "${LIBER_DEVELOPMENT:-0}" == "1" ]] && echo "--features development"
}

# Crate directories are resolved through the manifest rather than hard-coded, so a crate that moves
# does not need this file edited.
# Absolute. The tool answers relative to `src/`, and every caller here has already changed
# directory by the time it uses the answer.
source_path() {
	local rel
	rel="$(cd "$SRC_DIR" && tools/source-path.sh "$1")"
	[[ "$rel" == /* ]] && {
		echo "$rel"
		return
	}
	echo "$SRC_DIR/$rel"
}

# `--target <triple>` for every architecture except the host's default build.
target_flag() {
	[[ "$1" == x86_64 ]] && return 0
	echo "--target $(target_triple "$1")"
}

step_sdk() {
	step "sdk"
	# `--workspace`, because `src/sdk` is the SDK LIBRARY now and the component is the example
	# beside it. Without it cargo builds the root package only and the staged `.wasm` is whatever
	# the last build left behind.
	#
	# THE SHIPPING BUILD, and the one the image stages. `dev-diagnostics` is off, so the guest's
	# panic handler traps in silence - a component should not narrate its own failures to a log it
	# does not own - and `mkpackages` reads `.build/cargo/sdk/.../liber_component.wasm` for exactly
	# that reason.
	run_owned_shell '(cd "$SRC_DIR/sdk" && cargo build --release --target wasm32-unknown-unknown --workspace)'
	# A SIDECAR NAMING WHAT IT WAS BUILT FROM.
	#
	# A missing artifact is a failure and a STALE one used to pass: build the SDK, change
	# `report_panic()`, run the host-tests gate alone, and the old binary made the test green against
	# an implementation that no longer exists. The test compares this against a freshly computed
	# digest of the same inputs, so an artifact that predates the sources is named as stale rather
	# than trusted. See `tools/sdk-inputs.sh` for what goes into it.
	(cd "$SRC_DIR" && tools/sdk-inputs.sh default) >"$BUILD_DIR/cargo/sdk/wasm32-unknown-unknown/release/liber_component.wasm.inputs"
	# AND THE SAME EXAMPLE WITH THE FEATURE ON, into its own target directory so the artifact above
	# is untouched.
	#
	# `src/wasm`'s toolchain test asserts what a real guest does when it panics, and both halves of
	# `report_panic` are worth asserting: with the feature it logs its line through the granted log
	# AND traps, without it it only traps. One artifact can only be one of those, and because
	# nothing in the tree passed `--features` the half that ran was always the silent one -
	# everything under `#[cfg(feature = "dev-diagnostics")]` in `src/sdk/src/panic.rs` had no
	# automatic coverage against a real guest at all. Two artifacts, each asserted unconditionally.
	run_owned_shell '(cd "$SRC_DIR/sdk" && CARGO_TARGET_DIR="$BUILD_DIR/cargo/sdk-dev" cargo build --release --target wasm32-unknown-unknown -p liber_component --features liber-sdk/dev-diagnostics)'
	(cd "$SRC_DIR" && tools/sdk-inputs.sh dev-diagnostics) >"$BUILD_DIR/cargo/sdk-dev/wasm32-unknown-unknown/release/liber_component.wasm.inputs"
}

step_libs() {
	local arch="$1"
	step "libs ($arch)"
	local pairs=()
	mapfile -t pairs < <(shared_libs)
	[[ ${#pairs[@]} -gt 0 ]] || die "the manifest lists no shared libraries"
	run_owned_shell '(cd "$SRC_DIR" && tools/build-shared.sh "$(target_triple "$arch")" "${pairs[@]}")'
}

step_user() {
	local arch="$1" flag
	ensure step_libs "$arch"
	flag="$(target_flag "$arch")"
	step "user ($arch)"
	# shellcheck disable=SC2046,SC2086
	run_owned_shell '(cd "$(source_path system_manager)" && cargo build $flag)'
	# shellcheck disable=SC2046,SC2086
	run_owned_shell '(cd "$(source_path services)" && cargo build $flag $(dev_features))'
	# shellcheck disable=SC2046,SC2086
	run_owned_shell '(cd "$(source_path storage)" && cargo build $flag)'
	# shellcheck disable=SC2046,SC2086
	run_owned_shell '(cd "$(source_path drivers)" && cargo build $flag $(dev_features))'
}

step_kernel() {
	local arch="$1" flag
	flag="$(target_flag "$arch")"
	step "kernel ($arch)"
	# shellcheck disable=SC2086
	(cd "$SRC_DIR/kernel" && cargo build $flag)
}

step_loader() {
	local arch="$1"
	step "loader ($arch)"
	# UNDER THE SHARED LOADER LOCK (2026-09-01).
	#
	# Every loader build in this tree writes ONE output path per target, and the test harness stages a
	# run-private copy of it so a concurrent run cannot swap the bytes its medium is assembled from.
	# That staging took `kernel-test-build.lock` and this writer did not, so the lock was held against
	# nobody: this build could replace the shared output while the harness was copying it, and an
	# A-to-B-to-A trust-profile cycle could even restore the original hash while the copy consumed B.
	# A lock the other writers ignore does not make anything authoritative, so the writers take it.
	mkdir -p "$SRC_DIR/../.build/state"
	# IN A SUBSHELL, so the lock is released when the build finishes rather than held for the rest of
	# this script: it protects the loader OUTPUT, and holding it across the volume and package steps
	# would block a concurrent run's staging for no reason.
	(
		flock 9
		case "$arch" in
		# The host triple's UEFI target, and cargo takes the configuration of the working directory -
		# which is why both of these `cd` into the loader rather than passing `--manifest-path`.
		x86_64) (cd "$SRC_DIR/boot/loader" && cargo build) ;;
		aarch64) (cd "$SRC_DIR/boot/loader" && cargo build --target aarch64-unknown-uefi) ;;
		# riscv64 has no UEFI rustc target at all: its EFI application is assembled by hand from a
		# static PIE, a linker script and objcopy, which is a program and lives in one.
		riscv64) (cd "$SRC_DIR" && tools/build-loader-riscv64.sh) ;;
		*) die "no loader for '$arch'" ;;
		esac
	) 9>"$SRC_DIR/../.build/state/kernel-test-build.lock"
}

# Assemble the boot packages from an ALREADY-BUILT userspace. Deliberately without a `user`
# dependency: a packaging step that quietly builds what it is missing cannot tell you something
# was missing.
# EVERY MEDIUM INPUT IS PUBLISHED UNDER THE ONE LOCK A CONSUMER'S SNAPSHOT TAKES. `mkpackages`
# writes each output to a temporary name and, with this set, renames the whole set into place in one
# `flock`-held step over `kernel-test-build.lock` - the lock the loader build and the harness staging
# already take. `mkimage.sh` copies its inputs under the same lock, so it sees a complete generation
# and never one file from before a publication and the next from after it. Held for the renames
# only: never across a compile.
export LIBER_PUBLISH_LOCK="$SRC_DIR/../.build/state/kernel-test-build.lock"

step_packages() {
	local arch="$1"
	step "packages ($arch)"
	mkdir -p "$SRC_DIR/../.build/state"
	(cd "$SRC_DIR/tools/mkpackages" && cargo run --quiet -- "$arch")
}

# The system volume, and the kernel goes on it only when asked for by PATH.
#
# It used to read `.build/boot/kernel`, a slot every image builder writes and nobody owns, so the
# volume took whatever the previous recipe had left there - and a disk image built after a test run
# carried the TEST kernel and booted into the suite. Naming the file removes the slot from the path.
step_volume() {
	local arch="$1" with_kernel="${2:-0}" kernel_strip="${3:-none}" args=("$arch" system-volume)
	local staged_kernel=""
	if [[ "$with_kernel" == "1" ]]; then
		local source_kernel="$BUILD_DIR/cargo/kernel/$(target_triple "$arch")/debug/kernel"
		staged_kernel="$BUILD_DIR/boot/kernel-volume-$arch-$kernel_strip.$$"
		local strip_tool="objcopy"
		[[ "$arch" == "x86_64" ]] || strip_tool="llvm-strip"
		KERNEL_STRIP_TOOL="$strip_tool" "$SRC_DIR/tools/stage-kernel.sh" \
			"$kernel_strip" "$source_kernel" "$staged_kernel"
		args+=("--with-kernel=$staged_kernel")
	fi
	step "volume ($arch)"
	local status=0
	mkdir -p "$SRC_DIR/../.build/state"
	(cd "$SRC_DIR/tools/mkpackages" && cargo run --quiet -- "${args[@]}") || status=$?
	[[ -z "$staged_kernel" ]] || rm -f -- "$staged_kernel"
	return "$status"
}

PARTS_ALL="sdk libs user kernel loader packages volume"

help() {
	usage_and_exit <<EOF
usage: build.sh [--arch ARCH[,ARCH...]] [--part PART[,PART...]] [-- CARGO ARGS...]

Builds the system. With no arguments: every part, for x86_64.

  --arch ARCH   x86_64 | aarch64 | riscv64 | all          (default: x86_64)
  --part PART   $PARTS_ALL | all       (default: all)
  --            everything after this is passed to cargo
  --kernel-on-volume
                put the kernel on the system volume - what ./image.sh does for shipping media. A
                test run needs it absent, because the suite boots its own kernel from the ESP and
                the loader prefers the volume's. Off by default.
  --dma-mode MODE
                enforcing-required | no-iommu | harness: which DMA mode the volume's signed
                manifest declares. A shipping medium is assembled around a volume signed for the
                same value as the medium, which is why ./image.sh passes it here; harness signs
                a manifest that declares none, for the test and development media whose boots
                take the mode from the harness carrier. Default: harness.
  --rebuild     ignore every build cache and produce each artifact again. The caches are keyed on
                sources, tools and manifest, so this is for when the KEY is what you doubt - a
                changed compiler that reports the same version, a half-written cache entry - and
                not something a normal build needs.
  -h, --help    this text

parts, in the order they are built:
  sdk        the SDK component (wasm32)
  libs       the shared libraries
  user       the userspace programs (implies libs)
  kernel     the kernel ELF
  loader     the system's own UEFI loader
  packages   the boot packages, from an already-built userspace
  volume     the LiberFS system volume the loader reads everything from

examples:
  ./build.sh                          # everything, x86_64
  ./build.sh --arch all               # everything, all three architectures
  ./build.sh --part kernel            # just the kernel
  ./build.sh --arch riscv64 --part user,packages
  ./build.sh --part user -- -p imgconv
  ./build.sh --rebuild                # ignore the caches and build every artifact again

The volume carries the kernel only when 'kernel' and 'volume' are both built, so a partial build
never replaces a shipping volume's kernel with a stale one.
EOF
}

archs=()
parts=()
cargo_args=()
kernel_on_volume=0

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
	--part)
		[[ $# -ge 2 ]] || die "--part needs a value"
		picked_raw="$(parse_list "$2" part "$PARTS_ALL")"
		mapfile -t picked <<<"$picked_raw"
		parts+=("${picked[@]}")
		shift 2
		;;
	--kernel-on-volume)
		kernel_on_volume=1
		shift
		;;
	--dma-mode)
		[[ $# -ge 2 ]] || die "--dma-mode needs a value"
		case "$2" in
		enforcing-required | no-iommu | harness) ;;
		*) die "--dma-mode takes enforcing-required, no-iommu or harness, got '$2'" ;;
		esac
		# EXPORTED for `mkpackages`, which signs the volume's manifest and reads it there.
		export LIBER_DMA_MODE="$2"
		shift 2
		;;
	--stall)
		[[ $# -ge 2 ]] || die "--stall needs a number of seconds"
		BUILD_STALL_FLAG="$2"
		shift 2
		;;
	--rebuild)
		# EXPORTED rather than passed along: `build-shared.sh` and `build-exe-start.sh` both read
		# it, and the parts below call them through several layers. The flag is what a person
		# types; the variable is how it travels.
		export LIBER_IMAGE_REBUILD=1
		shift
		;;
	--)
		shift
		cargo_args=("$@")
		break
		;;
	*) die "unexpected argument '$1' (try --help)" ;;
	esac
done

[[ ${#archs[@]} -eq 0 ]] && archs=(x86_64)
if [[ ${#parts[@]} -eq 0 ]]; then
	# shellcheck disable=SC2206
	parts=($PARTS_ALL)
fi

wants() {
	local want="$1" p
	for p in "${parts[@]}"; do [[ "$p" == "$want" ]] && return 0; done
	return 1
}

# `--` arguments only make sense for the cargo-driven parts; say so rather than ignore them.
if [[ ${#cargo_args[@]} -gt 0 ]] && ! wants user && ! wants kernel; then
	die "arguments after -- are passed to cargo, which only the 'user' and 'kernel' parts run"
fi

# A BUILD THAT STOPS MAKING PROGRESS SAYS SO WHILE IT IS STILL STOPPED.
#
# The guest runner has had a stall watchdog for months and the build had nothing: a build that wedged
# on a lock nobody released, or on a compiler that did not return, was discovered by a person
# eventually. This is the same design with the one difference the subject forces.
#
# IT WATCHES A MARK, NOT A LOG, because `build.sh` has no log of its own - its output goes wherever
# the caller sent it, and a watchdog cannot count lines it cannot see. Each step stamps a file with
# its own name; the watchdog compares that file's age against the window. It is the same "count the
# runner's OWN completions" rule the guest watchdog states, arrived at from the other side.
#
# AND IT REPORTS RATHER THAN KILLS. A guest can be shot down and restarted; a build killed mid-link
# leaves artifacts nobody can reason about, and the person watching is the one who should decide. The
# whole defect being fixed is not knowing - so saying it is the whole fix.
# THE WINDOW, VALIDATED BEFORE ANYTHING IS CREATED FOR IT.
#
# `--stall` overrides `BUILD_STALL`, and an invalid value ends the run through the ordinary failed
# verdict rather than being silently read as zero - which is what `((age >= BUILD_STALL))` did with a
# word: it compared against nothing and never reported. An invalid value in the ENVIRONMENT does not
# defeat a valid explicit flag, because the flag is the one the person typed.
build_stall_window() {
	local raw="$1" source="$2"
	case "$raw" in
	"" | *[!0-9]*) die "$source must be a whole number of seconds, not '$raw'" ;;
	esac
	# Normalised as decimal, so `0900` is nine hundred and not an octal surprise.
	local value=$((10#$raw))
	((value >= 0 && value <= 2147483647)) || die "$source is out of range: $raw"
	printf '%s' "$value"
}
if [[ -n "${BUILD_STALL_FLAG:-}" ]]; then
	BUILD_STALL="$(build_stall_window "$BUILD_STALL_FLAG" "--stall")"
else
	BUILD_STALL="$(build_stall_window "${BUILD_STALL:-900}" "BUILD_STALL")"
fi

# A BUILD THAT STOPS MAKING PROGRESS SAYS SO WHILE IT IS STILL STOPPED.
#
# The guest runner has had a stall watchdog for months and the build had nothing: a build that wedged
# on a lock nobody released, or on a compiler that did not return, was discovered by a person
# eventually. This is the same design with the one difference the subject forces.
#
# IT COUNTS THIS SCRIPT'S OWN STEPS, NOT OUTPUT, because `build.sh` has no log of its own - its output
# goes wherever the caller sent it - and because a compiler that prints for an hour without finishing
# is exactly the stall being watched for. Each `step` writes a GENERATION, a timestamp and a label
# together; the observer compares that timestamp against the window. It is the same "count the
# runner's OWN completions" rule the guest watchdog states, arrived at from the other side.
#
# THE GENERATION IS WHAT MAKES A REPEATED LABEL A NEW STEP. Two steps with the same name, or two
# within one clock tick, are two steps - and an observer that compared only the label or only the
# second would have treated the second as a continuation of the first and never rearmed.
#
# AND IT REPORTS RATHER THAN KILLS. A guest can be shot down and restarted; a build killed mid-link
# leaves artifacts nobody can reason about, and the person watching is the one who should decide. The
# whole defect being fixed is not knowing - so saying it is the whole fix.
BUILD_STEP_MARK="$BUILD_DIR/state/build-step.$$"
BUILD_STEP_GENERATION=0
mkdir -p "$BUILD_DIR/state"

# Announce a step AND stamp it, so the two cannot drift: a step that printed and did not stamp would
# be a step the observer thinks never started.
#
# WRITTEN WHOLE AND RENAMED, so the observer never reads half a record. It reads the file while the
# build writes it, and a partial line there is an age of "now" over a label that is not there.
step() {
	note "$*"
	((BUILD_STEP_GENERATION += 1))
	if ((BUILD_STALL > 0)); then
		printf '%s %s %s\n' "$BUILD_STEP_GENERATION" "$(date +%s)" "$*" >"$BUILD_STEP_MARK.tmp" 2>/dev/null &&
			mv -f "$BUILD_STEP_MARK.tmp" "$BUILD_STEP_MARK" 2>/dev/null || true
	fi
}

remove_step_mark() {
	rm -f "$BUILD_STEP_MARK" "$BUILD_STEP_MARK.tmp"
}

# Stop the observer and its sleeper, and reap both - before the mark goes, so the observer never
# reads a mark that is being removed under it.
#
# REGISTERED BEFORE THE OBSERVER IS LAUNCHED, which is the rule that makes a failure between the two
# clean up anyway. Only owned PIDs: this tree has killed its own gate by matching an executable name.
BUILD_OBSERVER_PID=""
stop_build_observer() {
	[[ -n "${BUILD_OBSERVER_PID:-}" ]] || return 0
	kill -TERM "$BUILD_OBSERVER_PID" 2>/dev/null || true
	wait "$BUILD_OBSERVER_PID" 2>/dev/null || true
	BUILD_OBSERVER_PID=""
}
register_run_cleanup stop_build_observer
register_run_cleanup remove_step_mark

if ((BUILD_STALL > 0)); then
	# The setup label, so a build that stalls before its first step still names where it stopped.
	step_mark_setup() {
		printf '%s %s %s\n' 0 "$(date +%s)" "setting up" >"$BUILD_STEP_MARK.tmp" 2>/dev/null &&
			mv -f "$BUILD_STEP_MARK.tmp" "$BUILD_STEP_MARK" 2>/dev/null || true
	}
	step_mark_setup
	(
		# THE OBSERVER OWNS ITS SLEEPER AND WAITS INTERRUPTIBLY. `while sleep 30` left a thirty-second
		# sleep running after the build finished, so the pipe stayed open and a caller reading the
		# build's output waited for a sleeper nobody needed. This one is stopped and reaped at once.
		sleeper=""
		stop_sleeper() {
			[[ -n "$sleeper" ]] || return 0
			kill -TERM "$sleeper" 2>/dev/null || true
			wait "$sleeper" 2>/dev/null || true
			sleeper=""
		}
		trap 'stop_sleeper; exit 0' TERM INT HUP QUIT
		parent=$$
		reported_generation=""
		while :; do
			# POLLED AT ONE SECOND, because the report has to arrive by the window plus the allowance
			# and a thirty-second poll could not: it reported up to thirty seconds late by
			# construction.
			sleep 1 &
			sleeper=$!
			wait "$sleeper" 2>/dev/null || true
			sleeper=""
			kill -0 "$parent" 2>/dev/null || break
			[[ -f "$BUILD_STEP_MARK" ]] || break
			read -r generation stamp label <"$BUILD_STEP_MARK" 2>/dev/null || continue
			[[ -n "$generation" && -n "$stamp" ]] || continue
			age=$(($(date +%s) - stamp))
			if ((age >= BUILD_STALL)); then
				# ONCE PER EPISODE, and a new generation is a new episode even when the progress
				# happened between two polls.
				if [[ "$reported_generation" != "$generation" ]]; then
					echo "build.sh: STALLED - no step has started for ${age}s; the last one was '${label:-unknown}'" >&2
					reported_generation="$generation"
				fi
			fi
		done
		stop_sleeper
	) &
	BUILD_OBSERVER_PID=$!
fi

wants sdk && ensure step_sdk

for arch in "${archs[@]}"; do
	wants libs && ensure step_libs "$arch"
	wants user && ensure step_user "$arch"
	wants kernel && ensure step_kernel "$arch"
	wants loader && ensure step_loader "$arch"
	wants packages && ensure step_packages "$arch"
	# The volume carries a kernel only when an IMAGE is being assembled, never as a side effect of
	# building.
	#
	# A shipping medium wants its kernel on the volume - that is what the volume is for. A test run
	# wants it absent, because the suite boots a different kernel staged on the ESP and the loader
	# prefers the volume's. Putting it there during an ordinary build made `./test.sh` boot the
	# SHIPPING kernel into an interactive shell and time out after fifteen minutes.
	wants volume && ensure step_volume "$arch" "$kernel_on_volume" "${LIBER_KERNEL_STRIP:-none}"
	# Record that a build ran over the sources as they stand now.
	#
	# `./test.sh` refuses a build older than its sources, which is right - it caught two stale
	# runs in one afternoon - but it read that age off the built artifacts, and those do not
	# always get rewritten. `mkpackages` skips a write whose bytes are unchanged (`write_if_changed`),
	# so a kernel-only edit, or a `git checkout` that restores a file to what was already built,
	# leaves the sources newer than an image that is byte-for-byte current. The suite then refused
	# to run and asked for the very build that had just succeeded.
	#
	# The stamp says what the artifacts cannot: a build covered these sources. One file per
	# architecture AND per part, so `--part loader` cannot vouch for a userspace it never touched -
	# a single stamp listing this run's parts would erase the record of the build before it.
	mkdir -p "$BUILD_DIR/state"
	# Each part records the digest of the sources IT reads, so a loader-only edit does not
	# invalidate a userspace that no byte of it touched, and vice versa.
	#
	# AND THE VOLUME PART HAS TWO STAMPS, because it has two shapes. The volume with a kernel on it
	# and the volume without one became two artifacts with two names; the RECORD of what was built
	# stayed one file, so `./image.sh` wrote the stamp and `./test.sh` read it and concluded the
	# userspace was current - while the shape the suite actually boots, the one without a kernel, may
	# not have been rebuilt since the sources changed. Edit userspace, `./image.sh`, `./test.sh`: the
	# staleness check passed and the suite booted an old volume. That is the original defect with a
	# different subject, and naming the artifacts apart was not enough while their receipts shared a
	# name.
	volume_stamp="volume"
	[[ "$kernel_on_volume" == "1" ]] || volume_stamp="volume-test"
	for part in "${parts[@]}"; do
		case "$part" in
		loader) printf '%s\n' "$(source_digest "${LOADER_SOURCES[@]}")" >"$BUILD_DIR/state/built-$arch-$part" ;;
		volume) printf '%s\n' "$(source_digest "${VOLUME_SOURCES[@]}")" >"$BUILD_DIR/state/built-$arch-$volume_stamp" ;;
		*) printf '%s\n' "$(source_digest "${VOLUME_SOURCES[@]}")" >"$BUILD_DIR/state/built-$arch-$part" ;;
		esac
	done
done

note "built: ${parts[*]} for ${archs[*]}"
