#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
first="${1:-static}"
kind="static"
mode="all"
artifact=""
backup=""
failure_log=""
restore_log=""

case "$first" in
static | undeclared-edge | duplicate-edge)
	kind="$first"
	mode="${2:-all}"
	;;
all | x86_64 | aarch64 | riscv64) mode="$first" ;;
*)
	echo "usage: $0 [static|undeclared-edge|duplicate-edge] [all|x86_64|aarch64|riscv64]" >&2
	exit 2
	;;
esac

command -v dd >/dev/null
command -v flock >/dev/null
command -v llvm-readelf >/dev/null
command -v od >/dev/null
command -v sha256sum >/dev/null
command -v timeout >/dev/null
mkdir -p "$root/boot/.build"
exec 8>"$root/boot/.build/image-build-x86_64-unknown-none.lock"
exec 9>"$root/boot/.build/image-build-aarch64-unknown-none.lock"
exec 10>"$root/boot/.build/image-build-riscv64gc-unknown-none-elf.lock"
flock 8
flock 9
flock 10

restore_artifact() {
	if [[ -n "$artifact" && -n "$backup" && -f "$backup" ]]; then cp "$backup" "$artifact"; fi
}

cleanup() {
	local status=$?
	restore_artifact
	rm -f "$backup" "$failure_log" "$restore_log"
	exit "$status"
}
trap cleanup EXIT

build_kernel() {
	local target="$1"
	local output="$2"
	local status
	pushd "$root/kernel" >/dev/null
	if [[ "$target" == x86_64-unknown-none ]]; then
		if timeout 600 cargo build >"$output" 2>&1; then status=0; else status=$?; fi
	else
		if timeout 600 cargo build --target "$target" >"$output" 2>&1; then status=0; else status=$?; fi
	fi
	popd >/dev/null
	return "$status"
}

write_u64_le() {
	local value="$1"
	local offset="$2"
	local byte byte_index
	for ((byte_index = 0; byte_index < 8; byte_index++)); do
		byte=$((value & 255))
		printf '%b' "\\$(printf '%03o' "$byte")" | dd of="$artifact" bs=1 seek="$((offset + byte_index))" conv=notrunc status=none
		value=$((value >> 8))
	done
}

inject_artifact() {
	case "$kind" in
	static)
		printf '\002\000' | dd of="$artifact" bs=1 seek=16 conv=notrunc status=none
		;;
	undeclared-edge)
		local -a offsets=()
		mapfile -t offsets < <(grep -aob 'lsrt.lslib' "$artifact" | cut -d: -f1)
		if [[ "${#offsets[@]}" != 1 ]]; then
			echo "image-injection-check: $kind expected one lsrt.lslib entry in $artifact" >&2
			return 1
		fi
		printf 'wire.lslib' | dd of="$artifact" bs=1 seek="${offsets[0]}" conv=notrunc status=none
		;;
	duplicate-edge)
		local dynamic_offset_hex dynamic_size_hex dynamic_offset dynamic_size value_offset word_index value
		local -a words=()
		local -a needed_indices=()
		local -a needed_values=()
		dynamic_offset_hex="$(llvm-readelf -lW "$artifact" | awk '$1 == "DYNAMIC" {print $2; exit}')"
		dynamic_size_hex="$(llvm-readelf -lW "$artifact" | awk '$1 == "DYNAMIC" {print $5; exit}')"
		if [[ -z "$dynamic_offset_hex" || -z "$dynamic_size_hex" ]]; then
			echo "image-injection-check: $kind found no PT_DYNAMIC segment in $artifact" >&2
			return 1
		fi
		dynamic_offset=$((dynamic_offset_hex))
		dynamic_size=$((dynamic_size_hex))
		mapfile -t words < <(od -An -v -tu8 -j "$dynamic_offset" -N "$dynamic_size" "$artifact" | tr -s ' ' '\n' | sed '/^$/d')
		for ((word_index = 0; word_index + 1 < ${#words[@]}; word_index += 2)); do
			if [[ "${words[word_index]}" == 1 ]]; then
				needed_indices+=("$((word_index / 2))")
				needed_values+=("${words[word_index + 1]}")
			fi
		done
		if [[ "${#needed_indices[@]}" -lt 2 ]]; then
			echo "image-injection-check: $kind expected two DT_NEEDED entries in $artifact" >&2
			return 1
		fi
		value_offset=$((dynamic_offset + needed_indices[1] * 16 + 8))
		value="${needed_values[0]}"
		write_u64_le "$value" "$value_offset"
		;;
	esac
}

rejection_pattern() {
	case "$kind" in
	static) printf '%s\n' 'dynamic echo is not ET_DYN' ;;
	undeclared-edge) printf '%s\n' 'dynamic echo DT_NEEDED providers differ from the manifest' ;;
	duplicate-edge) printf '%s\n' 'dynamic dyn_probe repeats a DT_NEEDED provider' ;;
	esac
}

check_target() {
	local label="$1"
	local target="$2"
	local volume_name="$3"
	local volume="$root/boot/.build/$volume_name"
	local artifact_hash before after_failure after_restore
	if [[ "$kind" == duplicate-edge ]]; then
		artifact="$root/user/dyn_probe/shared/$target/dyn_probe"
	else
		artifact="$root/user/tools/shared/$target/echo"
	fi
	[[ -f "$artifact" ]] || {
		echo "image-injection-check: missing staged $label artifact for $kind" >&2
		return 1
	}
	[[ -f "$volume" ]] || {
		echo "static-image-check: missing staged $label volume package" >&2
		return 1
	}
	backup="$(mktemp)"
	failure_log="$(mktemp)"
	restore_log="$(mktemp)"
	cp "$artifact" "$backup"
	artifact_hash="$(sha256sum "$artifact" | awk '{print $1}')"
	before="$(sha256sum "$volume" | awk '{print $1}')"
	inject_artifact
	if build_kernel "$target" "$failure_log"; then
		echo "image-injection-check: $label $kind injection unexpectedly built" >&2
		return 1
	fi
	if ! grep -q "$(rejection_pattern)" "$failure_log"; then
		echo "image-injection-check: $label did not reject the injected $kind artifact" >&2
		return 1
	fi
	after_failure="$(sha256sum "$volume" | awk '{print $1}')"
	if [[ "$before" != "$after_failure" ]]; then
		echo "image-injection-check: $label rewrote volume.pkg after rejecting $kind" >&2
		return 1
	fi
	restore_artifact
	if [[ "$(sha256sum "$artifact" | awk '{print $1}')" != "$artifact_hash" ]]; then
		echo "image-injection-check: $label failed to restore the dynamic artifact" >&2
		return 1
	fi
	build_kernel "$target" "$restore_log"
	after_restore="$(sha256sum "$volume" | awk '{print $1}')"
	if [[ "$before" != "$after_restore" ]]; then
		echo "image-injection-check: $label rebuilt a different volume after artifact restoration" >&2
		return 1
	fi
	rm -f "$backup" "$failure_log" "$restore_log"
	artifact=""
	backup=""
	failure_log=""
	restore_log=""
	printf 'image-injection-check: %s %s passed\n' "$kind" "$label"
}

case "$mode" in
all)
	check_target x86_64 x86_64-unknown-none volume.pkg
	check_target aarch64 aarch64-unknown-none volume-aarch64.pkg
	check_target riscv64 riscv64gc-unknown-none-elf volume-riscv64.pkg
	;;
x86_64) check_target x86_64 x86_64-unknown-none volume.pkg ;;
aarch64) check_target aarch64 aarch64-unknown-none volume-aarch64.pkg ;;
riscv64) check_target riscv64 riscv64gc-unknown-none-elf volume-riscv64.pkg ;;
*)
	echo "usage: $0 [static|undeclared-edge|duplicate-edge] [all|x86_64|aarch64|riscv64]" >&2
	exit 2
	;;
esac

echo "image-injection-check: $kind $mode passed"
