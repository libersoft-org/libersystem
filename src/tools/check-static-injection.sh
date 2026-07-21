#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
mode="${1:-all}"
artifact=""
backup=""
failure_log=""
restore_log=""

command -v dd >/dev/null
command -v flock >/dev/null
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

check_target() {
	local label="$1"
	local target="$2"
	local volume_name="$3"
	local volume="$root/boot/.build/$volume_name"
	local artifact_hash before after_failure after_restore
	artifact="$root/user/tools/shared/$target/echo"
	[[ -f "$artifact" ]] || {
		echo "static-image-check: missing staged $label echo artifact" >&2
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
	printf '\002\000' | dd of="$artifact" bs=1 seek=16 conv=notrunc status=none
	if build_kernel "$target" "$failure_log"; then
		echo "static-image-check: $label static executable injection unexpectedly built" >&2
		return 1
	fi
	if ! grep -q 'dynamic echo is not ET_DYN' "$failure_log"; then
		echo "static-image-check: $label did not reject the injected ET_EXEC artifact" >&2
		return 1
	fi
	after_failure="$(sha256sum "$volume" | awk '{print $1}')"
	if [[ "$before" != "$after_failure" ]]; then
		echo "static-image-check: $label rewrote volume.pkg after rejecting ET_EXEC" >&2
		return 1
	fi
	restore_artifact
	if [[ "$(sha256sum "$artifact" | awk '{print $1}')" != "$artifact_hash" ]]; then
		echo "static-image-check: $label failed to restore the dynamic artifact" >&2
		return 1
	fi
	build_kernel "$target" "$restore_log"
	after_restore="$(sha256sum "$volume" | awk '{print $1}')"
	if [[ "$before" != "$after_restore" ]]; then
		echo "static-image-check: $label rebuilt a different volume after artifact restoration" >&2
		return 1
	fi
	rm -f "$backup" "$failure_log" "$restore_log"
	artifact=""
	backup=""
	failure_log=""
	restore_log=""
	printf 'static-image-check: %s passed\n' "$label"
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
	echo "usage: $0 [all|x86_64|aarch64|riscv64]" >&2
	exit 2
	;;
esac

echo "static-image-check: $mode passed"
