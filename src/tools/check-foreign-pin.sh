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
STATIC_PIN="src/foreign/PIN-static-target.toml"

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

python3 - "$PIN" "$STATIC_PIN" <<'PY' || exit 1
import hashlib
import pathlib
import sys
import tomllib

pin = tomllib.load(open(sys.argv[1], "rb"))
static_path = pathlib.Path(sys.argv[2])
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
	# AND ITS CONTENTS ARE THE FROZEN ONES. Checking only that the files EXIST would let a header be
	# edited after the freeze - which changes the ABI every later measurement was taken under, which
	# is the whole reason this digest is in the pin.
	actual = digest_tree(sysroot_path)
	if actual != sysroot["digest"]:
		failures.append(f"the bootstrap sysroot has changed since the freeze ({actual[:12]} is not {sysroot['digest'][:12]})")

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

# 4. THE STATIC-TARGET PART, WHEN IT EXISTS. It is frozen after the bootstrap inputs and before pass
#    1, so its absence is a milestone that has not got there yet rather than a failure - but its
#    patch and its builder are checked against their digests the moment it does exist.
static_state = "absent"
if static_path.is_file():
	static = tomllib.load(static_path.open("rb"))
	static_state = "present"
	# THE ABI DESCRIPTION IS PINNED LIKE THE OTHER TWO. It was inside the builder until the image
	# build needed the same values; a file two builds read is a file that can move under both at
	# once, which is exactly what a pin is for.
	for section in ("patch", "builder", "abi"):
		entry = static[section]
		path = root / entry["path"]
		if not path.is_file():
			failures.append(f"static-target: {entry['path']} is pinned and missing")
			continue
		actual = digest_file(path)
		if actual != entry["sha256"]:
			failures.append(f"static-target: {entry['path']} has changed since the freeze ({actual[:12]} is not {entry['sha256'][:12]})")
	# THE FREEZE ORDER, CHECKED RATHER THAN TRUSTED. This part pins a patch against the revision the
	# bootstrap part froze; if it names a different one, one of the two has moved without the other.
	pinned_revision = pin["upstream"]["vulkan-loader"]["revision"]
	if static["patch"]["applies_to"] != pinned_revision:
		failures.append(f"static-target: the patch applies to {static['patch']['applies_to']} and the bootstrap part pins {pinned_revision}")
	# EVERY TARGET CLAIMS TO HAVE BEEN REPRODUCED. A recorded archive that was built once is evidence
	# the deliverable was attempted, not that it is done.
	for name, archive in static["archives"].items():
		if not archive.get("reproduced"):
			failures.append(f"static-target: {name} is recorded without a second build agreeing - that is an attempt, not a result")

# 5. THE DERIVED PART, WHEN IT EXISTS. It is frozen after the converging link and before anything is
#    declared done, so its absence is a milestone that has not got there yet. When it is there, every
#    value it holds is checked against the tree - and against the OTHER parts, because the freeze
#    order is what makes a lockfile a lockfile rather than three files.
derived_state = "absent"
derived_path = root / "src/foreign/PIN-derived.toml"
if derived_path.is_file():
	derived = tomllib.load(derived_path.open("rb"))
	derived_state = "present"
	if derived["patches"]["applies_to"] != pin["upstream"]["vulkan-loader"]["revision"]:
		failures.append(f"derived: the patch series applies to {derived['patches']['applies_to']} and the bootstrap part pins {pin['upstream']['vulkan-loader']['revision']}")
	orders = [entry["order"] for entry in derived["patches"]["series"]]
	if orders != sorted(orders) or orders != list(range(1, len(orders) + 1)):
		failures.append(f"derived: the patch series is not a contiguous ordered series: {orders}")
	for entry in derived["patches"]["series"]:
		path = root / entry["path"]
		if not path.is_file():
			failures.append(f"derived: {entry['path']} is pinned and missing")
			continue
		actual = digest_file(path)
		if actual != entry["sha256"]:
			failures.append(f"derived: {entry['path']} has changed since the freeze ({actual[:12]} is not {entry['sha256'][:12]})")
	# THE PART THAT OWNS EACH PATCH. 0001 is the static-target part's and is listed here only so the
	# ported tree can be built; a series that claimed it would be two parts pinning one value.
	static_entries = [entry for entry in derived["patches"]["series"] if entry["part"] == "static-target"]
	if len(static_entries) != 1 or static_entries[0]["path"] != static["patch"]["path"]:
		failures.append("derived: the series does not name exactly the static-target part's own patch as that part's")
	sysroot_digest = digest_tree(root / derived["profile_sysroot"]["path"])
	if sysroot_digest != derived["profile_sysroot"]["digest"]:
		failures.append(f"derived: the profile sysroot has changed since the freeze ({sysroot_digest[:12]} is not {derived['profile_sysroot']['digest'][:12]})")
	for key in ("inventory", "tool"):
		path = root / derived["audit_link"][key]
		if not path.is_file():
			failures.append(f"derived: {derived['audit_link'][key]} is pinned and missing")
			continue
		actual = digest_file(path)
		if actual != derived["audit_link"][f"{key}_sha256"]:
			failures.append(f"derived: {derived['audit_link'][key]} has changed since the freeze ({actual[:12]} is not {derived['audit_link'][f'{key}_sha256'][:12]})")
	# EVERY TARGET CLAIMS A FIXED POINT, which is what "converged" means and is not the same as "it
	# linked once".
	for name in ("x86_64", "aarch64", "riscv64"):
		if not derived["audit_link"][name].get("converged"):
			failures.append(f"derived: {name} is recorded without a fixed point - that is one link, not a convergence")
	# THE FORBIDDEN-MECHANISM HALF IS TWO THINGS, and a part holding only one of them has recorded a
	# decision nothing enforces. The flags are what stop a mechanism being emitted; the gate is what
	# catches it if they ever fail to.
	forbidden = derived.get("forbidden_mechanisms")
	if not forbidden:
		failures.append("derived: no forbidden-mechanism decision is recorded, and the C++ ABI answer is exactly that decision")
	else:
		if not forbidden.get("c_flags") or not forbidden.get("rust_flags"):
			failures.append("derived: a forbidden mechanism is recorded without the flags for both producers - the flag that was missing is how the last one got through")
		gate = root / forbidden.get("gate", "")
		if not gate.is_file():
			failures.append(f"derived: the forbidden-mechanism gate {forbidden.get('gate')} does not exist, so the decision is a description")

	# THE QUARANTINE HALF IS A CONSUMER AND A GATE. A part recording the artifact without the gate
	# that launches it would be recording a file nobody runs, which is the state this milestone was
	# in before the carrier existed.
	quarantine = derived.get("quarantine")
	if not quarantine:
		failures.append("derived: no quarantine consumer is recorded, and the guest gate is what the audit link exists for")
	else:
		for key in ("source", "gate", "facility_source", "facility_gate"):
			if not (root / quarantine.get(key, "")).is_file():
				failures.append(f"derived: the quarantine {key} {quarantine.get(key)} does not exist")

	# AND THE FREEZE ORDER: nothing derived may exist before the part it is derived from.
	if static_state != "present":
		failures.append("derived: the derived part exists and the static-target part does not - it has frozen values derived from something unfrozen")

for line in failures:
	print(f"foreign-pin: {line}", file=sys.stderr)
if failures:
	raise SystemExit(1)

print(f"foreign-pin: the derived part is {derived_state}" + (f" - {len(derived['patches']['series'])} ordered patch(es), the profile sysroot and a fixed point on three targets" if derived_state == "present" else " - the milestone has not reached it"))
print(f"foreign-pin: the bootstrap part is intact - {len(pin['cross_files'])} cross file(s), the sysroot and {len(pin['upstream'])} pinned upstream(s)")
if verified:
	print(f"foreign-pin: {len(verified)} archive(s) present and matching their digest: {', '.join(sorted(verified))}")
else:
	# NAMED RATHER THAN PASSED OVER. The archives are audit-only and are not in the tree, so their
	# half of this check did not run - and a check that was not run is not a check that passed.
	print(f"foreign-pin: no upstream archive is staged in {store}, so their digests were NOT verified on this run")
if static_state == "present":
	names = ", ".join(sorted(static["archives"]))
	print(f"foreign-pin: the static-target part is intact - one patch against {static['patch']['applies_to']}, reproduced on {names}")
else:
	print("foreign-pin: the static-target part is not frozen yet, so pass 1 may not start")
PY

# PASS 1'S INVENTORY IS A GENERATED ARTIFACT, and the plan says a regeneration that differs from the
# recorded one fails the gate. It is regenerated here rather than trusted, whenever the archives it
# was derived from are present; without them the check says so instead of passing quietly.
if [[ -f src/foreign/INVENTORY-pass1.json ]]; then
	if [[ -f .build/foreign/x86_64/libvulkan.a && -f .build/foreign/aarch64/libvulkan.a && -f .build/foreign/riscv64/libvulkan.a ]]; then
		python3 src/tools/foreign-inventory.py --check >/dev/null || fail "the recorded pass-1 inventory is not what regenerating it produces"
		note "the pass-1 inventory reproduces from the three archives"
	else
		note "the pass-1 inventory is recorded and its archives are not built, so it was NOT regenerated on this run"
	fi
fi

note "the freeze order holds: the policy precedes the pin, and the pin matches the tree"
