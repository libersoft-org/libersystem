#!/bin/bash
# THE AUDIT-LINKED ARTIFACT ITSELF, through the generic checks, as a file.
#
# WHY THIS EXISTS SEPARATELY FROM THE STAGING. This milestone previously exempted the one artifact
# whose surface it claims: only a synthetic artifact was checked, which would have let it close with
# a substrate derived from an ELF the later importer immediately rejects. So the checks are run on
# this file, before and independently of any staging, and no production manifest names it.
#
# THE FIVE CHECKS ARE THE ONES EVERY OTHER ARTIFACT PASSES, and nothing here is a parallel audit -
# a parallel audit is the thing that drifts. They are:
#
#   RELOCATION FORMS    only the forms the runtime loader and the packager share an allowlist for.
#                         Every other form, thread-local ones included, rejects an artifact before it
#                         reaches a volume.
#   W^X                 no loadable segment is both writable and executable.
#   DYNAMIC METADATA    the dynamic table, its symbols and its strings are readable - an image whose
#                         metadata cannot be walked is not something to discover at load time.
#   IDENTITY            a well-formed v2 record whose language section is the foreign one, carrying
#                         each tool, the sysroot, the configure inputs, the objects, the patch series
#                         and the LICENCE the pinned upstream is taken under.
#   PROVIDER CLOSURE    the record accounts for exactly the artifact's own `DT_NEEDED` set. A record
#                         that left an edge out would be refused by the same check that refuses a
#                         forged one.
#   EXPORT COLLISION    no symbol it defines is one a staged provider already owns.

set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
cd "$ROOT"

PASS2="$ROOT/.build/foreign/pass2"

fail() {
	echo "foreign-audit-artifact: $*" >&2
	exit 1
}

if [[ ! -d "$PASS2" ]]; then
	echo "foreign-audit-artifact: NOT PERFORMED: there is no pass-2 output under .build/foreign/pass2, so no artifact was checked"
	exit 0
fi

declare -A TRIPLE=([x86_64]=x86_64-unknown-none [aarch64]=aarch64-unknown-none [riscv64]=riscv64gc-unknown-none-elf)

# THE ALLOWLIST IS READ FROM THE ONE PLACE THAT DEFINES IT rather than restated here: the runtime
# loader and the packager both decide with `bootproto::elf::dynamic_relocation_kind`, and a copy in
# this script would be a second policy that agrees until it does not.
policy="$(python3 "$HERE/foreign-relocation-allowlist.py" "$ROOT/src/boot/protocol/src/elf.rs")" || fail "the relocation policy could not be read from bootproto"

for arch in x86_64 aarch64 riscv64; do
	elf="$PASS2/$arch/audit-vulkan.lslib"
	[[ -f "$elf" ]] || fail "$arch: pass 2 produced no artifact to check"

	# 1. THE RELOCATION FORMS, BY NUMBER. The type is the low half of each entry-s info field, which
	#    is what the allowlist is written in - reading the NAME would mean mapping three
	#    architectures- spellings back to it here, which is the second policy this avoids.
	admitted="$(awk -v want="$arch" '$1 == want { $1 = ""; print }' <<<"$policy")"
	used="$(llvm-readelf -r --wide "$elf" | awk 'NF >= 3 && $1 ~ /^[0-9a-f]+$/ { print strtonum("0x" substr($2, length($2) - 7)) }' | sort -un)"
	[[ -n "$used" ]] || fail "$arch: the artifact carries no dynamic relocation at all, which a position-independent image cannot be"
	for form in $used; do
		grep -qw "$form" <<<"$admitted" || fail "$arch: the artifact uses relocation type $form, which is outside the loader allowlist [$admitted ]"
	done
	echo "foreign-audit-artifact: $arch: $(wc -w <<<"$used") relocation type(s), all inside the allowlist"

	# 2. W^X.
	if ! llvm-readelf -l --wide "$elf" | awk '$1 == "LOAD" && $0 ~ /W/ && $0 ~ /E/ { bad = 1 } END { exit bad }'; then
		fail "$arch: the artifact contains a writable executable segment"
	fi

	# 3. THE DYNAMIC METADATA.
	dynamic="$(llvm-readelf -d --wide "$elf")"
	for tag in SYMTAB STRTAB HASH; do
		grep -qF "($tag)" <<<"$dynamic" || fail "$arch: the artifact has no DT_$tag - its metadata cannot be walked"
	done
	if grep -Eq '\((RPATH|RUNPATH|TEXTREL)\)' <<<"$dynamic"; then
		fail "$arch: the artifact carries a forbidden dynamic-loader contract"
	fi

	# 4. THE IDENTITY RECORD.
	record="$(llvm-objcopy --dump-section .note.liber.identity=/dev/stdout "$elf" /dev/null 2>/dev/null | tail -c +21 | tr -d '\0')"
	[[ -n "$record" ]] || fail "$arch: the artifact carries no identity record"
	grep -qx "format=liber-image-identity-v2" <<<"$record" || fail "$arch: the record is not the format this system reads"
	grep -qx "language=foreign" <<<"$record" || fail "$arch: the record does not declare a foreign producer"
	grep -qx "target=${TRIPLE[$arch]}" <<<"$record" || fail "$arch: the record names a different target"
	for field in compiler-sha256 archiver-sha256 linker-sha256 sysroot-sha256 configure-sha256 objects-sha256 patches-sha256; do
		grep -qE "^$field=[0-9a-f]{64}$" <<<"$record" || fail "$arch: the record has no well-formed $field"
	done
	grep -qx "licence=Apache-2.0" <<<"$record" || fail "$arch: the record does not carry the licence the pinned upstream is taken under"

	# 5. THE PROVIDER CLOSURE: exactly the edges the artifact has, and no others.
	needed="$(sed -n 's/.*Shared library: \[\([^]]*\)\].*/\1/p' <<<"$dynamic" | sort -u)"
	recorded="$(sed -n 's/^provider=\([a-z0-9_-]*\):[0-9a-f]\{64\}$/\1.lslib/p' <<<"$record" | sort -u)"
	[[ "$needed" == "$recorded" ]] || fail "$arch: the record accounts for [$recorded] and the artifact needs [$needed]"
	echo "foreign-audit-artifact: $arch: the record accounts for exactly its provider closure"

	# 6. EXPORT COLLISION against what is staged. The artifact is not installed, so this asks the
	#    question an importer would ask: would adding it to an image give a symbol two owners?
	staged="$ROOT/.build/image/${TRIPLE[$arch]}/lib"
	if [[ -d "$staged" ]]; then
		mine="$(llvm-readelf --wide --dyn-syms "$elf" | awk '$7 != "UND" && $8 != "" && $8 != "Name" {print $8}' | sort -u)"
		while IFS= read -r provider; do
			theirs="$(llvm-readelf --wide --dyn-syms "$provider" | awk '$7 != "UND" && $8 != "" && $8 != "Name" {print $8}' | sort -u)"
			collision="$(comm -12 <(printf '%s\n' "$mine") <(printf '%s\n' "$theirs"))"
			[[ -z "$collision" ]] || fail "$arch: the artifact and $(basename "$provider") both define: $(tr '\n' ' ' <<<"$collision")"
		done < <(find "$staged" -name '*.lslib' -type f | sort)
		echo "foreign-audit-artifact: $arch: no export it defines is one a staged provider already owns"
	else
		echo "foreign-audit-artifact: $arch: NOT PERFORMED: no staged tree, so the export-collision question was not asked"
	fi
done
