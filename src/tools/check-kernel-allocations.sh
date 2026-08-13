#!/usr/bin/env bash
# A kernel allocation that ring 3 can trigger must be able to REFUSE.
#
# `Vec::with_capacity`, `vec![..]` and `Box::new` all abort the kernel when the heap is short. On a
# path userspace can drive, that is a denial of service needing no privilege at all: exhaust the
# heap, then make the ordinary syscall. The fallible forms - `try_reserve`, `try_reserve_exact`, and
# the `try_box` helper - answer instead of halting.
#
# THIS EXISTS BECAUSE THE CLASS WAS CLOSED BY ENUMERATION THREE TIMES. Each audit of the kernel's
# lifetimes named a handful of call sites, each was fixed, and the next audit found the next
# members - the handle table's slot push, two mapping reservations, the capability send, the ELF
# loader's per-page record, the symbol registry, the page-fault handler's frame adopt, the entry
# context of every spawn. A list is not a rule. This is the rule.
#
# What is checked: the three infallible forms, in the kernel crate, outside test code. What is
# allowed: a line carrying an `ALLOC-OK:` marker with a reason, on the line itself or the line above
# it. The marker is not a silencer - it is where a boot-only or allocator-internal path says which it
# is, in a place a reviewer of the next diff will read.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
kernel="${KERNEL_SRC:-$root/kernel}"

# The three forms. `Vec::new()` is not here: it does not allocate.
pattern='Vec::with_capacity\(|(^|[^_[:alnum:]])vec!\[|Box::new\('

scan() {
	local dir="$1" failed=0 file line n prev
	while IFS= read -r file; do
		# Test code is exempt: a test that cannot allocate has already failed, and its abort is a
		# test failure rather than a machine halt.
		case "$file" in
		*/tests.rs | */test_suites/* | */tests/*) continue ;;
		esac
		n=0
		prev=""
		while IFS= read -r line; do
			n=$((n + 1))
			if [[ "$line" =~ $pattern ]]; then
				# A comment ABOUT one of these forms is not one of them.
				[[ "$line" =~ ^[[:space:]]*// ]] && {
					prev="$line"
					continue
				}
				if [[ "$line" == *ALLOC-OK:* || "$prev" == *ALLOC-OK:* ]]; then
					prev="$line"
					continue
				fi
				echo "kernel-allocations: ${file#"$dir"/}:$n infallible allocation on a path that may be reachable from ring 3" >&2
				echo "    ${line#"${line%%[![:space:]]*}"}" >&2
				failed=1
			fi
			prev="$line"
		done <"$file"
	done < <(find "$dir" -name '*.rs' | sort)
	return "$failed"
}

# Prove the gate REFUSES before letting it approve: a run over a clean tree proves only that the tree
# is clean, and would pass just as well if the pattern had stopped matching.
self_test() {
	local scratch
	scratch="$(mktemp -d)"
	trap 'rm -rf "$scratch"' RETURN
	mkdir -p "$scratch/good" "$scratch/bad" "$scratch/marked" "$scratch/tests"

	printf 'fn f() {\n\tlet mut v: Vec<u8> = Vec::new();\n\tv.try_reserve(4).ok();\n}\n' >"$scratch/good/a.rs"
	if ! scan "$scratch/good" 2>/dev/null; then
		echo "kernel-allocations: SELF-TEST FAILED - fallible code was refused" >&2
		exit 1
	fi

	for form in 'let v = Vec::with_capacity(4);' 'let v = alloc::vec![0u8; 4];' 'let b = Box::new(4u8);'; do
		printf 'fn f() {\n\t%s\n}\n' "$form" >"$scratch/bad/a.rs"
		if scan "$scratch/bad" 2>/dev/null; then
			echo "kernel-allocations: SELF-TEST FAILED - an infallible allocation was accepted: $form" >&2
			exit 1
		fi
	done

	# The marker, on the line and on the line above it.
	printf 'fn f() {\n\tlet v = Vec::with_capacity(4); // ALLOC-OK: boot, before userspace exists\n}\n' >"$scratch/marked/a.rs"
	if ! scan "$scratch/marked" 2>/dev/null; then
		echo "kernel-allocations: SELF-TEST FAILED - a marked allocation was refused" >&2
		exit 1
	fi
	printf 'fn f() {\n\t// ALLOC-OK: boot, before userspace exists\n\tlet v = Vec::with_capacity(4);\n}\n' >"$scratch/marked/a.rs"
	if ! scan "$scratch/marked" 2>/dev/null; then
		echo "kernel-allocations: SELF-TEST FAILED - a marker on the preceding line was not honoured" >&2
		exit 1
	fi

	# Test code is exempt by path, and the exemption must actually apply.
	printf 'fn f() {\n\tlet v = Vec::with_capacity(4);\n}\n' >"$scratch/tests/tests.rs"
	if ! scan "$scratch/tests" 2>/dev/null; then
		echo "kernel-allocations: SELF-TEST FAILED - test code was not exempt" >&2
		exit 1
	fi
}

self_test
scan "$kernel"
echo "kernel-allocations: clean"
