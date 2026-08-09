#!/usr/bin/env bash
# Does the volume package that was BUILT carry exactly the entries the manifest describes?
#
# A thin wrapper around `system-manifest check-volume-package`, and it exists for one reason: to
# prove the check REFUSES before letting it approve. The gate ran the checker over the real package,
# the real package was correct, and it passed - which it would have gone on doing if the comparison
# had stopped comparing. A validator exercised only against a valid input is not exercised.
set -euo pipefail

cd "$(dirname "$0")/.."

package="${1:-../.build/boot/volume-x86_64.pkg}"

# Prove the refusal on a COPY, always.
#
# The obvious way to write this is to damage the package, run the checker, and put it back. That
# works until a kill, a crash or a concurrent run lands between the damage and the repair - and one
# did, in this tree, to a different gate: a self-test that edited a tracked file in place left it
# corrupted in the working tree, with the gate failing on what looked like a real cause. A self-test
# that can damage the build is a worse hazard than the defect it guards against.
self_test() {
	local scratch
	scratch="$(mktemp -d)"
	trap 'rm -rf "$scratch"' RETURN

	# The valid direction first, so every refusal below is known to be about what it changed. A
	# byte-for-byte copy of the real package must be accepted exactly as the original is.
	cp "$package" "$scratch/good.pkg"
	if ! tools/system-manifest.sh check-volume-package "$scratch/good.pkg" >/dev/null 2>&1; then
		echo "volume-layout: SELF-TEST FAILED - a faithful copy of the real package was rejected, so this gate is broken in the direction that blocks work" >&2
		return 1
	fi

	# An entry name changed in place. The archive stays structurally valid - same entry count, same
	# offsets, same lengths - and the only thing wrong is that it now names a file the manifest does
	# not describe. That is the shape a staging mistake actually has, and the shape a comparison
	# that stopped comparing would wave through.
	python3 - "$scratch/good.pkg" "$scratch/renamed.pkg" <<-'PY'
		import sys

		source, destination = sys.argv[1], sys.argv[2]
		blob = bytearray(open(source, "rb").read())
		# Rename the first ASCII path-looking run in the archive by one character, keeping its
		# length. Which entry does not matter; that the set no longer matches the manifest does.
		import re

		match = re.search(rb"[A-Za-z0-9_./-]{6,}\.lsexe", bytes(blob)) or re.search(rb"[A-Za-z0-9_./-]{8,}", bytes(blob))
		if match is None:
		    sys.exit("no entry name found to rename")
		at = match.start()
		blob[at] = ord("z") if blob[at] != ord("z") else ord("y")
		open(destination, "wb").write(bytes(blob))
	PY
	if tools/system-manifest.sh check-volume-package "$scratch/renamed.pkg" >/dev/null 2>&1; then
		echo "volume-layout: SELF-TEST FAILED - a package whose entry names do not match the manifest was accepted, so this gate is not checking what it claims to" >&2
		return 1
	fi

	# And a package that is not a package at all. `Package::parse` is the first thing the checker
	# does, and a checker that treats an unreadable archive as an empty one would report a clean
	# comparison over nothing.
	head -c 512 /dev/urandom >"$scratch/rubbish.pkg"
	if tools/system-manifest.sh check-volume-package "$scratch/rubbish.pkg" >/dev/null 2>&1; then
		echo "volume-layout: SELF-TEST FAILED - random bytes were accepted as a volume package" >&2
		return 1
	fi
}

[[ -f "$package" ]] || {
	echo "volume-layout: no volume package at $package - build one first" >&2
	exit 1
}

self_test
exec tools/system-manifest.sh check-volume-package "$package"
