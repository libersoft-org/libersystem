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

# THE PUBLIC ARCHITECTURE NAME, THROUGH THE ONE MAPPING THAT KNOWS IT. `${TARGET%%-*}` is right for
# two targets of three and prints `--arch riscv64gc` for the third - the exact nonexistent command
# this milestone removed from the other diagnostic, reintroduced by the gate added to prove it gone.
# Asked of `build-shared.sh` rather than copied here: a second copy of a mapping is the thing that
# rots, and the point is that both messages come from one answer.
public_arch() { "$VERIFY" --public-arch "$1"; }

# ALL THREE MAPPINGS, ASSERTED. M3 asks for the mapping to be covered by a test, and its only other
# caller is a diagnostic that never runs on a healthy build - so nothing would have noticed it rot.
for pair in x86_64-unknown-none:x86_64 aarch64-unknown-none:aarch64 riscv64gc-unknown-none-elf:riscv64; do
	got="$(public_arch "${pair%%:*}")"
	[[ "$got" == "${pair##*:}" ]] || fail "public_arch ${pair%%:*} said '$got' and the public name is '${pair##*:}' - a build command printed to somebody whose build just failed has to name one that exists"
done
echo "staged-consistency: the three target triples map to the three public --arch names"

[[ -d "$LIB" ]] || fail "no staged tree at $LIB - run ./build.sh --arch $(public_arch "$TARGET") first"
command -v llvm-objcopy >/dev/null || fail "llvm-objcopy is required by this gate"

work="$(mktemp -d)"
victim=""
# EVERY MUTATION THIS GATE MAKES IS REGISTERED HERE, not only the single-file one.
#
# `victim` covers the cases that overwrite one artifact and put the original back. Two later cases
# MOVE things - the whole staged tree, and one unreferenced library - and neither told this handler,
# so an unexpected acceptance, a signal or any other early exit ran `rm -rf "$work"` over the only
# copy and left the staged tree empty or a library gone. That is shared build output, and the failure
# path is exactly where it happens: `refuses` exits non-zero when a mutation is ACCEPTED, which is
# the case this gate exists to catch.
#
# `moved` is a list of `source<TAB>destination` pairs, replayed in reverse.
moved=()
moved_aside() {
	moved+=("$1	$2")
}
# A FAILED RESTORE IS REPORTED AND ITS COPY IS KEPT, which this used to swallow.
#
# `mv ... || true` hid a restore that did not happen, `moved=()` then forgot the pair, and `restore`
# deleted the directory holding the only copy - so a gate that could not put the tree back exited
# cleanly having destroyed it. `moved_back` now reports the count it could not restore, and it clears
# only the pairs that DID restore; `restore` keeps `$work` when anything is outstanding and says
# where it is.
moved_back() {
	local at pair from to
	local kept=()
	for ((at = ${#moved[@]} - 1; at >= 0; at--)); do
		pair="${moved[at]}"
		from="${pair%%	*}"
		to="${pair##*	}"
		[[ -e "$from" ]] || continue
		rm -rf "$to"
		if ! mv "$from" "$to" 2>/dev/null; then
			echo "staged-consistency: could not restore $to from $from" >&2
			kept+=("$pair")
		fi
	done
	moved=("${kept[@]}")
	((${#moved[@]} == 0))
}
restore() {
	local ok=0
	if [[ -n "$victim" && -f "$work/original" ]]; then
		cp -p "$work/original" "$victim" || ok=1
	fi
	moved_back || ok=1
	if ((ok == 0)); then
		rm -rf "$work"
	else
		echo "staged-consistency: the staged tree was NOT fully restored - the recovery copies are kept at $work" >&2
	fi
	return 0
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

# `$2`, WHERE IT MATTERS, IS THE REASON. A mutation refused for a DIFFERENT reason than the one it
# was made for is a case that has stopped testing its own subject - which is how a gate keeps passing
# while the rule it names quietly stops being checked.
# HOW MANY MUTATIONS ACTUALLY RAN, so the closing line reports a number it counted.
#
# It said "eleven mutations refused" unconditionally, and three of the cases have a subject only on
# some images - a consumer that already records every staged library, a tree with no unreferenced
# one. On such an image the gate skipped them, said eleven anyway, and a case that stopped running
# altogether would never be noticed.
# EVERY NAMED CASE MUST ACTUALLY RUN, and this is the ledger that says which did.
#
# The closing line counted refusals and exited zero, so a run in which three topology-dependent cases
# found no subject printed "9 mutation(s) refused" and passed - the same words a complete run prints,
# one number apart, and nobody reads a number they have no expectation for. A case that did not run
# is not a case that passed, and the promised set is fixed rather than discovered: the gate knows
# what it means to have run.
REQUIRED_CASES=(
	truncated-note
	missing-provider
	wrong-architecture
	corrupt-digest
	foreign-note
	absent-note
	unreadable-note
	duplicate-provider
	empty-tree
	unreferenced-library
	undeclared-edge
	missing-edge
)
ran_cases=()
ran() { ran_cases+=("$1"); }

refused_count=0
refuses() {
	local what="$1" because="${2:-}"
	if "$VERIFY" --verify-staged "$TARGET" >"$work/out" 2>&1; then
		echo "staged-consistency: $what was ACCEPTED" >&2
		sed -n '1,10p' "$work/out" >&2
		exit 1
	fi
	if [[ -n "$because" ]] && ! grep -aq "$because" "$work/out"; then
		echo "staged-consistency: $what was refused, but not for the reason it was made for" >&2
		echo "staged-consistency:   wanted: $because" >&2
		sed -n '1,6p' "$work/out" >&2
		exit 1
	fi
	refused_count=$((refused_count + 1))
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
ran missing-provider
undo

# 2. THE PROVIDER HAS NO IDENTITY SECTION. Unreadable rather than absent, which used to become an
#    empty digest and then be skipped for being empty.
mutate "$provider_file"
llvm-objcopy --remove-section .note.liber.identity "$provider_file" 2>/dev/null || fail "could not remove the identity section"
refuses "a staged provider carrying no identity note"
ran absent-note
undo

# 3. THE CONSUMER HAS NO IDENTITY SECTION. The other side of the same branch.
mutate "$consumer"
llvm-objcopy --remove-section .note.liber.identity "$consumer" 2>/dev/null || fail "could not remove the identity section"
refuses "a staged library carrying no identity note"
ran unreadable-note
undo

# 4. A DECLARED LENGTH OF ZERO. The header says the record is empty; there is no identity in an
#    empty record.
mutate "$provider_file"
llvm-objcopy --dump-section .note.liber.identity="$work/note" "$provider_file" /dev/null
printf '\x00\x00\x00\x00' | dd of="$work/note" bs=1 seek=4 count=4 conv=notrunc status=none
llvm-objcopy --update-section .note.liber.identity="$work/note" "$provider_file" 2>/dev/null || fail "could not write the mutated note back"
refuses "an identity note declaring a zero-length record"
ran truncated-note
undo

# 5. A DECLARED LENGTH PAST THE END OF THE SECTION. The header describes bytes the file does not
#    contain, and the digest used to be taken over whatever followed.
mutate "$provider_file"
llvm-objcopy --dump-section .note.liber.identity="$work/note" "$provider_file" /dev/null
printf '\xff\xff\x00\x00' | dd of="$work/note" bs=1 seek=4 count=4 conv=notrunc status=none
llvm-objcopy --update-section .note.liber.identity="$work/note" "$provider_file" 2>/dev/null || fail "could not write the mutated note back"
refuses "an identity note whose declared record runs past the section"
ran corrupt-digest
undo

# 6. A NAME THAT IS NOT THIS FORMAT'S. The twenty-byte header is twenty because the name is
#    "LIBER\0" padded to eight; a different name means the record is not where the reader looks.
mutate "$provider_file"
llvm-objcopy --dump-section .note.liber.identity="$work/note" "$provider_file" /dev/null
printf 'OTHER' | dd of="$work/note" bs=1 seek=12 count=5 conv=notrunc status=none
llvm-objcopy --update-section .note.liber.identity="$work/note" "$provider_file" 2>/dev/null || fail "could not write the mutated note back"
refuses "an identity note carrying another format's name"
ran foreign-note
undo

# 7. AND THE ORIGINAL DEFECT ITSELF: a provider replaced, and the consumer that records it not
#    rebuilt. One byte of the provider's record is enough to change its digest.
mutate "$provider_file"
llvm-objcopy --dump-section .note.liber.identity="$work/note" "$provider_file" /dev/null
original_byte="$(od -An -tu1 -j24 -N1 "$work/note" | tr -d ' ')"
printf "\\x$(printf '%02x' $(((original_byte + 1) % 256)))" | dd of="$work/note" bs=1 seek=24 count=1 conv=notrunc status=none
llvm-objcopy --update-section .note.liber.identity="$work/note" "$provider_file" 2>/dev/null || fail "could not write the mutated note back"
refuses "a provider whose identity changed while its consumers were not rebuilt"
ran wrong-architecture
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
ran duplicate-provider
undo

# 9. AN EMPTY STAGED TREE. Every case above mutates something that is THERE, and the check used to
#    iterate over whatever `.lslib` files it happened to find - so a readable but empty `lib/` ran
#    both loops zero times and reported that every staged library named the providers beside it. A
#    check entirely relative to its own input cannot say anything is missing.
# THE WHOLE DIRECTORY ASIDE, not a glob: the libraries live in per-owner subdirectories, so
# `$LIB/*.lslib` names nothing and a mutation that moves nothing tests nothing.
holding="$work/held"
rm -rf "$holding"
# REGISTERED BEFORE THE MOVE, NOT AFTER IT. "Before the mutation is tested" was not early enough:
# between `mv` and `moved_aside` the ledger knew nothing, so an interruption in that window ran an
# EXIT handler that deleted `$work` - and `$work` was where the only copy of the staged tree had just
# been put. The registration is free and the move is not, so the free one goes first.
moved_aside "$holding" "$LIB"
mv "$LIB" "$holding" || fail "could not move the staged tree aside"
mkdir -p "$LIB"
refuses "a readable but empty staged tree" "an empty tree is not a consistent one"
ran empty-tree
moved_back

# 10. ONE EXPECTED LIBRARY MISSING, AND NOTHING RECORDING IT. Case 1 removes a provider some consumer
#     names, so the consumer's own note reports it. This removes a library whose absence no remaining
#     note mentions - which the manifest is the only thing that can notice.
unreferenced=""
while IFS= read -r candidate; do
	name="$(basename "$candidate" .lslib)"
	if ! grep -aqr "provider=$name:" "$LIB" 2>/dev/null; then
		unreferenced="$candidate"
		break
	fi
done < <(find "$LIB" -name '*.lslib' -type f | sort)
if [[ -n "$unreferenced" ]]; then
	# BY ITS OWN PATH, because it lives in a per-owner subdirectory and putting it back at the top
	# would leave the tree consistent-looking and wrong.
	# Registered first, for the reason case 9 states: the window between the move and the ledger is a
	# window in which the recovery copy is unknown to the handler that deletes the directory it is in.
	moved_aside "$work/unreferenced.lslib" "$unreferenced"
	mv "$unreferenced" "$work/unreferenced.lslib" || fail "could not move the unreferenced library aside"
	refuses "an expected library missing from the staged tree that no remaining note names" "and it is not staged"
	ran unreferenced-library
	moved_back
else
	echo "staged-consistency:   every staged library is named by some note, so the unreferenced case has no subject in this image"
fi

# 11. A PROVIDER SET THAT IS NOT THE MANIFEST'S. Case 7 replaces a provider's BYTES so the digests
#     disagree; this records an edge the manifest does not declare at all, which every digest
#     comparison passes because the edge names a library that really is staged with that digest.
mutate "$consumer"
llvm-objcopy --dump-section .note.liber.identity="$work/note" "$consumer" /dev/null 2>/dev/null || fail "could not read the consumer's note"
foreign=""
while IFS= read -r candidate; do
	name="$(basename "$candidate" .lslib)"
	if [[ "$name" != "$provider" ]] && ! grep -aq "provider=$name:" "$work/note"; then
		foreign="$name"
		break
	fi
done < <(find "$LIB" -name '*.lslib' -type f | sort)
if [[ -n "$foreign" ]]; then
	# THE DIGEST THE BUILD RECORDED FOR IT, taken from a note that already names it.
	#
	# A library's own identity digest is a hash of its note's descriptor bytes, not a string inside
	# the note - so it cannot be grepped out of the file. Some other consumer records this provider
	# with that digest, and reading it from there is the same number by construction. It has to be
	# the RIGHT one: with a wrong digest the older recorded-versus-staged check fires first and this
	# case would pass for a reason that is not the one it is named for.
	#
	# `|| true` ON EVERY STAGE. Under `set -euo pipefail` a `grep` that matches nothing ends the
	# script, and a gate that dies where a case has no subject reports a failure it did not find.
	foreign_digest="$(grep -a -h -o "provider=$foreign:[0-9a-f]\{64\}" -r "$LIB" 2>/dev/null | sed -n '1s/.*://p' || true)"
	if [[ -n "$foreign_digest" ]]; then
		printf 'provider=%s:%s\n' "$foreign" "$foreign_digest" >>"$work/note"
		while (($(stat -c %s "$work/note") % 4 != 0)); do
			printf '\0' >>"$work/note"
		done
		true
		llvm-objcopy --update-section .note.liber.identity="$work/note" "$consumer" 2>/dev/null || fail "could not write the mutated note back"
		refuses "an identity note recording an edge the manifest does not declare" "the manifest declares no such dependency"
		ran undeclared-edge
	else
		echo "staged-consistency:   no readable foreign digest, so the undeclared-edge case has no subject"
	fi
	undo
else
	echo "staged-consistency:   the consumer already records every staged library, so the undeclared-edge case has no subject"
	undo
fi

# 12. AN EDGE THE MANIFEST DECLARES AND THE NOTE DOES NOT RECORD - the other direction of case 11,
#     and the one the verifier's reverse check exists for.
#
#     Case 11 adds an edge nothing declared. This REMOVES a declared one from the consumer's note
#     while both artifacts stay staged, which is what a library rebuilt without one of its providers
#     looks like: it records fewer edges, and every edge it does record still checks out. Only the
#     manifest can notice, and only in this direction - so without this case the reverse branch could
#     be deleted and the gate would stay green.
# THE FIRST LINE TAKEN BY EXPANSION, NOT BY `head`. `head` closes its input, and under `pipefail`
# that reads as a failed pipeline - a check whose result depends on how much the previous stage had
# left to write.
declared="$(src/tools/system-manifest.sh export-json 2>/dev/null | jq -r --arg artifact "$(basename "$consumer" .lslib)" '.libraries[$artifact].providers[]? // empty' 2>/dev/null || true)"
declared="${declared%%$'\n'*}"
if [[ -n "$declared" ]]; then
	mutate "$consumer"
	llvm-objcopy --dump-section .note.liber.identity="$work/note" "$consumer" /dev/null 2>/dev/null || fail "could not read the consumer's note"
	# THE ROW OUT, AND THE NOTE STILL WELL FORMED. The descriptor is a sequence of NUL-padded rows,
	# so the row is replaced by padding of its own length rather than deleted - a shorter section
	# would be refused for being malformed, which is a different case with a different name.
	# NO NESTED BLOCK IN THE PYTHON. `<<-` strips leading TABS so this can sit at the shell's own
	# indentation, and it strips Python's with them - an `if:` body written here loses the
	# indentation that makes it a body. A conditional EXPRESSION needs none.
	python3 - "$work/note" "$declared" <<-'PYEOF'
		import re, sys
		path, provider = sys.argv[1], sys.argv[2]
		data = open(path, 'rb').read()
		row = re.search(rb'provider=' + re.escape(provider.encode()) + rb':[0-9a-f]{64}', data)
		sys.exit(3) if row is None else open(path, 'wb').write(data[: row.start()] + b'\0' * (row.end() - row.start()) + data[row.end() :])
	PYEOF
	case $? in
	0)
		llvm-objcopy --update-section .note.liber.identity="$work/note" "$consumer" 2>/dev/null || fail "could not write the mutated note back"
		refuses "an identity note that does not record an edge the manifest declares" "records no such edge"
		ran missing-edge
		;;
	3) echo "staged-consistency:   the consumer's note does not record its declared provider, so the missing-edge case has no subject" ;;
	*) fail "could not remove a declared edge from the note" ;;
	esac
	undo
else
	echo "staged-consistency:   the manifest declares no provider for this consumer, so the missing-edge case has no subject"
fi

# AND THE TREE IS AS IT WAS. Said rather than assumed: the restore above is what every case depends
# on, and a gate that left the tree mutated would break the next build with no explanation.
"$VERIFY" --verify-staged "$TARGET" >/dev/null 2>&1 || fail "the staged tree does not verify after this gate restored it"
# AND THE PROMISED SET WAS ACTUALLY EXERCISED. A case with no subject is a gate that did not run,
# and it fails here naming the ones that did not - so "the image has no subject for it" becomes a
# thing somebody fixes rather than a line nobody reads.
missing_cases=()
for case_name in "${REQUIRED_CASES[@]}"; do
	found=0
	for done_name in ${ran_cases[@]+"${ran_cases[@]}"}; do
		[[ "$done_name" == "$case_name" ]] && {
			found=1
			break
		}
	done
	((found)) || missing_cases+=("$case_name")
done
if ((${#missing_cases[@]} > 0)); then
	echo "staged-consistency: ${#missing_cases[@]} of ${#REQUIRED_CASES[@]} named mutations did not run: ${missing_cases[*]}" >&2
	echo "staged-consistency:   a case with no subject in this image is a case this gate did not test - construct a subject for it or remove it from REQUIRED_CASES with a reason" >&2
	exit 1
fi
echo "staged-consistency: all ${#REQUIRED_CASES[@]} named mutations refused ($refused_count refusal(s)), and the tree verifies again afterwards"
