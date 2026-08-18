#!/usr/bin/env bash
# Stamp the non-kernel inputs that the kernel test image reuses between focused runs.
set -euo pipefail

STAMP_FORMAT="libersystem-test-preflight-v3"
MODE="${1:-}"
ARCH="${2:-}"
SCOPE="${TEST_PREFLIGHT_SCOPE:-narrow}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REPO_ROOT="$(cd "$ROOT/.." && pwd)"
# Overridable so the gate can be tested against deliberately broken producers without touching the
# stamps a real run depends on. It is a cache location, not a boundary: pointing it elsewhere loses
# the fast path for that run and proves nothing false, because `check` compares against whatever it
# finds there and refuses when there is nothing.
STAMP_DIR="${TEST_PREFLIGHT_STAMP_DIR:-$REPO_ROOT/.build/state/preflight}"

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

# THIS GATE'S ONE JOB IS TO BE HARD TO FOOL, and it was not.
#
# Every producer below used to run inside a command substitution used as a `printf` ARGUMENT, or
# behind a `< <(...)` process substitution. Bash propagates neither status: `printf` succeeding
# replaces the failed producer, and a `while read` loop does not see its producer's exit code.
# `set -euo pipefail` does not reach into either. So a failing `git ls-files` became the SHA-256 of
# an empty path stream, a failing `cargo -V` became the SHA-256 of partial text - and because the
# check side computed its record exactly the same way under the same degraded conditions, the two
# matched and the gate reported the inputs current. It approved a cached image without having
# inventoried the source or toolchain it claims to bind.
#
# The rule from here down: capture each producer into a file, CHECK ITS STATUS, then hash the file.
# Nothing is hashed that has not been shown to be complete, and every count is recorded so an empty
# inventory cannot look like a populated one.
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/libersystem-preflight.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

fail() {
	echo "test preflight: $1" >&2
	exit 1
}

# Run a producer, requiring it to succeed, and leave its output in `$1`.
#
# `exit` from here only works because this is called directly and never inside a `$(...)`: an exit
# in a command substitution ends the SUBSHELL and hands the parent a status the parent then has to
# check. That is the same trap this whole file was built on, so every command substitution below
# carries its own `|| fail`.
produce() {
	local into="$1"
	shift
	if ! "$@" >"$into" 2>"$into.err"; then
		echo "test preflight: producer failed: $*" >&2
		sed 's/^/  /' <"$into.err" >&2 || true
		exit 1
	fi
}

# How many NUL-separated entries a listing holds.
count_entries() {
	tr -dc '\0' <"$1" | wc -c
}

# Digest a NUL-separated path list held in a FILE, and count it. One batched hash: hashing this
# tree costs milliseconds, but a process per file costs seconds.
#
# The count goes into the record beside the digest. An empty stream has a perfectly good digest -
# always the same one - so a stamp written from a broken inventory and a check made under the same
# breakage agreed, which is precisely the false green. A count of zero is now visibly zero.
inventory_digest() {
	local listing="$1" count digest
	count="$(count_entries "$listing")" || return 1
	digest="$(cd "$REPO_ROOT" && xargs -0 -r sha256sum <"$listing" | LC_ALL=C sort | sha256sum | awk '{print $1}')" || return 1
	printf 'count=%s sha256=%s' "$count" "$digest"
}

# Every named path is REQUIRED. A missing one used to be silently omitted, which represents "this
# input is gone" as "this class has one fewer file" - and the two records still compared equal as
# long as it stayed gone.
existing_paths() {
	local path
	for path in "$@"; do
		if [[ ! -f "$REPO_ROOT/$path" ]]; then
			echo "test preflight: required input is missing: $path" >&2
			return 1
		fi
		printf '%s\0' "$path"
	done
}

# The tracked and untracked source inventory, from a `git ls-files` whose status is checked.
image_paths() {
	local listing="$TMP_DIR/ls-files" path
	produce "$listing" git -C "$REPO_ROOT" ls-files -co --exclude-standard -z -- src
	while IFS= read -r -d '' path; do
		case "$path" in
		src/kernel/*) continue ;;
		esac
		[[ -f "$REPO_ROOT/$path" ]] || continue
		if [[ "$SCOPE" == "narrow" ]] && narrow_excluded "$path"; then continue; fi
		printf '%s\0' "$path"
	done <"$listing"
}

toolchain_version() {
	cd "$ROOT/kernel" && cargo -V && rustc -vV
}

toolchain_digest() {
	local into="$TMP_DIR/toolchain"
	produce "$into" toolchain_version
	# Nonempty as well as successful: a tool that exits 0 and prints nothing is not a toolchain
	# identity, and its digest is the same constant for every such tool.
	[[ -s "$into" ]] || fail 'the toolchain identity is empty'
	sha256sum <"$into" | awk '{print $1}'
}

# The whole record, written to the file named by `$1`. It is a file rather than stdout because the
# caller has to be able to tell a complete record from a truncated one, and a pipeline cannot say.
current_state() {
	local out="$1"
	local product="$TMP_DIR/product" kernel_build="$TMP_DIR/kernel-build" image="$TMP_DIR/image"
	produce "$product" existing_paths product.conf
	produce "$kernel_build" existing_paths "${KERNEL_BUILD_INPUTS[@]}"
	produce "$image" image_paths
	# A source tree of a handful of files is not this repository; it is a `git ls-files` that
	# answered from the wrong directory or stopped early. The exact number is not the point - that
	# it is a plausible order of magnitude is.
	local image_count
	image_count="$(count_entries "$image")" || fail 'the source inventory could not be counted'
	((image_count >= 50)) || fail "the source inventory holds $image_count files, which is not this tree"
	# EVERY VALUE INTO A CHECKED VARIABLE FIRST. A `$(...)` used as a `printf` argument reports the
	# printf's status and discards the producer's - which is how a failing digest became a
	# successful line. `exit` from inside one does not help either: it ends the subshell, and the
	# parent carries on with whatever partial text was on its standard output.
	local toolchain product_row kernel_row image_row
	toolchain="$(toolchain_digest)" || fail 'the toolchain identity could not be taken'
	product_row="$(inventory_digest "$product")" || fail 'the product inputs could not be digested'
	kernel_row="$(inventory_digest "$kernel_build")" || fail 'the kernel build inputs could not be digested'
	image_row="$(inventory_digest "$image")" || fail 'the source inventory could not be digested'
	{
		printf 'format=%s\n' "$STAMP_FORMAT"
		printf 'arch=%s\n' "$ARCH"
		printf 'scope=%s\n' "$SCOPE"
		printf 'narrow-excludes=%s\n' "${NARROW_EXCLUDES[*]}"
		printf 'class=toolchain sha256=%s\n' "$toolchain"
		printf 'class=product %s\n' "$product_row"
		printf 'class=kernel-build %s\n' "$kernel_row"
		printf 'class=image %s\n' "$image_row"
	} >"$out"
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
		# `current_state` exits the script on any producer failure, so reaching the rename means
		# every class completed. A partial record is never published: the previous valid stamp
		# stays, or there is none.
		current_state "$temporary"
		mv "$temporary" "$stamp"
	done
	echo "test preflight: prepared $ARCH"
	exit 0
fi

stamp="$STAMP_DIR/$ARCH.$SCOPE.sha256"
# Into a checked file, then read. This was `mapfile -t actual < <(current_state)`, and a process
# substitution hands its reader no exit status at all - so the check side reproduced the write
# side's blindness exactly, and two degraded records comparing equal was reported as current.
current_state "$TMP_DIR/actual"
mapfile -t actual <"$TMP_DIR/actual"

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
