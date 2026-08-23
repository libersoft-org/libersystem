#!/usr/bin/env bash
# Compile the development configuration.
#
# NOTHING IN THIS TREE BUILT IT, and it broke twice in one release for that one reason. The
# development profile is a second configuration of the same source - `--features development` on the
# services and drivers crates - and it is how every scenario in `harness/scenarios/` is run, so while
# it does not compile, no scenario can be replayed. It stopped compiling when `security.lsidl` grew
# an argument that `dev_protocol.rs` was not passing, and nothing said so: the shipping build does
# not read that file.
#
# This gate is the compiler, pointed at the other configuration. It does not boot the guest - a gate
# that boots takes minutes, and the fault class this exists to catch is the one a compiler catches
# in seconds. One target, because the difference between the configurations is a Rust feature and
# not a per-architecture path.
set -euo pipefail

cd "$(dirname "$0")/.."

status=0
fail() {
	echo "development-build: $1" >&2
	status=1
}

# Its own target directory, for the reason the shipping half of `check-development-gate.sh` gives:
# so the gate never disturbs, or is disturbed by, whatever configuration the working tree was last
# built in. The two configurations differ in a feature, and sharing a directory would mean each run
# invalidating the other's cache.
target="$PWD/../.build/cargo/development"
for owner in services drivers; do
	crate="$(tools/source-path.sh "$owner")"
	# Built from inside the crate, as every other recipe does: the build-std configuration these
	# targets need comes from the `.cargo` config found by walking up from the working directory.
	if ! (cd "$crate" && CARGO_TARGET_DIR="$target" cargo build --quiet --target x86_64-unknown-none --features development); then
		fail "$owner does not compile with --features development"
	fi
done

# AND IT BUILT THE PROGRAMS THE FEATURE EXISTS FOR. A compile that produced none of them would be a
# gate reporting on nothing - the crate paths could be wrong, the feature could stop reaching the
# `required-features` programs, and `cargo build` would still succeed and still print nothing. The
# manifest says which programs are development-only; this is the inverse of the check that proves
# the shipping configuration does NOT build them.
mapfile -t gated < <(tools/system-manifest.sh export-json | jq -r '.programs[] | select(.development == true) | .name' | sort)
if ((${#gated[@]} == 0)); then
	fail "no program is marked development-only; the gate would pass vacuously"
fi
for program in "${gated[@]}"; do
	if [[ ! -e "$target/x86_64-unknown-none/debug/$program" ]]; then
		fail "the development configuration did not build $program"
	fi
done

if ((status == 0)); then
	echo "development-build: the development configuration compiles, with ${#gated[@]} development-only program(s)"
fi
exit "$status"
