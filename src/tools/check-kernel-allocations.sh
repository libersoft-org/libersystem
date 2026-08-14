#!/usr/bin/env bash
# A kernel allocation that ring 3 can trigger must be able to REFUSE.
#
# `Vec::with_capacity`, `vec![..]`, `Box::new`, `Arc::new`, `String::from`, `format!`, and growing a
# collection past its capacity all abort the kernel when the heap is short. On a path userspace can
# drive, that is a denial of service needing no privilege at all: exhaust the heap, then make the
# ordinary syscall. The fallible forms answer instead of halting - `try_reserve`, `try_reserve_exact`,
# and the `heap::try_box` / `try_arc` / `try_push` / `try_extend` / `try_string` / `try_vec` helpers.
#
# THIS EXISTS BECAUSE THE CLASS WAS CLOSED BY ENUMERATION THREE TIMES. Each audit of the kernel's
# lifetimes named a handful of call sites, each was fixed, and the next audit found the next
# members - the handle table's slot push, two mapping reservations, the capability send, the ELF
# loader's per-page record, the symbol registry, the page-fault handler's frame adopt, the entry
# context of every spawn. A list is not a rule. This is the rule.
#
# WHAT THIS GATE PROVES, exactly: no unmarked infallible heap growth in kernel source outside tests.
# That is NOT the same sentence as "every ring-3-reachable allocation can fail", and the difference
# is worth stating rather than leaving for the next audit to find. A gate over source text cannot
# know which paths ring 3 reaches; what it can do is force every site to be looked at once and its
# reason written down. The `ALLOC-OK:` markers are that record, and they are what a reviewer reads -
# a marker claiming a path is boot-only is a claim somebody can check, where an unmarked `Arc::new`
# is a question nobody was asked.
#
# The forms checked, and why each is here:
#   Vec::with_capacity, vec![..], Box::new   - allocate outright.
#   Arc::new                                 - almost every kernel OBJECT is one, and almost every
#                                              one of those is minted by a syscall.
#   String::from, format!, to_string          - allocate outright, and names come from ring 3.
#   push / push_back / push_front / insert / extend on a collection - reallocate when full.
#
# The exemptions:
#   - test code, by path: a test that cannot allocate has already failed.
#   - a function that calls `try_reserve` anywhere in its body: it asked for the room before it
#     grew, which is the whole point, and demanding a marker beside every push after a reservation
#     would bury the markers that matter. This applies only to the collection-growth forms; the
#     outright allocators are never exempt this way.
#   - an `ALLOC-OK:` marker with a reason, on the line itself or the line above it.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
kernel="${KERNEL_SRC:-$root/kernel}"

scan() {
	local dir="$1"
	local out
	# One awk pass per file: collect the function ranges first, so a growth call can be judged
	# against whether its own function reserved room. A bash line loop cannot see that.
	out="$(find "$dir" -name '*.rs' ! -name 'tests.rs' -not -path '*/tests/*' -not -path '*/test_suites/*' -not -path '*/build.rs' | sort | while IFS= read -r file; do
		awk -v file="$file" -v prefix="$dir/" '
			{ lines[FNR] = $0 }
			/^[[:space:]]*(pub[[:space:]]+)?(pub\([^)]*\)[[:space:]]+)?(const[[:space:]]+)?(unsafe[[:space:]]+)?(extern[[:space:]]+"[^"]*"[[:space:]]+)?fn[[:space:]]/ { starts[++nf] = FNR }
			END {
				# Everything before the first fn is item scope: statics, consts. Judged as one span.
				starts[0] = 1
				starts[nf + 1] = FNR + 1
				for (i = 0; i <= nf; i++) {
					reserved = 0
					for (l = starts[i]; l < starts[i + 1]; l++) if (lines[l] ~ /try_reserve/) reserved = 1
					for (l = starts[i]; l < starts[i + 1]; l++) {
						line = lines[l]
						if (line ~ /^[[:space:]]*\/\//) continue
						if (line ~ /ALLOC-OK:/) continue
						if (l > 1 && lines[l - 1] ~ /ALLOC-OK:/) continue
						hard = (line ~ /Vec::with_capacity\(|(^|[^_[:alnum:]])vec!\[|Box::new\(|Arc::new\(|Rc::new\(|String::from\(|format!\(|\.to_string\(\)/)
						grow = (line ~ /\.push\(|\.push_back\(|\.push_front\(|\.insert\(|\.extend\(|\.extend_from_slice\(/)
						if (hard || (grow && !reserved)) {
							name = file
							sub("^" prefix, "", name)
							printf "kernel-allocations: %s:%d infallible allocation on a path that may be reachable from ring 3\n", name, l
							sub(/^[[:space:]]+/, "", line)
							printf "    %s\n", line
						}
					}
				}
			}
		' "$file"
	done)"
	if [[ -n "$out" ]]; then
		printf '%s\n' "$out" >&2
		return 1
	fi
	return 0
}

# Prove the gate REFUSES before letting it approve: a run over a clean tree proves only that the tree
# is clean, and would pass just as well if the pattern had stopped matching.
self_test() {
	local scratch
	scratch="$(mktemp -d)"
	trap 'rm -rf "$scratch"' RETURN
	mkdir -p "$scratch/good" "$scratch/bad" "$scratch/marked" "$scratch/tests" "$scratch/reserved"

	printf 'fn f() {\n\tlet mut v: Vec<u8> = Vec::new();\n\tv.try_reserve(4).ok();\n}\n' >"$scratch/good/a.rs"
	if ! scan "$scratch/good" 2>/dev/null; then
		echo "kernel-allocations: SELF-TEST FAILED - fallible code was refused" >&2
		exit 1
	fi

	for form in 'let v = Vec::with_capacity(4);' 'let v = alloc::vec![0u8; 4];' 'let b = Box::new(4u8);' 'let a = Arc::new(4u8);' 'let s = String::from("x");' 'let s = alloc::format!("{}", 1);' 'v.push(4u8);' 'q.push_back(4u8);' 'v.extend(other);'; do
		printf 'fn f() {\n\t%s\n}\n' "$form" >"$scratch/bad/a.rs"
		if scan "$scratch/bad" 2>/dev/null; then
			echo "kernel-allocations: SELF-TEST FAILED - an infallible allocation was accepted: $form" >&2
			exit 1
		fi
	done

	# A reservation in the same function covers the growth after it - and does NOT cover an
	# outright allocation, which no reservation makes fallible.
	printf 'fn f() {\n\tv.try_reserve(1).ok();\n\tv.push(4u8);\n}\n' >"$scratch/reserved/a.rs"
	if ! scan "$scratch/reserved" 2>/dev/null; then
		echo "kernel-allocations: SELF-TEST FAILED - a push after a reservation was refused" >&2
		exit 1
	fi
	printf 'fn f() {\n\tv.try_reserve(1).ok();\n\tlet a = Arc::new(4u8);\n}\n' >"$scratch/reserved/a.rs"
	if scan "$scratch/reserved" 2>/dev/null; then
		echo "kernel-allocations: SELF-TEST FAILED - a reservation excused an outright allocation" >&2
		exit 1
	fi
	# And a reservation in a DIFFERENT function does not reach across the boundary.
	printf 'fn f() {\n\tv.try_reserve(1).ok();\n}\n\nfn g() {\n\tv.push(4u8);\n}\n' >"$scratch/reserved/a.rs"
	if scan "$scratch/reserved" 2>/dev/null; then
		echo "kernel-allocations: SELF-TEST FAILED - a reservation covered another function's growth" >&2
		exit 1
	fi

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
	# One marker does not license the next line as well.
	printf 'fn f() {\n\t// ALLOC-OK: boot\n\tlet v = Vec::with_capacity(4);\n\tlet w = Vec::with_capacity(4);\n}\n' >"$scratch/marked/a.rs"
	if scan "$scratch/marked" 2>/dev/null; then
		echo "kernel-allocations: SELF-TEST FAILED - one marker covered a second allocation" >&2
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
