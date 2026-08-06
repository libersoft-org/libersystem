#!/usr/bin/env bash
# Prove the targeted build and the authoritative rebuild produce the same artifact.
#
# The whole incremental workflow rests on one assumption: that building a single artifact
# produces what the full audited rebuild would have produced. Nothing else in this repository
# checks that. A targeted build that quietly diverged - a different flag, a stale provider, a
# different link order - would be invisible until something failed in the guest for reasons
# that had nothing to do with the change being tested.
#
# So this builds the same sources twice, once each way, and compares the bytes:
#
#   authoritative   every cache discarded, the complete compile/link/audit path
#   targeted        every cache discarded, one artifact and its closure only
#
# Both sides force a real rebuild rather than a cache hit. Comparing two cache hits would
# compare a file with itself and prove nothing at all, which is the trap this check exists to
# avoid falling into.
#
# The sample is what the roadmap names: a leaf executable, a shared provider with consumers,
# and the runtime provider every dynamic image links. Bytes are compared first because byte
# equality settles the question; the identity record is only read when they differ, to name
# the field that moved rather than leaving a digest mismatch to be investigated by hand.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
build="$root/../.build"

target="${1:-x86_64}"
shift || true
case "$target" in
x86_64 | x86_64-unknown-none)
	target=x86_64-unknown-none
	recipe=shared-libs
	;;
aarch64 | aarch64-unknown-none)
	target=aarch64-unknown-none
	recipe=shared-libs-aarch64
	;;
riscv64 | riscv64gc-unknown-none-elf)
	target=riscv64gc-unknown-none-elf
	recipe=shared-libs-riscv64
	;;
*)
	echo "fast-path-parity: unsupported target '$target'" >&2
	exit 2
	;;
esac

# A leaf executable, a provider with consumers, and the runtime provider.
sample=("$@")
if [[ ${#sample[@]} -eq 0 ]]; then sample=(uname base-proto lsrt); fi

staged_path() {
	local name="$1" destination
	destination="$("$root/tools/system-manifest.sh" export-json | python3 -c "
import json, sys
manifest = json.load(sys.stdin)
entry = manifest['programs'].get('$name') or manifest['libraries'].get('$name')
print(entry['destination'] if entry else '')
")"
	if [[ -z "$destination" ]]; then
		echo "fast-path-parity: $name is not a manifest-declared artifact" >&2
		exit 2
	fi
	echo "$build/image/$target/${destination%.lsexe}"
}

# The identity record as text, so a mismatch can be reported by field rather than by digest.
# An explicit output file is required: given one argument, llvm-objcopy edits the input.
identity_record() {
	local image="$1" out
	out="$(mktemp)"
	llvm-objcopy --dump-section .note.liber.identity="$out" "$image" /dev/null 2>/dev/null || true
	# The note carries an 8-byte header before the record's own text.
	tr -d '\0' <"$out" | sed -n 's/.*\(format=liber-image-identity-v1\)/\1/p;/^[a-z-]*=/p'
	rm -f "$out"
}

echo "fast-path-parity: authoritative rebuild of $target (every cache discarded)"
authoritative_started=$SECONDS
(cd "$root" && LIBER_IMAGE_REBUILD=1 just "$recipe" >/dev/null)
authoritative_seconds=$((SECONDS - authoritative_started))

# Keep the authoritative bytes: the targeted rebuild below overwrites the staged file.
keep="$(mktemp -d)"
for name in "${sample[@]}"; do
	path="$(staged_path "$name")"
	if [[ ! -f "$path" ]]; then
		echo "fast-path-parity: $name is declared but not staged at $path" >&2
		exit 1
	fi
	cp "$path" "$keep/$name"
done

failures=0
for name in "${sample[@]}"; do
	path="$(staged_path "$name")"
	targeted_started=$SECONDS
	(cd "$root" && LIBER_IMAGE_REBUILD=1 tools/dev-build.sh "$name" "$target" >/dev/null)
	targeted_seconds=$((SECONDS - targeted_started))
	if cmp -s "$keep/$name" "$path"; then
		echo "fast-path-parity: $name identical ($(stat -c %s "$path") B, targeted rebuild ${targeted_seconds}s)"
		continue
	fi
	failures=$((failures + 1))
	echo "fast-path-parity: $name DIFFERS between the targeted and authoritative builds" >&2
	echo "     authoritative $(sha256sum <"$keep/$name" | cut -c1-32)  targeted $(sha256sum <"$path" | cut -c1-32)" >&2
	# Name the first identity field that moved; identical records mean the difference is in
	# the code rather than in how it was built, which is the more serious of the two.
	diff <(identity_record "$keep/$name") <(identity_record "$path") >/dev/null 2>&1 && {
		echo "     the identity records match, so the two builds disagree about the code itself" >&2
		continue
	}
	# `diff` exits 1 when the files differ, which is the case being reported, so its status
	# is not an error here - and `head` closing the pipe would make it one anyway.
	differing="$(diff <(identity_record "$keep/$name") <(identity_record "$path") || true)"
	first="$(grep '^[<>]' <<<"$differing" | sed -n '1,2p' | tr '\n' ' ')"
	echo "     first identity difference: $first" >&2
done

echo "fast-path-parity: ${#sample[@]} artifact(s) compared, $failures divergence(s); authoritative rebuild took ${authoritative_seconds}s"
rm -rf "$keep"
exit $((failures > 0))
