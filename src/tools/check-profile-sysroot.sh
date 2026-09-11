#!/bin/bash
# The profile sysroot declares EXACTLY what the substrate provides, and compiles the same objects.
#
# WHY THIS IS A GATE. The profile sysroot's rule is an inversion of the bootstrap one - it is sized by
# the measured inventory rather than by the option set - and an inversion nobody checks decays back
# into what it inverted. The specific decay is one declaration at a time: a source needs a function,
# the fastest fix is to declare it, the compile succeeds, and the link failure arrives weeks later
# with nobody able to say who decided the substrate should have that symbol.
#
# THREE CLAIMS, EACH FAILING ON ITS OWN:
#
#   DECLARED IMPLIES PROVIDED     every function this sysroot declares has a symbol in the substrate
#                                   crates. A declaration without one is the POSIX layer arriving
#                                   through the include path.
#   PROVIDED IMPLIES DECLARED     every symbol the inventory names is declared here, or is one of the
#                                   twelve the loader's own patched headers declare. A symbol nothing
#                                   declares is a substrate carrying code no configuration asks for.
#   THE TRIM CHANGED NOTHING      the pinned configuration compiled against this sysroot produces the
#                                   archives the static-target pin froze, digest for digest, on all
#                                   three targets. That is what proves the forty-three declarations
#                                   removed from the bootstrap set were surface nobody required -
#                                   and it is the only form of proof that does not rely on reading
#                                   the sources correctly.
#
# THE THIRD NEEDS THE AUDIT-ONLY UPSTREAM, which lives under `.build/` and is not vendored. With the
# sources absent this check runs the first two and says the third was not performed. It never assumes
# it.

set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
cd "$ROOT"

SYSROOT="src/foreign/profile-sysroot"
INVENTORY="src/foreign/INVENTORY-pass1.json"
STATIC_PIN="src/foreign/PIN-static-target.toml"

fail() {
	echo "profile-sysroot: $*" >&2
	exit 1
}

[[ -d "$SYSROOT/include" ]] || fail "$SYSROOT/include is missing"
[[ -f "$SYSROOT/sysroot.toml" ]] || fail "$SYSROOT/sysroot.toml is missing - the sysroot has to say what it is"
[[ -f "$INVENTORY" ]] || fail "$INVENTORY is missing - there is nothing to size the sysroot by"

python3 - "$SYSROOT" "$INVENTORY" <<'PY' || exit 1
import json
import pathlib
import re
import sys
import tomllib

sysroot = pathlib.Path(sys.argv[1])
inventory = json.load(open(sys.argv[2]))
identity = tomllib.load(open(sysroot / "sysroot.toml", "rb"))
failures = []

# THE THREE TARGETS MUST AGREE, as they do for the facilities crates and for the same reason: one
# sysroot cannot serve three different surfaces.
surfaces = {arch: set(inventory[arch]["undefined"]) for arch in ("x86_64", "aarch64", "riscv64")}
if len({frozenset(surface) for surface in surfaces.values()}) != 1:
	print("profile-sysroot: the three targets name different symbols; one sysroot cannot serve them", file=sys.stderr)
	raise SystemExit(1)
named = set(surfaces["x86_64"])

# WHAT THE HEADERS DECLARE. A prototype, or an `extern` object - `stderr` is an object and is as much
# a symbol as any function. Macros and types are not declarations of anything the substrate provides.
prototype = re.compile(r"^\s*(?:extern\s+)?[A-Za-z_][\w \t\*]*?\b(\w+)\s*\(", re.M)
obj = re.compile(r"^\s*extern\s+[\w \t\*]*?\b(\w+)\s*;", re.M)
declared = set()
for header in sorted((sysroot / "include").rglob("*.h")):
	text = header.read_text()
	declared |= set(prototype.findall(text)) | set(obj.findall(text))
# `alloca` maps to a compiler builtin and is not a symbol anything provides. It is the one macro in
# these headers whose spelling looks like a call, which is why it is excluded by name.
declared.discard("alloca")

elsewhere = set(identity.get("declared_elsewhere", []))
if not elsewhere:
	failures.append("sysroot.toml names no `declared_elsewhere` set - then every inventory symbol must be declared here, and the loader's own headers say otherwise")

# THE SUBSTRATE'S EXPORTS, read the same way the facilities gate reads them, so the two cannot
# disagree about what "provided" means.
export = re.compile(r'^[ \t]*#\[(?:cfg_attr\(not\(test\), )?unsafe\(no_mangle\)\)?\]\s*\n(?:[ \t]*#\[[^\]]*\]\s*\n)*[ \t]*pub (?:unsafe )?(?:extern "C" )?(?:fn|static mut) (\w+)', re.M)
provided = set()
for crate in ("src/user/libs/foreign/abi/src", "src/user/libs/foreign/discovery/src"):
	for source in sorted(pathlib.Path(crate).glob("*.rs")):
		provided |= set(export.findall(source.read_text()))

for symbol in sorted(declared - provided):
	failures.append(f"<{symbol}> is declared by the profile sysroot and no substrate crate provides it - a compile that succeeds and a link that fails")
for symbol in sorted(named - declared - elsewhere):
	failures.append(f"<{symbol}> is named by the inventory and declared nowhere - neither here nor by the loader's own headers")
for symbol in sorted(declared - named):
	failures.append(f"<{symbol}> is declared by the profile sysroot and the inventory does not name it - the sysroot is sized by the measurement, not by the option set")

count = identity.get("declares")
if count != len(declared):
	failures.append(f"sysroot.toml says it declares {count} and it declares {len(declared)}")

# THE TARGET HEADERS EXIST AND NAME THE TRIPLES THE CROSS FILES DO. Two descriptions of one ABI is
# how the C half and the Rust half come to disagree about a type, so they are compared rather than
# both trusted.
triples = {"x86_64": "x86_64-unknown-none-elf", "aarch64": "aarch64-unknown-none", "riscv64": "riscv64-unknown-none-elf"}
for arch, triple in triples.items():
	header = sysroot / "arch" / f"{arch}.h"
	if not header.is_file():
		failures.append(f"{header} is missing - the data model is per target and force-included per object")
		continue
	text = header.read_text()
	if f'#define LIBER_PROFILE_TRIPLE "{triple}"' not in text:
		failures.append(f"{header} does not name the triple {triple} that this target is built with")
	if "_Static_assert" not in text:
		failures.append(f"{header} asserts nothing - a data model that is described and not checked is a comment")

for failure in failures:
	print(f"profile-sysroot: {failure}", file=sys.stderr)
raise SystemExit(1 if failures else 0)
PY

echo "profile-sysroot: declarations and inventory agree"

# THE THIRD CLAIM. It needs the audit-only upstream, and says so when it is absent rather than
# passing quietly.
if [[ ! -d "$ROOT/.build/foreign/src-loader" || ! -d "$ROOT/.build/foreign/src-headers" ]]; then
	echo "profile-sysroot: NOT PERFORMED: the pinned sources are not unpacked under .build/foreign, so the archives were not rebuilt against this sysroot"
	exit 0
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
for arch in x86_64 aarch64 riscv64; do
	expected="$(sed -n "s/^$arch = { sha256 = \"\([0-9a-f]\{64\}\)\".*/\1/p" "$STATIC_PIN")"
	[[ -n "$expected" ]] || fail "$STATIC_PIN records no archive digest for $arch"
	"$HERE/build-foreign-static.sh" --arch "$arch" --sysroot profile --out "$work/$arch" >"$work/$arch.log" 2>&1 || {
		cat "$work/$arch.log" >&2
		fail "$arch: the pinned configuration does not compile against the profile sysroot"
	}
	actual="$(sha256sum "$work/$arch/libvulkan.a" | cut -d' ' -f1)"
	[[ "$actual" == "$expected" ]] || fail "$arch: the profile sysroot produces $actual and the static-target pin froze $expected - the trim changed what was compiled"
	echo "profile-sysroot: $arch: rebuilt against the profile sysroot, digest unchanged"
done
