#!/bin/bash
# The C++ ABI decision matrix, enforced on the artifact rather than described in a document.
#
# THE DECISION, MECHANISM BY MECHANISM, AND WHAT DECIDED IT. Each answer is what pass 1 and pass 2
# actually NAME, not what a reader expects a C99 configuration to need:
#
#   exception tables and unwinding   FORBIDDEN. The converged surface names no `_Unwind_*`, and all
#                                     three userspace linker scripts discard `.eh_frame*` anyway - so
#                                     a C++ object that compiled would silently lose its unwind
#                                     metadata, which is worse than refusing it.
#   RTTI                            FORBIDDEN. No `_ZTI`, `_ZTS` or `_ZTV` anywhere in the closure.
#   `atexit` registration           FORBIDDEN. Neither `atexit` nor `__cxa_atexit` is named by the
#                                     converged link, so a registration would be a handler nothing
#                                     runs.
#   static initialisers             ADMITTED, and measured: exactly one constructor and one
#                                     destructor, the same two on all three targets. That mechanism
#                                     gets the supported-ABI answer and its positive gates, which is
#                                     a different deliverable from this one.
#   the `errno` model               ADMITTED, and PROCESS-WIDE, which is what the no-thread-creation
#                                     pin makes correct.
#
# A FORBIDDEN MECHANISM GETS FLAGS AND A CHECK, and this is the check. The flags are in the ported
# build; they are what stops the compiler emitting these in the first place, and this is what catches
# it if a flag, a compiler or a source ever changes that. Neither answer may be "audit it", so the
# refusal is a build failure on every target.
#
# THE MUTATION IS RUN EVERY TIME, not described. A fixture carrying an exception table, a typeinfo
# symbol and an `atexit` registration is built and fed to the same scan, which must refuse it - an
# empty result and a broken scan are indistinguishable otherwise.

set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
cd "$ROOT"

PASS2="$ROOT/.build/foreign/pass2"

fail() {
	echo "foreign-cxx-abi: $*" >&2
	exit 1
}

# THE FORBIDDEN SECTIONS AND SYMBOLS, named rather than pattern-guessed where a name will do.
FORBIDDEN_SECTIONS='\.eh_frame|\.eh_frame_hdr|\.gcc_except_table|\.ARM\.extab|\.ARM\.exidx'
FORBIDDEN_SYMBOLS='^_Unwind_|^__cxa_|^_ZT[ISV]|^atexit$|^__gxx_personality'

scan() {
	local artifact="$1" what="$2" found=0
	# CAPTURED, THEN MATCHED. `grep -q` stops at the first match and the writer takes SIGPIPE, which
	# under `pipefail` makes a FOUND section read as a failed pipeline - the exact inversion this
	# check exists to avoid.
	local sections
	sections="$(llvm-readelf -S --wide "$artifact" 2>/dev/null | grep -E " ($FORBIDDEN_SECTIONS) " || true)"
	if [[ -n "$sections" ]]; then
		echo "foreign-cxx-abi: $what carries a forbidden section:" >&2
		printf '%s\n' "$sections" >&2
		found=1
	fi
	local symbols
	symbols="$(llvm-nm "$artifact" 2>/dev/null | awk '{print $NF}' | grep -E "$FORBIDDEN_SYMBOLS" | sort -u || true)"
	if [[ -n "$symbols" ]]; then
		echo "foreign-cxx-abi: $what names a forbidden mechanism: $(tr '\n' ' ' <<<"$symbols")" >&2
		found=1
	fi
	return "$found"
}

# THE MUTATION FIRST. A check that has not been shown refusing is a check nobody can read the result
# of, and this one's whole output is an absence.
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cat >"$work/reintroduce.cpp" <<'EOF'
// Not built into anything. It exists so the scan can be shown refusing what it reports absent: a
// virtual base gives typeinfo and a vtable, a throw gives an exception table and a personality
// routine, and a static object with a destructor gives an `__cxa_atexit` registration.
struct Base {
	virtual ~Base();
	virtual int value() const;
};
struct Derived : Base {
	~Derived() override;
	int value() const override;
};
static Derived instance;
int reintroduced(const Base &base) {
	if (base.value() < 0) {
		throw base.value();
	}
	return instance.value();
}
EOF
clang++ --target=x86_64-unknown-none-elf -ffreestanding -nostdlibinc -fPIC -O0 -fexceptions -funwind-tables -frtti -c "$work/reintroduce.cpp" -o "$work/reintroduce.o" 2>/dev/null || fail "the mutation fixture does not compile, so nothing was proved about the scan"
if scan "$work/reintroduce.o" "the mutation fixture" 2>/dev/null; then
	fail "the scan accepted an object carrying exception tables, typeinfo and an atexit registration - it would accept anything"
fi
echo "foreign-cxx-abi: the scan refuses a fixture carrying every forbidden mechanism"

# THEN THE REAL ARTIFACTS, on every target.
if [[ ! -d "$PASS2" ]]; then
	echo "foreign-cxx-abi: NOT PERFORMED: there is no pass-2 output under .build/foreign/pass2, so no artifact was scanned"
	exit 0
fi
failed=0
for arch in x86_64 aarch64 riscv64; do
	elf="$PASS2/$arch/audit-vulkan.lslib"
	archive="$PASS2/$arch/libvulkan.a"
	[[ -f "$elf" && -f "$archive" ]] || fail "$arch: pass 2 produced no artifact to scan"
	clean=1
	scan "$elf" "$arch: the audit-linked ELF" || clean=0
	scan "$archive" "$arch: the ported archive" || clean=0
	if ((clean)); then
		echo "foreign-cxx-abi: $arch: no exception table, no typeinfo, no vtable, no atexit registration"
	else
		failed=1
	fi
done
((failed == 0)) || fail "a forbidden mechanism is present in the closure this milestone ships the surface of"
