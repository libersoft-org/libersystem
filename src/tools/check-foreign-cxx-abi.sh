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
# THE MUTATIONS ARE RUN EVERY TIME, not described - ONE FIXTURE PER MECHANISM, each refused for ITS
# OWN reason (2026-10-15). A single fixture carrying all of them at once proved less than it looked:
# the scan refuses an object on the first thing it finds, so a pattern that had stopped matching one
# mechanism would have been covered by the other two and the refusal would have stayed green. Each
# fixture now carries one mechanism, and the scan's report must NAME every marker that mechanism
# leaves - the section or symbol a regression would make it stop seeing. And a clean object compiled
# with the forbidding flags must be ACCEPTED, because a scan that refused everything would pass every
# mutation above.

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

# THE MUTATIONS FIRST. A check that has not been shown refusing is a check nobody can read the result
# of, and this one's whole output is an absence.
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# Compile one fixture. Everything but the mechanism under test is switched off, so the fixture
# carries that mechanism and nothing that could stand in for it.
fixture() {
	local name="$1" language="$2"
	shift 2
	local compiler=clang
	[[ "$language" == c++ ]] && compiler=clang++
	"$compiler" -x "$language" --target=x86_64-unknown-none-elf -ffreestanding -nostdlibinc -fPIC -O0 "$@" -c "$work/$name.src" -o "$work/$name.o" 2>"$work/$name.log" || {
		cat "$work/$name.log" >&2
		fail "the $name fixture does not compile, so nothing was proved about the scan"
	}
}

# The scan must refuse the fixture, and its report must name every marker the mechanism leaves.
refuses() {
	local name="$1" mechanism="$2" report marker
	shift 2
	if report="$(scan "$work/$name.o" "the $name fixture" 2>&1)"; then
		fail "the scan accepted an object carrying $mechanism - it would accept that mechanism reintroduced"
	fi
	for marker in "$@"; do
		grep -qE "(^|[[:space:]])$(sed 's/[.]/\\./g' <<<"$marker")([[:space:]]|$)" <<<"$report" || fail "the scan refused the $name fixture without naming $marker, so a scan that stopped seeing $mechanism would pass on another marker: $report"
	done
	echo "foreign-cxx-abi: the scan refuses $mechanism, naming $*"
}

# NO FLAG SWITCHES OFF MORE THAN ITS OWN MECHANISM, so each fixture is compiled with the others off.
quiet=(-fno-exceptions -fno-unwind-tables -fno-asynchronous-unwind-tables)

# 1. UNWIND TABLES WITH NOTHING THAT THROWS - what the aarch64 artifact really carried: an `.eh_frame`
#    no C source asked for and no symbol names.
cat >"$work/unwind-tables.src" <<'EOF'
/* Not built into anything: a function compiled with unwind tables and nothing else. */
int liber_cxx_unwinds(int value) { return value + 1; }
EOF
fixture unwind-tables c -funwind-tables -fasynchronous-unwind-tables
refuses unwind-tables "unwind tables" .eh_frame

# 2. EXCEPTIONS: a throw, and a cleanup the unwinder has to run on the way past it.
cat >"$work/exceptions.src" <<'EOF'
// Not built into anything: a throw gives the allocation, the throw and the personality routine; a
// local object with a destructor across a call that may throw gives the landing pad and the resume.
struct Guard {
	~Guard();
};
int liber_cxx_throws(int value) {
	if (value < 0) {
		throw value;
	}
	return value;
}
int liber_cxx_cleans_up(int value) {
	Guard guard;
	return liber_cxx_throws(value);
}
EOF
fixture exceptions c++ -fexceptions -fno-rtti
refuses exceptions "exception tables and unwinding" .gcc_except_table __cxa_throw __cxa_allocate_exception __gxx_personality_v0 _Unwind_Resume

# 3. RTTI: a polymorphic class whose key function is here, which emits its vtable and typeinfo here.
cat >"$work/rtti.src" <<'EOF'
// Not built into anything: the first non-inline virtual function is defined in this file, so this
// is the object that carries the class's vtable, its typeinfo and its type name.
struct LiberBase {
	virtual int value() const;
};
int LiberBase::value() const { return 1; }
EOF
fixture rtti c++ -frtti "${quiet[@]}"
refuses rtti "RTTI" _ZTV9LiberBase _ZTI9LiberBase _ZTS9LiberBase

# 4. AN `atexit` REGISTRATION FROM C - the mechanism by its C name, which no C++ fixture produces.
cat >"$work/atexit.src" <<'EOF'
/* Not built into anything: a handler registered to run at exit. */
extern int atexit(void (*handler)(void));
static void liber_cxx_at_exit(void) {}
int liber_cxx_registers(void) { return atexit(liber_cxx_at_exit); }
EOF
fixture atexit c "${quiet[@]}"
refuses atexit "an atexit registration" atexit

# 5. AND ITS C++ FORM: a static object with a destructor, which registers it with `__cxa_atexit`.
cat >"$work/static-destructor.src" <<'EOF'
// Not built into anything: a namespace-scope object whose destructor must run at exit.
struct LiberHeld {
	~LiberHeld();
};
static LiberHeld liber_held;
EOF
fixture static-destructor c++ -fno-rtti "${quiet[@]}"
refuses static-destructor "an atexit registration from a static destructor" __cxa_atexit

# 6. THE CONTROL: a plain object compiled with the forbidding flags must PASS.
cat >"$work/clean.src" <<'EOF'
/* Not built into anything: the shapes above with none of the mechanisms. */
int liber_cxx_plain(int value) { return value + 1; }
EOF
fixture clean c "${quiet[@]}"
if ! report="$(scan "$work/clean.o" "the clean fixture" 2>&1)"; then
	fail "the scan refused a clean object, so its refusals above say nothing: $report"
fi
echo "foreign-cxx-abi: the scan accepts a clean object compiled with the forbidding flags"

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
