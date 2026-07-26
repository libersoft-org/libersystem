#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 ]]; then
	echo "usage: dev-build.sh <artifact> [x86_64|aarch64|riscv64]" >&2
	exit 2
fi

artifact="$1"
target="${2:-x86_64}"
root="$(cd "$(dirname "$0")/.." && pwd)"

case "$target" in
x86_64 | x86_64-unknown-none) target="x86_64-unknown-none" ;;
aarch64 | aarch64-unknown-none) target="aarch64-unknown-none" ;;
riscv64 | riscv64gc-unknown-none-elf) target="riscv64gc-unknown-none-elf" ;;
*)
	echo "dev-build: unsupported target '$target'" >&2
	exit 2
	;;
esac

"$root/tools/build-shared.sh" --artifact "$artifact" "$target"
