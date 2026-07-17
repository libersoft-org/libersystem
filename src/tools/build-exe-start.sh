#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
	echo "usage: build-exe-start.sh <target> <output>" >&2
	exit 2
fi

target="$1"
out="$2"
root="$(cd "$(dirname "$0")/.." && pwd)"
mkdir -p "$(dirname "$out")"
generator="${TMPDIR:-/tmp}/libersystem-exe-start-generator"
source="${TMPDIR:-/tmp}/libersystem-exe-start-${target}.s"
rustc --edition=2024 -O "$root/tools/exe-start.rs" -o "$generator"
"$generator" "$target" >"$source"

case "$target" in
x86_64-unknown-none)
	triple=x86_64-unknown-none-elf
	assembler_flags=()
	;;
aarch64-unknown-none)
	triple=aarch64-unknown-none-elf
	assembler_flags=()
	;;
riscv64gc-unknown-none-elf)
	triple=riscv64-unknown-none-elf
	assembler_flags=(-target-abi=lp64d -mattr=+m,+a,+f,+d,+c)
	;;
*)
	echo "build-exe-start: unsupported target '$target'" >&2
	exit 2
	;;
esac

llvm-mc -filetype=obj -triple="$triple" "${assembler_flags[@]}" "$source" -o "$out"

if ! llvm-readelf -h "$out" | grep -q 'Type:.*REL'; then
	echo "build-exe-start: $out is not ET_REL" >&2
	exit 1
fi
defined="$(llvm-readelf --wide --symbols "$out" | awk '$5 == "GLOBAL" && $7 != "UND" && $8 != "" {print $8}' | sort -u)"
undefined="$(llvm-readelf --wide --symbols "$out" | awk '$5 == "GLOBAL" && $7 == "UND" && $8 != "" {print $8}' | sort -u)"
if [[ "$defined" != "_start" || "$undefined" != "$(printf '%s\n' __user_main liber_rt_start | sort)" ]]; then
	echo "build-exe-start: $out has an unexpected symbol boundary" >&2
	exit 1
fi
relocations="$(llvm-readelf -r "$out")"
case "$target" in
x86_64-unknown-none) expected_relocations=(R_X86_64_PC32 R_X86_64_PLT32) ;;
aarch64-unknown-none) expected_relocations=(R_AARCH64_ADR_PREL_PG_HI21 R_AARCH64_ADD_ABS_LO12_NC R_AARCH64_CALL26) ;;
riscv64gc-unknown-none-elf) expected_relocations=(R_RISCV_PCREL_HI20 R_RISCV_PCREL_LO12_I R_RISCV_CALL_PLT) ;;
esac
for relocation in "${expected_relocations[@]}"; do
	if ! grep -q "$relocation" <<<"$relocations"; then
		echo "build-exe-start: $out is missing $relocation" >&2
		exit 1
	fi
done
echo "build-exe-start: $out ($(stat -c %s "$out") bytes)"
