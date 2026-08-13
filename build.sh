#!/usr/bin/env bash
# Build the system, or the parts of it you name.
#
# `--part` names a CLOSED SET. It deliberately does not name individual libraries or executables:
# that would be a second source of truth beside the manifest and it would drift, exactly as
# INSTALL.md drifted from the recipes within hours. Cargo already selects at that granularity, so
# finer choices pass through: `./build.sh --part user -- -p imgconv`.

SCRIPT_NAME=build.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

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
	note "sdk"
	# `--workspace`, because `src/sdk` is the SDK LIBRARY now and the component is the example
	# beside it. Without it cargo builds the root package only and the staged `.wasm` is whatever
	# the last build left behind.
	(cd "$SRC_DIR/sdk" && cargo build --release --target wasm32-unknown-unknown --workspace)
}

step_libs() {
	local arch="$1"
	note "libs ($arch)"
	local pairs=()
	mapfile -t pairs < <(shared_libs)
	[[ ${#pairs[@]} -gt 0 ]] || die "the manifest lists no shared libraries"
	(cd "$SRC_DIR" && tools/build-shared.sh "$(target_triple "$arch")" "${pairs[@]}")
}

step_user() {
	local arch="$1" flag
	ensure step_libs "$arch"
	flag="$(target_flag "$arch")"
	note "user ($arch)"
	# shellcheck disable=SC2046,SC2086
	(cd "$(source_path system_manager)" && cargo build $flag)
	# shellcheck disable=SC2046,SC2086
	(cd "$(source_path services)" && cargo build $flag $(dev_features))
	# shellcheck disable=SC2046,SC2086
	(cd "$(source_path storage)" && cargo build $flag)
	# shellcheck disable=SC2046,SC2086
	(cd "$(source_path drivers)" && cargo build $flag $(dev_features))
}

step_kernel() {
	local arch="$1" flag
	flag="$(target_flag "$arch")"
	note "kernel ($arch)"
	# shellcheck disable=SC2086
	(cd "$SRC_DIR/kernel" && cargo build $flag)
}

step_loader() {
	local arch="$1"
	note "loader ($arch)"
	case "$arch" in
	x86_64) (cd "$SRC_DIR/loader" && cargo build) ;;
	# aarch64 and riscv64 build their EFI images differently enough that a shared body would be a
	# branch either way; the riscv64 one is assembled by hand from a linker script and objcopy.
	*) (cd "$SRC_DIR" && just "loader-$arch") ;;
	esac
}

# Assemble the boot packages from an ALREADY-BUILT userspace. Deliberately without a `user`
# dependency: a packaging step that quietly builds what it is missing cannot tell you something
# was missing.
step_packages() {
	local arch="$1"
	note "packages ($arch)"
	(cd "$SRC_DIR/tools/mkpackages" && cargo run --quiet -- "$arch")
}

# The system volume, and the kernel goes on it only when asked for by PATH.
#
# It used to read `.build/boot/kernel`, a slot every image builder writes and nobody owns, so the
# volume took whatever the previous recipe had left there - and a disk image built after a test run
# carried the TEST kernel and booted into the suite. Naming the file removes the slot from the path.
step_volume() {
	local arch="$1" with_kernel="${2:-0}" args=("$arch" system-volume)
	if [[ "$with_kernel" == "1" ]]; then
		args+=("--with-kernel=$BUILD_DIR/cargo/kernel/$(target_triple "$arch")/debug/kernel")
	fi
	note "volume ($arch)"
	(cd "$SRC_DIR/tools/mkpackages" && cargo run --quiet -- "${args[@]}")
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
	# A shipping medium wants its kernel on the volume - that is the point of P02M0108. A test run
	# wants it absent, because the suite boots a different kernel staged on the ESP and the loader
	# prefers the volume's. Putting it there during an ordinary build made `./test.sh` boot the
	# SHIPPING kernel into an interactive shell and time out after fifteen minutes.
	wants volume && ensure step_volume "$arch" "$kernel_on_volume"
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
	for part in "${parts[@]}"; do
		case "$part" in
		loader) printf '%s\n' "$(source_digest loader)" >"$BUILD_DIR/state/built-$arch-$part" ;;
		*) printf '%s\n' "$(source_digest "${VOLUME_SOURCES[@]}")" >"$BUILD_DIR/state/built-$arch-$part" ;;
		esac
	done
done

note "built: ${parts[*]} for ${archs[*]}"
