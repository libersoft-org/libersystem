#!/bin/bash
# The facilities crate provides EXACTLY what the derived inventory names, and nothing beyond it.
#
# WHY THIS IS A GATE AND NOT A REVIEW NOTE. "Do not grow a general POSIX layer" is a rule about a
# direction of travel, and every individual step along it looks reasonable: one more string function
# because a source needed it, one more file call because it was easier than deciding. The rule only
# holds if adding a symbol the inventory does not name FAILS.
#
# BOTH DIRECTIONS MATTER, and they fail differently:
#   a symbol provided and not named    the POSIX layer arriving one function at a time
#   a symbol named and not provided    a link that fails after every other question is answered
#
# TWO CRATES, ONE SURFACE. Thirty-seven of the inventory's symbols are translations of a C function
# and live in `foreign-abi`; the other twenty-one - opening a file, reading a directory, asking for a
# user id, finding and loading a provider - are decisions about what a provider IS on this system,
# and live in `foreign-discovery`. The split is checked rather than described, in both directions and
# for both crates: a symbol in the wrong one is the discovery policy leaking into a C library, or a C
# library quietly answering a discovery question.

set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
cd "$ROOT"

# PASS 2 IS THE AUTHORITY, NOT PASS 1. The candidate surface over-states what a system must provide -
# a per-object undefined symbol may be satisfied by another upstream object - so the set a crate is
# checked against is what the CONVERGED LINK resolved. Pass 1 is where the question was first asked
# and is not where it is answered; when pass 2 has not run yet, this falls back to it and says so.
INVENTORY="src/foreign/INVENTORY-pass2.json"
INVENTORY_PASS="2"
if [[ ! -f "$INVENTORY" ]]; then
	INVENTORY="src/foreign/INVENTORY-pass1.json"
	INVENTORY_PASS="1"
fi
FACILITIES="src/user/libs/foreign/abi"
DISCOVERY="src/user/libs/foreign/discovery"

fail() {
	echo "foreign-facilities: $*" >&2
	exit 1
}

[[ -f "$INVENTORY" ]] || fail "$INVENTORY is missing - there is nothing to check the crates against"
[[ -d "$FACILITIES" ]] || fail "$FACILITIES is missing"
[[ -d "$DISCOVERY" ]] || fail "$DISCOVERY is missing"

python3 - "$INVENTORY" "$FACILITIES" "$DISCOVERY" "$INVENTORY_PASS" <<'PY' || exit 1
import json
import pathlib
import re
import sys

inventory = json.load(open(sys.argv[1]))
facilities = pathlib.Path(sys.argv[2]) / "src"
discovery = pathlib.Path(sys.argv[3]) / "src"

# THE THREE TARGETS MUST AGREE ABOUT WHAT THEY NEED. They do today - one C99 configuration with no
# per-architecture sources - and if they ever stop, this crate cannot be one crate.
# WHAT THE LINK RESOLVED FROM THE SUBSTRATE, and not the whole surface the archive asks for. The two
# differ by the four memory functions the RUNTIME owns in this image: `lsrt.lslib` publishes them so
# every library can import them, and a second definition here would be an export with two owners -
# which is exactly what the generic check on the audit-linked artifact refused. Pass 1 has no such
# distinction to make, so it falls back to its undefined closure.
key = "undefined" if sys.argv[4] == "1" else "resolved_from_substrate"
surfaces = {arch: set(inventory[arch][key]) for arch in ("x86_64", "aarch64", "riscv64")}
named = set.union(*surfaces.values())
if len(set(map(frozenset, surfaces.values()))) != 1:
	print("foreign-facilities: the three targets name different symbols; one crate cannot serve them", file=sys.stderr)
	raise SystemExit(1)

# THE DISCOVERY ITEM'S SHARE, written down. Each of these is a decision about what a provider or a
# configuration directory IS on this system, which is that item's question and not a C translation.
# THE ENVIRONMENT AND THE IDENTITY QUERIES ARE GONE, and that is the platform port showing through
# rather than a trim: the ported loader never reads an environment, so `getenv` and the four id
# queries `is_high_integrity` needed are symbols the converged link does not ask for - and a symbol
# nothing requires is not built.
# THE SUBSTRATE'S OWN CONTROL SURFACE, which is not inventory surface and is named here so the rule
# above cannot quietly absorb it. Every other symbol in these crates exists because the pinned
# configuration REFERENCES it; these two exist so a LAUNCH can hand the substrate what only a launch
# knows - its diagnostic sink, and which provider was bound into the closure. Nothing upstream names
# them, so the exact-surface rule would report them as built and unrequired, and removing them would
# leave the substrate with no way to be told anything.
#
# THEY ARE REQUIRED TO EXIST, both ways, for the same reason the rest of the surface is: a substrate
# that lost its installer would fail at the first foreign call with a diagnostic nobody installed.
INTERFACE = {
	"liber_foreign_install_sink": "foreign-abi",
	"liber_foreign_install_icd": "foreign-discovery",
}

DISCOVERY = {
	"closedir", "opendir", "readdir", "fclose", "fileno", "fopen", "fread", "fstat", "dladdr",
	"loader_platform_close_library", "loader_platform_executable_path", "loader_platform_file_exists",
	"loader_platform_get_proc_address", "loader_platform_is_path_absolute",
	"loader_platform_open_library", "loader_platform_open_library_error",
}

# BOTH SPELLINGS, AND INDENTED OR NOT. The variadic entry points live inside a `cfg`-gated module,
# so they are indented; a pattern anchored at the line start reported all three as missing.
export = re.compile(r'^[ \t]*#\[(?:cfg_attr\(not\(test\), )?unsafe\(no_mangle\)\)?\]\s*\n(?:[ \t]*#\[[^\]]*\]\s*\n)*[ \t]*pub (?:unsafe )?(?:extern "C" )?(?:fn|static mut) (\w+)', re.M)
def exported_by(root):
	found = set()
	for source in sorted(root.glob("*.rs")):
		found.update(match.group(1) for match in export.finditer(source.read_text()))
	# The sentinel `stderr` points at is the facilities crate's own and is not a C library name.
	found.discard("__liber_stderr_stream")
	return found


crates = {
	"foreign-abi": (exported_by(facilities) - set(INTERFACE), named - DISCOVERY),
	"foreign-discovery": (exported_by(discovery) - set(INTERFACE), named & DISCOVERY),
}

failed = False
# AND THE CONTROL SURFACE ITSELF EXISTS. Exempting it from the exact-surface rule without requiring
# it would make "not inventory surface" mean "not checked at all".
for symbol, owner in sorted(INTERFACE.items()):
	if symbol not in exported_by(facilities if owner == "foreign-abi" else discovery):
		failed = True
		print(f"foreign-facilities: {owner} does not export <{symbol}>, which is how a launch hands the substrate what only a launch knows", file=sys.stderr)

for crate, (provided, owed) in sorted(crates.items()):
	missing = sorted(owed - provided)
	extra = sorted(provided - owed)
	if missing:
		failed = True
		print(f"foreign-facilities: {crate}: the inventory names {len(missing)} symbol(s) it does not provide, and nothing else will:", file=sys.stderr)
		for name in missing:
			print(f"foreign-facilities:     {name}", file=sys.stderr)
	if extra:
		failed = True
		# A SYMBOL IN THE WRONG CRATE IS NOT A TIDINESS QUESTION. In `foreign-abi` it is a C library
		# answering a discovery question; in `foreign-discovery` it is the discovery policy growing a
		# general POSIX layer. Both are the direction of travel this rule exists to stop.
		print(f"foreign-facilities: {crate}: provides {len(extra)} symbol(s) that are not its share of the inventory:", file=sys.stderr)
		for name in extra:
			print(f"foreign-facilities:     {name}", file=sys.stderr)

if failed:
	raise SystemExit(1)

total = sum(len(provided) for provided, _ in crates.values())
for crate, (provided, owed) in sorted(crates.items()):
	print(f"foreign-facilities: {crate}: {len(provided)} symbol(s), exactly its share of the inventory")
print(f"foreign-facilities: {total} of the {len(named)} symbol(s) pass {sys.argv[4]} derived are provided, and none beyond them")
PY
