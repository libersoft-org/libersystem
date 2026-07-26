#!/usr/bin/env bash
set -euo pipefail

explain=0
if [[ "${1:-}" == "--explain" ]]; then
	explain=1
	shift
fi

if [[ $# -lt 1 || $# -gt 2 ]]; then
	echo "usage: dev-build.sh [--explain] <artifact> [x86_64|aarch64|riscv64]" >&2
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

arguments=(--artifact "$artifact")
if [[ "$explain" == 1 ]]; then arguments=(--explain "${arguments[@]}"); fi
"$root/tools/build-shared.sh" "${arguments[@]}" "$target"
