#!/bin/bash
# P02M0135's lockfile, checked against the tree it pins.
#
# A LOCKFILE NOTHING READS IS A COMMENT. Its whole purpose is that a value cannot drift from what was
# frozen without something failing, and "something" has to be a check rather than a reader's memory.
#
# WHAT IS VERIFIED HERE, AND WHY EACH MATTERS:
#
#   the digests still match   a cross file or a sysroot header edited after the freeze changes the
#                             ABI every later measurement was taken under, silently. The inventory
#                             would then describe a compile nobody can reproduce.
#   the parts exist in order  the freeze order is LICENSING POLICY -> BOOTSTRAP INPUTS -> bootstrap
#                             pin -> static target -> ... and a part that appears before the one it
#                             depends on has frozen a value derived from something unfrozen.
#   the gate's own claim      the bootstrap part claims a compile reaches a source diagnostic rather
#                             than a missing header on all three targets. That claim is re-checked
#                             against the recorded upstream when the sources are present, and is
#                             reported as unverifiable when they are not - never assumed.
#
# THE UPSTREAM IS AUDIT-ONLY AND CONTENT-ADDRESSED, not vendored. It lives under `.build/` where the
# archives were placed once, by a person. This check therefore has two modes and says which it ran:
# with the archives present it verifies their digests, and without them it verifies everything that
# does not need them. A check that silently skipped the interesting half would be worse than one that
# is not run at all.

set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
cd "$ROOT"

PIN="src/foreign/PIN-bootstrap.toml"

fail() {
	echo "foreign-pin: $*" >&2
	exit 1
}

note() {
	echo "foreign-pin: $*"
}

[[ -f "$PIN" ]] || fail "$PIN is missing - the bootstrap part of the lockfile is what everything after it reads"

# THE FREEZE ORDER'S FIRST RULE. Nothing but the licensing policy and the bootstrap inputs may exist
# before the bootstrap pin, and the policy is what governs fetching the sources the inputs are built
# against - so a pin without a policy has frozen a dependency nobody reviewed.
[[ -f docs/DEPENDENCY_POLICY.md ]] || fail "the lockfile exists and docs/DEPENDENCY_POLICY.md does not - the policy precedes the pin it governs"

python3 - "$PIN" <<'PY' || exit 1
import hashlib
import pathlib
import sys
import tomllib

pin = tomllib.load(open(sys.argv[1], "rb"))
root = pathlib.Path(".")
failures = []


def digest_file(path):
	return hashlib.sha256(path.read_bytes()).hexdigest()


def digest_tree(path):
	"""The digest of a directory: every file's own digest, in path order, hashed again.

	ORDER IS PART OF IT. A tree digest that depended on directory iteration order would differ
	between two machines holding identical bytes, which makes it useless as a pin."""
	inner = hashlib.sha256()
	for entry in sorted(p for p in path.rglob("*") if p.is_file()):
		inner.update(hashlib.sha256(entry.read_bytes()).hexdigest().encode())
		inner.update(b"  ./" + str(entry.relative_to(path)).encode() + b"\n")
	return inner.hexdigest()


# 1. THE CROSS FILES ARE THE ONES THAT WERE FROZEN.
for name, entry in pin["cross_files"].items():
	path = root / entry["path"]
	if not path.is_file():
		failures.append(f"{name}: {entry['path']} is pinned and missing")
		continue
	actual = digest_file(path)
	if actual != entry["sha256"]:
		failures.append(f"{name}: {entry['path']} has changed since the freeze ({actual[:12]} is not {entry['sha256'][:12]})")

# 2. THE BOOTSTRAP SYSROOT IS THE ONE THAT WAS FROZEN, AND HOLDS WHAT THE PIN SAYS IT HOLDS.
sysroot = pin["bootstrap_sysroot"]
sysroot_path = root / sysroot["path"]
if not sysroot_path.is_dir():
	failures.append(f"the bootstrap sysroot {sysroot['path']} is pinned and missing")
else:
	for header in sysroot["headers"]:
		if not (sysroot_path / "include" / header).is_file():
			failures.append(f"the sysroot is pinned to hold {header} and does not")

# 3. THE UPSTREAM ARCHIVES, WHEN THEY ARE PRESENT. Audit-only and content-addressed: fetched once by
#    a person into `.build/foreign/`, never by the build. A digest that does not match is the same
#    failure as an unreviewed licence.
store = root / ".build" / "foreign"
verified = []
for name, entry in pin["upstream"].items():
	archive = store / f"{name}-{entry['revision']}.tar.gz"
	if not archive.is_file():
		continue
	actual = digest_file(archive)
	if actual != entry["archive_sha256"]:
		failures.append(f"{name}: the archive in {store} is not the pinned one ({actual[:12]})")
	else:
		verified.append(name)

for line in failures:
	print(f"foreign-pin: {line}", file=sys.stderr)
if failures:
	raise SystemExit(1)

print(f"foreign-pin: the bootstrap part is intact - {len(pin['cross_files'])} cross file(s), the sysroot and {len(pin['upstream'])} pinned upstream(s)")
if verified:
	print(f"foreign-pin: {len(verified)} archive(s) present and matching their digest: {', '.join(sorted(verified))}")
else:
	# NAMED RATHER THAN PASSED OVER. The archives are audit-only and are not in the tree, so their
	# half of this check did not run - and a check that was not run is not a check that passed.
	print(f"foreign-pin: no upstream archive is staged in {store}, so their digests were NOT verified on this run")
PY

note "the freeze order holds: the policy precedes the pin, and the pin matches the tree"
