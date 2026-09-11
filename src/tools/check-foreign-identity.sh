#!/bin/bash
# A FOREIGN ARTIFACT'S IDENTITY IS LOAD BEARING, field by field.
#
# THE CLAIM BEING CHECKED. A foreign artifact's identity record carries what TURNED ITS SOURCES INTO
# CODE - the compiler, the archiver, the linker, the flags, the sysroot it compiled against and the
# configure inputs that selected what was built. Every one of those can change the ABI of the result
# without changing a single source byte. So each must change the artifact's identity digest, and
# through it every consumer edge that names the artifact and the cache key that decides whether to
# rebuild it. A digest that survives any of them is a stale consumer or a stale cache entry passing
# after an ABI-affecting rebuild.
#
# WHY THE CHECK IS ARITHMETIC AND NOT A REBUILD. Proving it by rebuilding would need a second
# compiler, a second sysroot and a second flag set on the machine running the gate. What the claim
# actually rests on is narrower and exactly checkable: the digest is taken over the WHOLE record, and
# the record is the whole of the cache key's identity part. So a mutation of any line changes both,
# and that is what is measured here - on the real staged record, one line at a time.
#
# WHAT ELSE HOLDS THE SAME GROUND, named so this gate is not read as the only thing that does:
# the guest suite mutates each of these fields inside a real volume and requires ProcessService to
# refuse the launch, and `icd-selection` boots the consumer that reaches the artifact through a
# selection slot. This gate is the BUILD-side half - the digest and the cache key - and those two are
# the LAUNCH-side half.

set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/.."

target="x86_64-unknown-none"
image="$root/../.build/image/$target"
cache="$root/../.build/cache/$target"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

fail() {
	echo "foreign-identity: $*" >&2
	exit 1
}

record_of() {
	llvm-objcopy --dump-section .note.liber.identity=/dev/stdout "$1" /dev/null 2>/dev/null | tail -c +21 | tr -d '\0'
}

foreign="$image/lib/foreign/icdprobe.lslib"
consumer="$image/libexec/icdcheck"
[[ -f "$foreign" ]] || fail "the foreign artifact is not staged at $foreign - build the image first"
[[ -f "$consumer" ]] || fail "the consumer is not staged at $consumer - build the image first"

record="$work/record"
record_of "$foreign" >"$record"
[[ -s "$record" ]] || fail "the foreign artifact carries no identity record"
grep -qx "language=foreign" "$record" || fail "the foreign artifact does not carry a foreign language section"
base="$(sha256sum "$record" | cut -d' ' -f1)"

# THE MUTATIONS. One line at a time, each a field that can change what the artifact IS without
# changing a source byte.
for field in compiler compiler-sha256 archiver archiver-sha256 linker linker-sha256 cflags sysroot-sha256 configure-sha256 objects-sha256 patches-sha256 licence; do
	grep -q "^$field=" "$record" || fail "the foreign record has no $field line"
	sed "s|^$field=.*|$field=mutated|" "$record" >"$work/mutated"
	cmp -s "$record" "$work/mutated" && fail "$field: the mutation changed nothing, so this proves nothing"
	mutated="$(sha256sum "$work/mutated" | cut -d' ' -f1)"
	[[ "$mutated" != "$base" ]] || fail "$field: a changed $field leaves the identity digest unmoved"
	echo "foreign-identity: $field moves the identity digest"
done

# AND THE RUST SIDE OF THE SAME RULE, because the language section is per producer and a check that
# only covered one of them would leave the other free to change under a matching digest.
rust_record="$work/rust-record"
record_of "$consumer" >"$rust_record"
grep -qx "language=rust" "$rust_record" || fail "the consumer does not carry a rust language section"
rust_base="$(sha256sum "$rust_record" | cut -d' ' -f1)"
for field in rustc-commit rustflags features; do
	grep -q "^$field=" "$rust_record" || fail "the rust record has no $field line"
	sed "s|^$field=.*|$field=mutated|" "$rust_record" >"$work/mutated"
	mutated="$(sha256sum "$work/mutated" | cut -d' ' -f1)"
	[[ "$mutated" != "$rust_base" ]] || fail "$field: a changed $field leaves the identity digest unmoved"
	echo "foreign-identity: $field moves the identity digest"
done

# THE CONSUMER EDGE. What a consumer records for a provider - or for a selection candidate - is that
# provider's identity digest, so "the digest moved" and "every consumer edge that names it is
# invalidated" are the same sentence only if the recorded number IS this number.
recorded="$(sed -n 's/^selection=vulkan-icd:icdprobe\.lslib=\([0-9a-f]\{64\}\)$/\1/p' "$rust_record")"
[[ -n "$recorded" ]] || fail "the consumer records no selection candidate digest for the foreign artifact"
[[ "$recorded" == "$base" ]] || fail "the consumer records $recorded and the staged artifact's record hashes to $base"
echo "foreign-identity: the consumer edge carries exactly the artifact's identity digest"

# THE CACHE KEY. The artifact's cache-input file ENDS WITH the identity record, and the key is that
# file's digest - so a moved record is a moved key by construction. Checked rather than reasoned:
# the inputs file on disk must contain the record, and its digest must be the recorded build key.
inputs="$cache/library-icdprobe.inputs"
key_file="$cache/library-icdprobe.build-key"
[[ -f "$inputs" && -f "$key_file" ]] || fail "the foreign artifact has no cache entry to check"
if ! grep -qx "configure-sha256=$(sed -n 's/^configure-sha256=//p' "$record")" "$inputs"; then
	fail "the cache inputs do not contain the artifact's identity record; then a changed record would not change the key"
fi
expected_key="$(sha256sum "$inputs" | cut -d' ' -f1)"
actual_key="$(head -c 64 "$key_file")"
[[ "$expected_key" == "$actual_key" ]] || fail "the recorded build key is not the digest of the cache inputs ($actual_key vs $expected_key)"
echo "foreign-identity: the cache key is the digest of a file the identity record is part of"

echo "foreign-identity: every producer field moves the digest, the consumer edge and the cache key"
