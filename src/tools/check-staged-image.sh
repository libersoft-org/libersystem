#!/usr/bin/env bash
# Verify that a staged system image is the one this tree produces.
#
# The build already records, per artifact, the sha256 of the bytes it staged
# (`cache/<target>/{library,executable}-<name>.sha256`), and the build's own
# audits refuse a staged path set that differs from the manifest. What neither of those
# answers is the question this script exists for: is the image sitting on disk right now
# still the image this tree would produce - without paying for a rebuild to find out.
#
# It earned its place. An aarch64 boot stalled after DeviceManager with every service
# started from the init package online and not one of the thirteen the volume provides,
# no error and no fault, and it read exactly like a regression in the aarch64 port;
# deleting the staged image and rebuilding it from nothing fixed it outright. Nothing in
# the build had said a word, because nothing was asking this question. A red test that a
# stale cache caused is indistinguishable from a red test the code caused, which makes
# every verdict only as trustworthy as a directory nobody validates.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
build_root="$root/../.build"

usage() {
	echo "usage: check-staged-image.sh [x86_64|aarch64|riscv64]..." >&2
	echo "       with no argument, every target that has a staged image is checked" >&2
	exit 2
}

target_triple() {
	case "$1" in
	x86_64) printf 'x86_64-unknown-none' ;;
	aarch64) printf 'aarch64-unknown-none' ;;
	riscv64) printf 'riscv64gc-unknown-none-elf' ;;
	*) return 1 ;;
	esac
}

manifest_json="$("$root/tools/system-manifest.sh" export-json)"

# One target: every declared artifact must be staged, hash to what was recorded when it
# was staged, and nothing undeclared may sit alongside it. Problems are counted rather
# than fatal on the first one, because "which parts of this image are wrong" is the useful
# answer and stopping at the first mismatch hides it.
check_target() {
	local name="$1"
	local triple image records problems checked
	triple="$(target_triple "$name")"
	image="$build_root/image/$triple"
	records="$build_root/cache/$triple"
	problems=0
	checked=0

	if [[ ! -d "$image" ]]; then
		echo "staged-image: $name has no staged image at $image" >&2
		return 1
	fi
	if [[ ! -d "$records" ]]; then
		echo "staged-image: $name has a staged image but no artifact records at $records" >&2
		echo "staged-image: nothing to check it against, so rebuild it: just user-$name" >&2
		return 1
	fi

	# Declared libraries and dynamic volume programs, as "<record prefix> <staged path>".
	# A program's staged file drops the `.lsexe` the manifest destination carries, the same
	# way the build stages it.
	local expected
	expected="$(
		jq -r '.libraries[] | "library-\(.name)\t\(.destination)"' <<<"$manifest_json"
		jq -r '.programs[] | select(.linkage == "dynamic" and .stage == "volume") | "executable-\(.name)\t\(.destination | sub("\\.lsexe$"; ""))"' <<<"$manifest_json"
	)"

	# Every staged file is hashed by one `sha256sum` invocation rather than one per file.
	# With 133 artifacts per target the difference is not cosmetic: spawning a process each
	# time cost 1.4 s against about a fifth of that, and this runs before every test.
	local record relative staged
	local -a hash_paths=()
	while IFS=$'\t' read -r record relative; do
		[[ -n "$record" ]] || continue
		[[ -f "$image/$relative" ]] && hash_paths+=("$image/$relative")
	done <<<"$expected"
	local -A actual_sha=()
	if ((${#hash_paths[@]} > 0)); then
		local line hash path
		while IFS= read -r line; do
			hash="${line%% *}"
			path="${line#* }"
			path="${path# }"
			actual_sha["$path"]="$hash"
		done < <(sha256sum -- "${hash_paths[@]}")
	fi

	local expected_sha
	while IFS=$'\t' read -r record relative; do
		[[ -n "$record" ]] || continue
		staged="$image/$relative"
		if [[ ! -f "$staged" ]]; then
			echo "staged-image: $name is missing $relative (the manifest declares it)" >&2
			((problems += 1))
			continue
		fi
		if [[ ! -f "$records/$record.sha256" ]]; then
			echo "staged-image: $name staged $relative with no recorded digest ($record.sha256)" >&2
			((problems += 1))
			continue
		fi
		expected_sha="$(<"$records/$record.sha256")"
		if [[ "${actual_sha[$staged]:-}" != "$expected_sha" ]]; then
			echo "staged-image: $name has stale content in $relative" >&2
			echo "staged-image:   recorded $expected_sha" >&2
			echo "staged-image:   on disk  ${actual_sha[$staged]:-<unreadable>}" >&2
			((problems += 1))
			continue
		fi
		((checked += 1))
	done <<<"$expected"

	# Anything staged that the manifest does not declare. The build's own audits cover this
	# during a build of this target; repeating it here is what makes this script answer for
	# the image on its own, including for a target that has not been built in this tree.
	local declared undeclared
	declared="$(cut -f 2 <<<"$expected" | sort)"
	undeclared="$(comm -13 <(printf '%s\n' "$declared") <(find "$image" -type f \( -name '*.lslib' -o -path "$image/bin/*" -o -path "$image/libexec/*" \) -printf '%P\n' | sort))"
	if [[ -n "$undeclared" ]]; then
		while IFS= read -r relative; do
			[[ -n "$relative" ]] || continue
			echo "staged-image: $name stages $relative, which the manifest does not declare" >&2
			((problems += 1))
		done <<<"$undeclared"
	fi

	if ((problems > 0)); then
		echo "staged-image: $name is not consistent with this tree ($problems problem(s)); rebuild it: just user-$name" >&2
		return 1
	fi
	echo "staged-image: $name consistent ($checked artifacts)"
	return 0
}

targets=()
if (($# == 0)); then
	for name in x86_64 aarch64 riscv64; do
		[[ -d "$build_root/image/$(target_triple "$name")" ]] && targets+=("$name")
	done
	if ((${#targets[@]} == 0)); then
		echo "staged-image: no staged image for any target; nothing to check" >&2
		exit 0
	fi
else
	for name in "$@"; do
		target_triple "$name" >/dev/null || usage
		targets+=("$name")
	done
fi

status=0
for name in "${targets[@]}"; do
	check_target "$name" || status=1
done
exit "$status"
