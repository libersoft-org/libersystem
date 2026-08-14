#!/usr/bin/env bash
# A frame a page table ever pointed at goes back through `frame::retire`, never `frame::deallocate`.
#
# `retire` exists because of a defect this kernel had: `unmap` clears the PTE and invalidates the
# LOCAL core's TLB and nothing else, so another core running in the same address space keeps its
# translation until a shootdown tells it to drop one. A frame handed back to the allocator in that
# window is a physical use-after-free, and it does not need a hostile process - it needs two cores and
# a mapping that outlived its unmap by a few microseconds. `retire` does the shootdown, and quarantines
# what it could not complete rather than freeing it.
#
# THIS EXISTS BECAUSE THE RULE WAS ALREADY WRITTEN DOWN AND STILL BROKEN. Every site that frees
# something a page table pointed at was converted to `retire` in the round that created it, the
# module's own doc comment states the rule and names the exact failure - and the NEXT round added a
# rollback on the stack-growth path that unmapped a page and called `deallocate`. A rule stated in a
# comment is a rule the next diff does not read.
#
# What is checked: `frame::deallocate` and the two `dealloc_frame` wrappers over it, in the kernel
# crate, outside test code and outside the frame allocator itself. What is allowed: a call carrying a
# `NEVER-MAPPED:` marker with the reason, on the line itself or in the comment block just above it.
# The marker is not a silencer - it is where the caller says why no core can hold a translation, in
# the place a reviewer of the next diff will read. A caller that cannot say it should be calling
# `retire`.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
kernel="${KERNEL_SRC:-$root/kernel}"

# `deallocate` and its two per-architecture wrappers, which are the same door under another name.
pattern='(frame::)?deallocate\(|dealloc_frame\('

scan() {
	local dir="$1" failed=0 file line n prev
	while IFS= read -r file; do
		case "$file" in
		# Test code is exempt: a test that frees a mapped frame wrongly corrupts its own run, and the
		# tests are single-core by construction.
		*/tests.rs | */test_suites/* | */tests/*) continue ;;
		# The allocator itself IS the retirement path - `retire` calls `deallocate` once the
		# shootdown is done, which is the whole point - and `deallocate`'s own definition lives here.
		*/mem/frame/*) continue ;;
		esac
		n=0
		prev=""
		while IFS= read -r line; do
			n=$((n + 1))
			if [[ "$line" =~ $pattern ]]; then
				# A comment ABOUT the call, or the wrapper's own definition, is not a call.
				if [[ "$line" =~ ^[[:space:]]*// || "$line" =~ ^[[:space:]]*(pub[[:space:]]+)?(unsafe[[:space:]]+)?fn[[:space:]] ]]; then
					[[ "$line" =~ ^[[:space:]]*// ]] && prev="$prev $line"
					continue
				fi
				if [[ "$line" == *NEVER-MAPPED:* || "$prev" == *NEVER-MAPPED:* ]]; then
					prev=""
					continue
				fi
				echo "frame-retirement: ${file#"$dir"/}:$n a frame is returned through the allocator's plain door with no statement that no core can still translate it" >&2
				echo "    ${line#"${line%%[![:space:]]*}"}" >&2
				echo "    use frame::retire, or say NEVER-MAPPED: <why> on the call or in the comment above it" >&2
				failed=1
			fi
			# The comment block above a call, accumulated, so a multi-line SAFETY note carries the
			# marker wherever in it the author put it. Any non-comment line ends the block.
			if [[ "$line" =~ ^[[:space:]]*// ]]; then
				prev="$prev $line"
			else
				prev=""
			fi
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
	mkdir -p "$scratch/good" "$scratch/bad" "$scratch/marked" "$scratch/tests" "$scratch/mem/frame"

	printf 'fn f() {\n\tunsafe { frame::retire(&[p]) };\n}\n' >"$scratch/good/a.rs"
	if ! scan "$scratch/good" 2>/dev/null; then
		echo "frame-retirement: SELF-TEST FAILED - a retirement was refused" >&2
		exit 1
	fi

	for form in 'unsafe { frame::deallocate(p) };' 'unsafe { crate::mem::frame::deallocate(p) };' 'unsafe { paging::dealloc_frame(p) };'; do
		printf 'fn f() {\n\t%s\n}\n' "$form" >"$scratch/bad/a.rs"
		if scan "$scratch/bad" 2>/dev/null; then
			echo "frame-retirement: SELF-TEST FAILED - an unmarked free was accepted: $form" >&2
			exit 1
		fi
	done

	printf 'fn f() {\n\tunsafe { frame::deallocate(p) }; // NEVER-MAPPED: the map is what failed\n}\n' >"$scratch/marked/a.rs"
	if ! scan "$scratch/marked" 2>/dev/null; then
		echo "frame-retirement: SELF-TEST FAILED - a marked free was refused" >&2
		exit 1
	fi
	printf 'fn f() {\n\t// SAFETY: allocated here.\n\t// NEVER-MAPPED: the map is what failed.\n\tunsafe { frame::deallocate(p) };\n}\n' >"$scratch/marked/a.rs"
	if ! scan "$scratch/marked" 2>/dev/null; then
		echo "frame-retirement: SELF-TEST FAILED - a marker in the comment block above was not honoured" >&2
		exit 1
	fi
	# And a marker that belonged to an EARLIER call does not cover a later one: the block resets at
	# the first non-comment line, or one justification would license every free after it in the file.
	printf 'fn f() {\n\t// NEVER-MAPPED: the map is what failed.\n\tunsafe { frame::deallocate(p) };\n\tunsafe { frame::deallocate(q) };\n}\n' >"$scratch/marked/a.rs"
	if scan "$scratch/marked" 2>/dev/null; then
		echo "frame-retirement: SELF-TEST FAILED - one marker covered a second, unrelated free" >&2
		exit 1
	fi

	printf 'fn f() {\n\tunsafe { frame::deallocate(p) };\n}\n' >"$scratch/tests/tests.rs"
	if ! scan "$scratch/tests" 2>/dev/null; then
		echo "frame-retirement: SELF-TEST FAILED - test code was not exempt" >&2
		exit 1
	fi
	printf 'fn f() {\n\tunsafe { deallocate(p) };\n}\n' >"$scratch/mem/frame/mod.rs"
	if ! scan "$scratch/mem" 2>/dev/null; then
		echo "frame-retirement: SELF-TEST FAILED - the allocator itself was not exempt" >&2
		exit 1
	fi
}

self_test
scan "$kernel"
echo "frame-retirement: clean"
