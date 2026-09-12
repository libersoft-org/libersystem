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
# PASS 2 SIZES IT, NOT PASS 1. The candidate surface over-states what a system must provide, and the
# platform port removed six of its symbols outright - so the sysroot that declares what the SUBSTRATE
# backs has to be measured against what the converged link resolved.
INVENTORY="src/foreign/INVENTORY-pass2.json"
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
# THE WHOLE SURFACE, because a header declares what a SOURCE may call and the closure is what backs
# it - and four of those names are backed by the runtime rather than by the substrate crates. A
# sysroot sized to the substrate alone would drop `memcpy`, which every C compiler emits calls to.
surfaces = {arch: set(inventory[arch]["archive_surface"]) for arch in ("x86_64", "aarch64", "riscv64")}
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
# AND WHAT THE RUNTIME OWNS. `lsrt.lslib` publishes the four memory functions on purpose, so that
# every library in this image imports them rather than carrying a copy; the converged link resolves
# the C references to them from there. They are as provided as anything in the crates above - by a
# different owner, which is what the audit artifact's export-collision check exists to keep true.
provided |= {"memcpy", "memmove", "memset", "memcmp"}

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
if [[ ! -d "$ROOT/.build/foreign/src-loader-port" || ! -d "$ROOT/.build/foreign/src-headers" ]]; then
	echo "profile-sysroot: NOT PERFORMED: the ported sources are not unpacked under .build/foreign, so the archives were not rebuilt against this sysroot"
	exit 0
fi

# THE TRIM CHANGED NOTHING, PROVED ON THE TREE THAT MATTERS. The ported configuration is compiled
# twice - once against the bootstrap sysroot, which declares everything the sources include, and once
# against this one, which declares only what the substrate backs - and the two archives must be
# identical. That is what proves the declarations dropped from here were surface nobody required; a
# reading of the sources would prove nothing of the kind.
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
for arch in x86_64 aarch64 riscv64; do
	for which in bootstrap profile; do
		"$HERE/build-foreign-static.sh" --arch "$arch" --sysroot "$which" --ported --out "$work/$arch-$which" >"$work/$arch-$which.log" 2>&1 || {
			cat "$work/$arch-$which.log" >&2
			fail "$arch: the ported configuration does not compile against the $which sysroot"
		}
	done
	bootstrap_digest="$(sha256sum "$work/$arch-bootstrap/libvulkan.a" | cut -d' ' -f1)"
	profile_digest="$(sha256sum "$work/$arch-profile/libvulkan.a" | cut -d' ' -f1)"
	[[ "$bootstrap_digest" == "$profile_digest" ]] || fail "$arch: the two sysroots produce different archives ($profile_digest vs $bootstrap_digest) - the trim changed what was compiled"
	echo "profile-sysroot: $arch: the ported archive is the same against both sysroots"
done
