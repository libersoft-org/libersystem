#!/usr/bin/env bash
# Stamp the non-kernel inputs that the kernel test image reuses between focused runs.
set -euo pipefail

MODE="${1:-}"
ARCH="${2:-}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REPO_ROOT="$(cd "$ROOT/.." && pwd)"
STAMP_DIR="$REPO_ROOT/.build/test-preflight"

usage() {
	echo "usage: test-preflight.sh <write|check> <x86_64|aarch64|riscv64>" >&2
	exit 2
}

[[ "$MODE" == "write" || "$MODE" == "check" ]] || usage
case "$ARCH" in
x86_64 | aarch64 | riscv64) ;;
*) usage ;;
esac

case "$ARCH" in
x86_64) PREPARE_RECIPE="test-preflight" ;;
aarch64) PREPARE_RECIPE="test-preflight-aarch64" ;;
riscv64) PREPARE_RECIPE="test-preflight-riscv64" ;;
esac

for command in cargo git rustc sha256sum; do
	command -v "$command" >/dev/null || {
		echo "test preflight: required command not found: $command" >&2
		exit 1
	}
done

hash_file() {
	local path="$1"
	[[ -f "$REPO_ROOT/$path" ]] || return 0
	printf 'file=%s sha256=%s\n' "$path" "$(sha256sum "$REPO_ROOT/$path" | awk '{print $1}')"
}

input_digest() {
	{
		printf '%s\n' 'format=libersystem-test-preflight-v1'
		printf 'arch=%s\n' "$ARCH"
		printf 'kernel-cargo=%s\n' "$(cd "$ROOT/kernel" && cargo -V | sha256sum | awk '{print $1}')"
		printf 'kernel-rustc=%s\n' "$(cd "$ROOT/kernel" && rustc -vV | sha256sum | awk '{print $1}')"
		while IFS= read -r -d '' path; do
			case "$path" in
			src/kernel/*) continue ;;
			esac
			hash_file "$path"
		done < <(git -C "$REPO_ROOT" ls-files -co --exclude-standard -z -- product.conf src)
		for path in src/kernel/Cargo.toml src/kernel/build.rs src/kernel/rust-toolchain.toml src/kernel/.cargo/config.toml; do
			hash_file "$path"
		done
	} | LC_ALL=C sort | sha256sum | awk '{print $1}'
}

stamp="$STAMP_DIR/$ARCH.sha256"
actual="$(input_digest)"

if [[ "$MODE" == "write" ]]; then
	mkdir -p "$STAMP_DIR"
	printf '%s\n' "$actual" >"$stamp.tmp"
	mv "$stamp.tmp" "$stamp"
	echo "test preflight: prepared $ARCH"
	exit 0
fi

if [[ ! -f "$stamp" ]]; then
	echo "test preflight: no prepared $ARCH image inputs; run just $PREPARE_RECIPE" >&2
	exit 1
fi
if [[ "$(<"$stamp")" != "$actual" ]]; then
	echo "test preflight: non-kernel inputs changed; run just $PREPARE_RECIPE" >&2
	exit 1
fi
echo "test preflight: $ARCH inputs are current"
