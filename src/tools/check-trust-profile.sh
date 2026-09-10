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

# UNDER THE SHARED LOADER LOCK, like every other writer of this output (2026-09-01).
#
# This gate deliberately CYCLES trust profiles through the one loader output path, which makes it the
# most dangerous writer of the three: a run staging its own copy could catch the profile this gate is
# passing through, and an A-to-B-to-A cycle restores the original hash so a before/after check agrees.
# The harness staging took this lock and the writers did not, so it was held against nobody.
build() {
	mkdir -p "../.build/state"
	(
		flock 9
		cd "$LOADER" && env "$@" cargo build --quiet
	) 9>"../.build/state/kernel-test-build.lock" || {
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
if grep -qa "ROLLBACK FLOOR ENFORCED" "$OUT"; then
	echo "trust-profile: the test-trust loader CONTAINS the enforcing marker - it keeps no floor" >&2
	exit 1
fi
echo "trust-profile: the test-trust loader carries the published key and says so, and keeps no floor"

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

# AND THE ENFORCING PROFILE IS A THIRD IDENTITY, told apart the same way: built with the release
# key it carries that key and the enforcing marker and none of the test profile's identity; the two
# non-enforcing loaders above carry no enforcing marker, so a build cannot claim a floor it does not
# keep. The published RECOVERY key is checked beside the boot key, because a release loader that
# carried either could accept a manifest anyone can sign.
ENFORCING_MARKER="ROLLBACK FLOOR ENFORCED"
TEST_RECOVERY_KEY_HEX="cd267d8f9c9013744f42374272d6c5b3be33c7a6c3bb3b90bcb4318cc7fac439"
if grep -qa "$ENFORCING_MARKER" "$OUT"; then
	echo "trust-profile: a release loader CONTAINS the enforcing marker - it does not keep a floor and must not say it does" >&2
	status=1
fi
if grep -q "$TEST_RECOVERY_KEY_HEX" <<<"$release_hex"; then
	echo "trust-profile: a release loader CONTAINS the published recovery test key" >&2
	status=1
fi
echo "trust-profile: building the rollback-enforcing loader with the release key"
build LIBER_TRUST_PROFILE=rollback-enforcing LIBER_TRUST_KEY="$RELEASE_KEY" LIBER_TRUST_KEY_ID=42
enforcing_hex="$(hexdump_of "$OUT")"
if ! grep -qa "$ENFORCING_MARKER" "$OUT"; then
	echo "trust-profile: the rollback-enforcing loader does not carry its marker" >&2
	status=1
fi
if grep -q "$TEST_KEY_HEX" <<<"$enforcing_hex" || grep -q "$TEST_RECOVERY_KEY_HEX" <<<"$enforcing_hex" || grep -qa "$MARKER" "$OUT"; then
	echo "trust-profile: an enforcing loader built with the release key CONTAINS the test profile's identity" >&2
	status=1
fi
if ! grep -q "$RELEASE_KEY" <<<"$enforcing_hex"; then
	echo "trust-profile: the enforcing loader does not contain the key it was built for" >&2
	status=1
fi
if ((status == 0)); then
	echo "trust-profile: the enforcing loader carries its marker and the release key, and the non-enforcing loaders carry no marker"
fi
# AND A PROFILE THIS TREE DOES NOT NAME DOES NOT BUILD, which is what keeps a misspelt profile from
# silently becoming the default.
if (cd "$LOADER" && LIBER_TRUST_PROFILE=rollback-enforcin cargo build --quiet >/dev/null 2>&1); then
	echo "trust-profile: a loader built with an unknown profile name COMPILED - a misspelling would have become a default" >&2
	status=1
else
	echo "trust-profile: an unknown profile name does not compile"
fi

# THE BUILD DIRECTORY IS LEFT AS THE DEVELOPMENT PROFILE. A gate that leaves a release loader in the
# tree's build output is one whose next `./run.sh` boots something nobody asked for.
build LIBER_TRUST_PROFILE=test-trust
exit "$status"
