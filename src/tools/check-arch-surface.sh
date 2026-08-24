#!/usr/bin/env bash
# Every symbol in an architecture's compiled contract is a path that architecture can execute.
#
# WHAT THIS REFUSES, AND WHY IT IS A GATE RATHER THAN A READING. The three backends used to carry
# twenty `todo!()` bodies between them - the x86 loader hand-off, answered by every port that does
# not arrive through it. None was reachable: aarch64 enters `aarch64::boot::aarch64_main` and
# riscv64 `riscv64::boot::riscv64_main`, each bringing up its own console, page tables, per-CPU
# register, interrupt controller, timer, syscall vector and secondary cores. But a static scan and a
# reader both saw unfinished interrupt and timer glue, and the only thing separating the two
# readings was a paragraph of prose. P02M0151 removed the bodies by removing the requirement: the
# hand-off compiles for x86_64 alone. This keeps them from coming back one convenient stub at a time.
#
# A TEST FAULT PROBE IS NOT AN EXCEPTION TO THIS. The suites deliberately fault to prove a handler
# runs, and those live in `tests.rs` files or behind `#[cfg(test)]`, which is what makes them
# identifiable as tests rather than as a hole in the contract.
set -euo pipefail

cd "$(dirname "$0")/.."
root="kernel/arch"
status=0

# The scan skips `tests.rs` files entirely, and inside every other file it drops the `#[cfg(test)]`
# blocks before looking. The block filter is line-based and deliberately simple: a `#[cfg(test)]`
# attribute turns the scan off until the brace depth returns to what it was.
scan() {
	local file="$1"
	awk '
		/^[[:space:]]*#\[cfg\(test\)\]/ { skipping = 1; depth = 0; next }
		skipping {
			n = gsub(/\{/, "{"); m = gsub(/\}/, "}")
			depth += n - m
			if (depth <= 0 && (n > 0 || m > 0)) skipping = 0
			next
		}
		/todo!\(|unimplemented!\(/ {
			if ($0 ~ /^[[:space:]]*\/\//) next
			printf "%s:%d:%s\n", FILENAME, FNR, $0
		}
	' "$file"
}

while IFS= read -r file; do
	found="$(scan "$file")"
	if [[ -n "$found" ]]; then
		echo "$found" | while IFS= read -r line; do
			echo "arch-surface: $line" >&2
		done
		status=1
	fi
done < <(find "$root" -name '*.rs' ! -name 'tests.rs' | sort)

# Prove the gate REFUSES before letting it approve: a tree that is clean proves nothing about a scan
# that has stopped matching, which is how six gates in this tree once reported success for a month.
self_test() {
	local scratch
	scratch="$(mktemp -d)"
	trap 'rm -rf "$scratch"' RETURN
	mkdir -p "$scratch/arch/fake"
	printf 'pub fn live() {\n\ttodo!("not done")\n}\n' >"$scratch/arch/fake/mod.rs"
	if [[ -z "$(scan "$scratch/arch/fake/mod.rs")" ]]; then
		echo "arch-surface: SELF-TEST FAILED - an unreachable body in a production file was not seen" >&2
		return 1
	fi
	printf '#[cfg(test)]\nmod tests {\n\tpub fn probe() {\n\t\ttodo!("a deliberate test fault")\n\t}\n}\n' >"$scratch/arch/fake/mod.rs"
	if [[ -n "$(scan "$scratch/arch/fake/mod.rs")" ]]; then
		echo "arch-surface: SELF-TEST FAILED - a test-only body was reported as a contract hole" >&2
		return 1
	fi
}
self_test || exit 1

if ((status == 0)); then
	echo "arch-surface: no unreachable bodies in the compiled architecture surface ($(find "$root" -name '*.rs' ! -name 'tests.rs' | wc -l) file(s))"
fi
exit "$status"
