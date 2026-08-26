#!/usr/bin/env bash
# The staged-tree check refuses what it cannot read, and this is what proves it.
#
# WHAT A CHECK DOES WITH INPUT IT CANNOT READ IS THE CHECK. `verify_staged_provider_chains` had three
# fail-open branches on one screen - an unreadable provider became an empty digest, an unreadable
# consumer was skipped, a missing provider was skipped - so deleting a provider a consumer records
# left it exiting zero and printing that every library matched the providers staged beside it. Each
# of those is now a refusal, and a refusal nobody has watched happen is a refusal nobody has tested.
#
# Every case below MUTATES THE REAL STAGED TREE and puts it back. The trap restores on any exit,
# including an interrupt, because a staged tree left mutated by a gate is a build that fails
# afterwards for a reason nothing explains.
set -euo pipefail

cd "$(dirname "$0")/../.."
TARGET="${1:-x86_64-unknown-none}"
LIB=".build/image/$TARGET/lib"
VERIFY="src/tools/build-shared.sh"

fail() {
	echo "staged-consistency: $*" >&2
	exit 1
}

[[ -d "$LIB" ]] || fail "no staged tree at $LIB - run ./build.sh --arch ${TARGET%%-*} first"
command -v llvm-objcopy >/dev/null || fail "llvm-objcopy is required by this gate"

work="$(mktemp -d)"
victim=""
restore() {
	if [[ -n "$victim" && -f "$work/original" ]]; then
		cp "$work/original" "$victim"
	fi
	rm -rf "$work"
}
trap restore EXIT

# A consumer and the provider it records, taken from the tree rather than named here: a gate that
# hardcodes two artifact names stops testing the day either is renamed.
consumer=""
provider=""
while IFS= read -r file; do
	note="$work/note"
	llvm-objcopy --dump-section .note.liber.identity="$note" "$file" /dev/null 2>/dev/null || continue
	# `sed -n '1p'` rather than `head -1`: nothing downstream of a pipe here stops early, because
	# under `pipefail` that is a failed pipeline on a match that worked.
	row="$(grep -a -o 'provider=[a-z0-9_-]*:[0-9a-f]\{64\}' "$note" | sed -n '1p' || true)"
	[[ -n "$row" ]] || continue
	consumer="$file"
	provider="${row#provider=}"
	provider="${provider%%:*}"
	break
done < <(find "$LIB" -name '*.lslib' -type f | sort)
[[ -n "$consumer" && -n "$provider" ]] || fail "no staged library records a provider - there is nothing here to check"
provider_file="$(find "$LIB" -name "$provider.lslib" -type f | sed -n '1p')"
[[ -n "$provider_file" ]] || fail "the recorded provider $provider is not staged, which this gate cannot mutate around"
echo "staged-consistency: $(basename "$consumer" .lslib) records $provider"

# THE BASELINE FIRST. A gate whose mutations all fail is indistinguishable from one whose subject is
# broken to begin with.
"$VERIFY" --verify-staged "$TARGET" >/dev/null 2>&1 || fail "the unmutated staged tree does not verify - every case below would be meaningless"
echo "staged-consistency: the unmutated tree verifies"

refuses() {
	local what="$1"
	if "$VERIFY" --verify-staged "$TARGET" >"$work/out" 2>&1; then
		echo "staged-consistency: $what was ACCEPTED" >&2
		sed -n '1,10p' "$work/out" >&2
		exit 1
	fi
	echo "staged-consistency:   refused: $what"
}

mutate() {
	victim="$1"
	cp "$victim" "$work/original"
}

undo() {
	cp "$work/original" "$victim"
	victim=""
}

# 1. THE PROVIDER IS GONE. The case the original defect is named for: a consumer records a provider
#    that is not staged beside it, and the check used to skip it.
mutate "$provider_file"
rm -f "$provider_file"
refuses "a provider a consumer records is missing from the staged tree"
undo

# 2. THE PROVIDER HAS NO IDENTITY SECTION. Unreadable rather than absent, which used to become an
#    empty digest and then be skipped for being empty.
mutate "$provider_file"
llvm-objcopy --remove-section .note.liber.identity "$provider_file" 2>/dev/null || fail "could not remove the identity section"
refuses "a staged provider carrying no identity note"
undo

# 3. THE CONSUMER HAS NO IDENTITY SECTION. The other side of the same branch.
mutate "$consumer"
llvm-objcopy --remove-section .note.liber.identity "$consumer" 2>/dev/null || fail "could not remove the identity section"
refuses "a staged library carrying no identity note"
undo

# 4. A DECLARED LENGTH OF ZERO. The header says the record is empty; there is no identity in an
#    empty record.
mutate "$provider_file"
llvm-objcopy --dump-section .note.liber.identity="$work/note" "$provider_file" /dev/null
printf '\x00\x00\x00\x00' | dd of="$work/note" bs=1 seek=4 count=4 conv=notrunc status=none
llvm-objcopy --update-section .note.liber.identity="$work/note" "$provider_file" 2>/dev/null || fail "could not write the mutated note back"
refuses "an identity note declaring a zero-length record"
undo

# 5. A DECLARED LENGTH PAST THE END OF THE SECTION. The header describes bytes the file does not
#    contain, and the digest used to be taken over whatever followed.
mutate "$provider_file"
llvm-objcopy --dump-section .note.liber.identity="$work/note" "$provider_file" /dev/null
printf '\xff\xff\x00\x00' | dd of="$work/note" bs=1 seek=4 count=4 conv=notrunc status=none
llvm-objcopy --update-section .note.liber.identity="$work/note" "$provider_file" 2>/dev/null || fail "could not write the mutated note back"
refuses "an identity note whose declared record runs past the section"
undo

# 6. A NAME THAT IS NOT THIS FORMAT'S. The twenty-byte header is twenty because the name is
#    "LIBER\0" padded to eight; a different name means the record is not where the reader looks.
mutate "$provider_file"
llvm-objcopy --dump-section .note.liber.identity="$work/note" "$provider_file" /dev/null
printf 'OTHER' | dd of="$work/note" bs=1 seek=12 count=5 conv=notrunc status=none
llvm-objcopy --update-section .note.liber.identity="$work/note" "$provider_file" 2>/dev/null || fail "could not write the mutated note back"
refuses "an identity note carrying another format's name"
undo

# 7. AND THE ORIGINAL DEFECT ITSELF: a provider replaced, and the consumer that records it not
#    rebuilt. One byte of the provider's record is enough to change its digest.
mutate "$provider_file"
llvm-objcopy --dump-section .note.liber.identity="$work/note" "$provider_file" /dev/null
original_byte="$(od -An -tu1 -j24 -N1 "$work/note" | tr -d ' ')"
printf "\\x$(printf '%02x' $(((original_byte + 1) % 256)))" | dd of="$work/note" bs=1 seek=24 count=1 conv=notrunc status=none
llvm-objcopy --update-section .note.liber.identity="$work/note" "$provider_file" 2>/dev/null || fail "could not write the mutated note back"
refuses "a provider whose identity changed while its consumers were not rebuilt"
undo

# 8. A PROVIDER ROW RECORDED TWICE. M2 named this case and it was never written, and the check it
#    exercises was genuinely partial: only rows that DISAGREED were reported, so a note recording one
#    provider twice with the SAME digest went through. The record is a text block of `key=value`
#    lines, so the mutation is the consumer's own provider line repeated - and the note header's
#    `descsz` has to grow with it, which is what makes this a note rather than a longer file.
mutate "$consumer"
llvm-objcopy --dump-section .note.liber.identity="$work/note" "$consumer" /dev/null
descsz="$(od -An -tu4 -j4 -N4 "$work/note" | tr -d ' ')"
dd if="$work/note" bs=1 skip=20 count="$descsz" of="$work/record" status=none
grep -a -m 1 '^provider=' "$work/record" >>"$work/record"
new_len="$(stat -c%s "$work/record")"
# The header, built the way `write_identity_note` builds it: the escapes are composed as TEXT and
# `%b` turns them into bytes, because a command substitution cannot carry the NUL bytes themselves.
escaped="$(printf '\\x06\\x00\\x00\\x00\\x%02x\\x%02x\\x%02x\\x%02x\\x01\\x00\\x00\\x00LIBER\\x00\\x00\\x00' \
	"$((new_len & 0xff))" "$(((new_len >> 8) & 0xff))" "$(((new_len >> 16) & 0xff))" "$(((new_len >> 24) & 0xff))")"
printf '%b' "$escaped" >"$work/note"
cat "$work/record" >>"$work/note"
# Padded to a four-byte boundary, as the writer does.
note_len="$(stat -c%s "$work/note")"
while ((note_len % 4 != 0)); do
	printf '\0' >>"$work/note"
	note_len=$((note_len + 1))
done
llvm-objcopy --update-section .note.liber.identity="$work/note" "$consumer" 2>/dev/null || fail "could not write the mutated note back"
refuses "an identity note recording one provider twice"
undo

# AND THE TREE IS AS IT WAS. Said rather than assumed: the restore above is what every case depends
# on, and a gate that left the tree mutated would break the next build with no explanation.
"$VERIFY" --verify-staged "$TARGET" >/dev/null 2>&1 || fail "the staged tree does not verify after this gate restored it"
echo "staged-consistency: eight mutations refused, and the tree verifies again afterwards"
