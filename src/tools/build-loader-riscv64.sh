#!/usr/bin/env bash
# Build the system's own UEFI loader for riscv64 (staged into the boot image as BOOTRISCV64.EFI).
#
# There is no built-in riscv64 UEFI rustc target - and rustc's object backend cannot emit a riscv64
# PE/COFF - so the loader is compiled as a static PIE on the ELF target with a hand-written PE/COFF
# header (loader/src/arch/riscv64/head.rs) prepended by the linker script (loader/riscv64-pe.ld);
# llvm-objcopy then flattens it into a valid EFI application, padded to the image end so its section
# raw data is fully backed.
#
# This was a Justfile recipe with a bash shebang inside it, which is what a recipe becomes when it
# is a program. `build.sh --arch riscv64 --part loader` reached it through `just`, so the ONE thing
# in the build path that still needed `just` installed was this file's contents.
set -euo pipefail

cd "$(dirname "$0")/../boot/loader"

# Built from the loader's own directory so cargo reads ITS `.cargo/config.toml`. Run with
# `--manifest-path` from elsewhere, cargo takes the configuration of the working directory
# instead - which has no `build-std` for this target, so the build failed with "can't find
# crate for core" and the recipe had been red for as long as anyone had looked.
RUSTFLAGS="-C relocation-model=pic -C link-arg=-pie -C link-arg=-T$PWD/riscv64-pe.ld" \
	cargo build --target riscv64gc-unknown-none-elf

# WHERE CARGO ACTUALLY PUT IT, which is not always the tree's own directory.
#
# This was the fixed path alone, so a build with `CARGO_TARGET_DIR` set wrote its ELF somewhere else
# and this went on reading - and CONVERTING - whatever was already at the fixed path. It reported
# success having produced the previous loader, which is the worst of the three possible answers. The
# profile gate builds a second loader into a directory of its own, and that is the caller that found
# it.
out="${CARGO_TARGET_DIR:-../../../.build/cargo/loader}/riscv64gc-unknown-none-elf/debug/libersystem-loader"

# READ THE ELF, THEN LOOK AT IT - not `llvm-readelf | awk ... || true`.
#
# That shape was in the Justfile recipe this file replaces, and it hides two different failures
# behind one `|| true`: `awk`'s early `exit` closes the pipe, so `llvm-readelf` takes a SIGPIPE and
# under `pipefail` the pipeline fails on SUCCESS - and a readelf that genuinely failed produces the
# same empty answer. The `source-hygiene` gate names this pattern and never saw it, because it scans
# scripts and the recipe was not one. Moving it here is what found it.
read_elf() {
	local what="$1"
	shift
	local output
	if ! output="$(llvm-readelf "$@" 2>&1)"; then
		echo "loader-riscv64: llvm-readelf failed reading $what" >&2
		printf '%s\n' "$output" >&2
		exit 1
	fi
	printf '%s\n' "$output"
}

# NO PIPE AT ALL, in either check. `awk` stops at the first match, and a reader that stops early
# closes the pipe under `pipefail` - so a SUCCESSFUL match reads as a failed pipeline. Reading the
# whole output into a variable first is what makes the two questions separable: did readelf work,
# and what does its output say.
symbols="$(read_elf "the symbol table" -s "$out")"
end=$(awk '/ _pe_image_end$/{print "0x"$2; exit}' <<<"$symbols")
[[ -n "$end" ]] || {
	echo "loader-riscv64: no _pe_image_end symbol in $out" >&2
	exit 1
}
# EVERY RELOCATION MUST BE ONE THE STUB APPLIES. `_pe_entry` walks `.rela.dyn` and handles
# R_RISCV_RELATIVE; a type it does not know leaves those pointers unrelocated, and the failure
# then appears somewhere with no connection to the cause. A toolchain that starts emitting one
# fails HERE, at the build, with the type named.
relocations="$(read_elf "the relocations" -r "$out")"
types="$(awk '$3 ~ /^R_/ {print $3}' <<<"$relocations" | sort -u)"
bad=$(grep -vx -e R_RISCV_RELATIVE -e R_RISCV_NONE <<<"$types" || true)
[[ -z "$bad" ]] || {
	echo "loader-riscv64: the self-relocation stub cannot apply: $bad" >&2
	exit 1
}
llvm-objcopy -O binary --pad-to "$end" "$out" "$out.efi"
echo "loader-riscv64: wrote $out.efi"
