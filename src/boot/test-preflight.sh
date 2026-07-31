#!/usr/bin/env bash
# Stamp the non-kernel inputs that the kernel test image reuses between focused runs.
set -euo pipefail

STAMP_FORMAT="libersystem-test-preflight-v2"
MODE="${1:-}"
ARCH="${2:-}"
SCOPE="${TEST_PREFLIGHT_SCOPE:-narrow}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REPO_ROOT="$(cd "$ROOT/.." && pwd)"
STAMP_DIR="$REPO_ROOT/.build/state/preflight"

usage() {
	echo "usage: test-preflight.sh <write|check> <x86_64|aarch64|riscv64> [--scope <narrow|full>]" >&2
	exit 2
}

[[ $# -ge 2 ]] || usage
shift 2
while [[ $# -gt 0 ]]; do
	case "$1" in
	--scope)
		[[ $# -ge 2 ]] || usage
		SCOPE="$2"
		shift 2
		;;
	*) usage ;;
	esac
done

[[ "$MODE" == "write" || "$MODE" == "check" ]] || usage
case "$ARCH" in
x86_64 | aarch64 | riscv64) ;;
*) usage ;;
esac
case "$SCOPE" in
narrow | full) ;;
*) usage ;;
esac

case "$ARCH" in
x86_64) PREPARE_RECIPE="test-preflight" ;;
aarch64) PREPARE_RECIPE="test-preflight-aarch64" ;;
riscv64) PREPARE_RECIPE="test-preflight-riscv64" ;;
esac

for command in cargo git rustc sha256sum xargs; do
	command -v "$command" >/dev/null || {
		echo "test preflight: required command not found: $command" >&2
		exit 1
	}
done

# Host-only tooling. None of these crates is staged into the system image, linked
# into a guest artifact or read while one is built, so changing them cannot alter
# what the test image boots. Generated bindings under src/idl reach an artifact
# only through their checked-in output, which `just gen-check` owns. Each entry
# narrows what invalidates the fast path, so keep the list short and explicit; it
# is recorded in the stamp, and editing it invalidates every existing stamp.
NARROW_EXCLUDES=(
	'src/idl/*'
	'src/tools/*-conformance/*'
	'src/tools/audio-bench/*'
	'src/tools/image-bench/*'
	'src/tools/image-mutate/*'
	'src/tools/lsidl-gen/*'
	'src/tools/normalize-ogg.rs'
	'src/tools/normalize-ogg/*'
)

# Explicit inputs outside the image tree. The kernel sources themselves are Cargo's
# to track; these files decide how the test image is assembled, not what it contains.
KERNEL_BUILD_INPUTS=(
	src/kernel/Cargo.toml
	src/kernel/build.rs
	src/kernel/rust-toolchain.toml
	src/kernel/.cargo/config.toml
)

narrow_excluded() {
	local path="$1" pattern
	for pattern in "${NARROW_EXCLUDES[@]}"; do
		# shellcheck disable=SC2053 # pattern is a glob on purpose
		[[ "$path" == $pattern ]] && return 0
	done
	return 1
}

# Digest a NUL-separated path list read from stdin. One batched hash: hashing this
# tree costs milliseconds, but a process per file costs seconds.
inventory_digest() {
	(cd "$REPO_ROOT" && xargs -0 -r sha256sum) | LC_ALL=C sort | sha256sum | awk '{print $1}'
}

existing_paths() {
	local path
	for path in "$@"; do
		[[ -f "$REPO_ROOT/$path" ]] && printf '%s\0' "$path"
	done
	return 0
}

image_paths() {
	local path
	while IFS= read -r -d '' path; do
		case "$path" in
		src/kernel/*) continue ;;
		esac
		[[ -f "$REPO_ROOT/$path" ]] || continue
		if [[ "$SCOPE" == "narrow" ]] && narrow_excluded "$path"; then continue; fi
		printf '%s\0' "$path"
	done < <(git -C "$REPO_ROOT" ls-files -co --exclude-standard -z -- src)
	return 0
}

toolchain_digest() {
	(cd "$ROOT/kernel" && cargo -V && rustc -vV) | sha256sum | awk '{print $1}'
}

current_state() {
	printf 'format=%s\n' "$STAMP_FORMAT"
	printf 'arch=%s\n' "$ARCH"
	printf 'scope=%s\n' "$SCOPE"
	printf 'narrow-excludes=%s\n' "${NARROW_EXCLUDES[*]}"
	printf 'class=toolchain sha256=%s\n' "$(toolchain_digest)"
	printf 'class=product sha256=%s\n' "$(existing_paths product.conf | inventory_digest)"
	printf 'class=kernel-build sha256=%s\n' "$(existing_paths "${KERNEL_BUILD_INPUTS[@]}" | inventory_digest)"
	printf 'class=image sha256=%s\n' "$(image_paths | inventory_digest)"
}

if [[ "$MODE" == "write" ]]; then
	mkdir -p "$STAMP_DIR"
	# Prepare both scopes. The fast recipes check the narrow stamp; the broad one
	# stays available for checkpoint validation without a second preparation pass.
	# The temporary carries this run's pid, because the name is what makes the publication
	# atomic against a second writer and `$stamp.tmp` is a name every run shares. Two preflights
	# overlapping - two suites started together, or one driving several architectures - had one
	# write the file the other was about to rename, and the loser died on
	#   mv: cannot stat '.../x86_64.narrow.sha256.tmp': No such file or directory
	# which says nothing about a race. The rest of the build already knows this: providers link
	# to `<name>.<pid>.candidate`, identity records come from `mktemp`, and the artifact cache
	# sweeps `*.tmp.$$`. Renaming is atomic against a READER whatever it is called; against
	# another writer it needs a name that cannot collide.
	for SCOPE in narrow full; do
		stamp="$STAMP_DIR/$ARCH.$SCOPE.sha256"
		temporary="$stamp.tmp.$$"
		current_state >"$temporary"
		mv "$temporary" "$stamp"
	done
	echo "test preflight: prepared $ARCH"
	exit 0
fi

stamp="$STAMP_DIR/$ARCH.$SCOPE.sha256"
mapfile -t actual < <(current_state)

if [[ ! -f "$stamp" ]]; then
	echo "test preflight: no prepared $ARCH $SCOPE image inputs; run just $PREPARE_RECIPE" >&2
	exit 1
fi

mapfile -t recorded <"$stamp"
if [[ "${#recorded[@]}" != "${#actual[@]}" ]]; then
	echo "test preflight: stamp layout changed; run just $PREPARE_RECIPE" >&2
	exit 1
fi
for index in "${!actual[@]}"; do
	[[ "${recorded[$index]}" == "${actual[$index]}" ]] && continue
	changed="${actual[$index]%% *}"
	case "$changed" in
	class=*) changed="${changed#class=} inputs" ;;
	narrow-excludes=*) changed="invalidation set definition" ;;
	*) changed="${changed%%=*} identity" ;;
	esac
	echo "test preflight: $changed changed; run just $PREPARE_RECIPE" >&2
	exit 1
done
echo "test preflight: $ARCH $SCOPE inputs are current"
