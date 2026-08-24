#!/usr/bin/env bash
# Prove that a release loader carries none of the test profile's identity.
#
# THE TEST KEY IS PUBLISHED ON PURPOSE - a fixture nobody can reproduce a build with is not a
# fixture - and that is exactly why a release build must not contain it. A profile is only a profile
# if the two builds differ in the binary rather than in a comment.
#
# Both directions, because a gate that only checks the release build cannot tell "the test key is
# absent" from "this grep finds nothing": it asserts the marker IS in the test-trust binary first.
set -euo pipefail

cd "$(dirname "$0")/.."
LOADER="boot/loader"
# The build directory is the repository's, not the crate's: `target-dir` in the loader's cargo
# configuration points at it, so the artifact is not under `boot/loader`.
OUT="../.build/cargo/loader/x86_64-unknown-uefi/debug/libersystem-loader.efi"

# The published test key's PUBLIC half and its key id, as they appear in a binary. Written here
# rather than read from the source, so a change to either has to be made in two places on purpose.
TEST_KEY_HEX="d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"
TEST_KEY_ID_LE="01007e57"
MARKER="TEST TRUST (published key)"

# A release key that is not the test key, and not anybody's: this gate builds a release loader, it
# does not make one.
RELEASE_KEY="d04ab232742bb4ab3a1368bd4615e4e6d0224ab71a016baf8520a332c9778737"

hexdump_of() {
	# `-p` is a plain hex dump; the newlines go so a value split across two lines is still found.
	xxd -p "$1" | tr -d '\n'
}

build() {
	(cd "$LOADER" && env "$@" cargo build --quiet) || {
		echo "trust-profile: the loader did not build with $*" >&2
		exit 1
	}
}

echo "trust-profile: building the test-trust loader"
build LIBER_TRUST_PROFILE=test-trust
test_hex="$(hexdump_of "$OUT")"
if ! grep -q "$TEST_KEY_HEX" <<<"$test_hex"; then
	echo "trust-profile: the test-trust loader does not carry the test key - this gate cannot tell the two profiles apart" >&2
	exit 1
fi
if ! grep -qa "$MARKER" "$OUT"; then
	echo "trust-profile: the test-trust loader does not carry its marker - see above" >&2
	exit 1
fi
echo "trust-profile: the test-trust loader carries the published key and says so"

echo "trust-profile: building the external-release loader"
build LIBER_TRUST_PROFILE=external-release LIBER_TRUST_KEY="$RELEASE_KEY" LIBER_TRUST_KEY_ID=42
release_hex="$(hexdump_of "$OUT")"
status=0
if grep -q "$TEST_KEY_HEX" <<<"$release_hex"; then
	echo "trust-profile: a release loader CONTAINS the published test key" >&2
	status=1
fi
if grep -q "$TEST_KEY_ID_LE" <<<"$release_hex"; then
	echo "trust-profile: a release loader CONTAINS the test key id" >&2
	status=1
fi
if grep -qa "$MARKER" "$OUT"; then
	echo "trust-profile: a release loader CONTAINS the TEST TRUST marker" >&2
	status=1
fi
if ! grep -q "$RELEASE_KEY" <<<"$release_hex"; then
	echo "trust-profile: a release loader does not contain the key it was built for" >&2
	status=1
fi
if ((status == 0)); then
	echo "trust-profile: the release loader carries its own key and none of the test profile's identity"
fi

# THE BUILD DIRECTORY IS LEFT AS THE DEVELOPMENT PROFILE. A gate that leaves a release loader in the
# tree's build output is one whose next `./run.sh` boots something nobody asked for.
build LIBER_TRUST_PROFILE=test-trust
exit "$status"
