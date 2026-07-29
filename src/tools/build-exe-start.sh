#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
	echo "usage: build-exe-start.sh <target> <output>" >&2
	exit 2
fi

target="$1"
out="$2"
root="$(cd "$(dirname "$0")/.." && pwd)"
build_root="$root/../.build"
tool_dir="$build_root/tools"
generator="$tool_dir/exe-start-generator"
generator_key_file="$tool_dir/exe-start-generator.build-key"
object_key_file="$out.build-key"
source="$tool_dir/exe-start-${target}.$$.s"
temporary_generator="$generator.tmp.$$"
temporary_object="$out.tmp.$$"
temporary_key="$object_key_file.tmp.$$"
mkdir -p "$(dirname "$out")" "$tool_dir"
trap 'rm -f "$source" "$temporary_generator" "$temporary_object" "$temporary_key"' EXIT

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

generator_key="$({
	printf 'format=liber-exe-start-generator-v1\n'
	sha256sum "$root/tools/exe-start.rs"
	rustc -vV
} | sha256sum | awk '{print $1}')"
if [[ ! -x "$generator" || ! -f "$generator_key_file" || "$(<"$generator_key_file")" != "$generator_key" ]]; then
	rustc --edition=2024 -O "$root/tools/exe-start.rs" -o "$temporary_generator"
	mv "$temporary_generator" "$generator"
	printf '%s\n' "$generator_key" >"$generator_key_file.tmp.$$"
	mv "$generator_key_file.tmp.$$" "$generator_key_file"
fi

object_key="$({
	printf 'format=liber-exe-start-object-v1\n'
	printf 'generator=%s\n' "$generator_key"
	printf 'target=%s\n' "$target"
	sha256sum "$root/tools/build-exe-start.sh"
	llvm-mc --version
} | sha256sum | awk '{print $1}')"
if [[ "${LIBER_IMAGE_REBUILD:-0}" == 0 && -f "$out" && -f "$object_key_file" && "$(<"$object_key_file")" == "$object_key" ]]; then
	echo "build-exe-start: cache hit $out"
	exit 0
fi

"$generator" "$target" >"$source"

llvm-mc -filetype=obj -triple="$triple" "${assembler_flags[@]}" "$source" -o "$temporary_object"

start_object_header="$(llvm-readelf -h "$temporary_object")"
if ! grep -q 'Type:.*REL' <<<"$start_object_header"; then
	echo "build-exe-start: generated object is not ET_REL" >&2
	exit 1
fi
defined="$(llvm-readelf --wide --symbols "$temporary_object" | awk '$5 == "GLOBAL" && $7 != "UND" && $8 != "" {print $8}' | sort -u)"
undefined="$(llvm-readelf --wide --symbols "$temporary_object" | awk '$5 == "GLOBAL" && $7 == "UND" && $8 != "" {print $8}' | sort -u)"
if [[ "$defined" != "_start" || "$undefined" != "$(printf '%s\n' __user_main liber_rt_start | sort)" ]]; then
	echo "build-exe-start: $out has an unexpected symbol boundary" >&2
	exit 1
fi
relocations="$(llvm-readelf -r "$temporary_object")"
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
mv "$temporary_object" "$out"
printf '%s\n' "$object_key" >"$temporary_key"
mv "$temporary_key" "$object_key_file"
echo "build-exe-start: cache miss $out ($(stat -c %s "$out") bytes)"
