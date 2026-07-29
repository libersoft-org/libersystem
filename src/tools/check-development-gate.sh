#!/usr/bin/env bash
# Verify the development boundary is a compile-time one.
#
# The development agent, its volatile artifact registry and the control port's transport must
# be absent from a shipped image, not present and refusing to run. That is only true if the
# shipping configuration never builds them and never stages them, and if no default feature
# set turns them on - all three of which are easy to lose by accident and impossible to
# notice by reading, because the runtime refusals stay in place either way.
set -euo pipefail

cd "$(dirname "$0")/.."
manifest_json="$(tools/system-manifest.sh export-json)"

status=0
fail() {
	echo "development-gate: $1" >&2
	status=1
}

# 1. The manifest marks exactly the development-only programs, and every one of them is
#    behind `required-features` in the crate that owns it.
mapfile -t gated < <(jq -r '.programs[] | select(.development == true) | .name' <<<"$manifest_json" | sort)
if ((${#gated[@]} == 0)); then
	fail "no program is marked development-only; the gate would pass vacuously"
fi
for program in "${gated[@]}"; do
	owner="$(jq -r --arg p "$program" '.programs[$p].owner' <<<"$manifest_json")"
	crate="$(tools/source-path.sh "$owner")/Cargo.toml"
	program_block="$(grep -A 4 "name = \"$program\"" "$crate")" || program_block=""
	if ! grep -q 'required-features = \["development"\]' <<<"$program_block"; then
		fail "$program is development-only in the manifest but not gated by required-features in $crate"
	fi
done

# 2. No default feature set enables it. A `development` listed in any `default = [...]` would
#    make the gate meaningless everywhere.
for crate in "$(tools/source-path.sh services)/Cargo.toml" "$(tools/source-path.sh drivers)/Cargo.toml" kernel/Cargo.toml; do
	default_features="$(sed -n '/^\[features\]/,/^\[/p' "$crate" | grep -E '^default = ')" || default_features=""
	if grep -q development <<<"$default_features"; then
		fail "$crate enables development by default"
	fi
done

# 3. The shipping configuration builds, and produces none of the gated binaries. Built into
#    its own target directory so this check never disturbs, or is disturbed by, whatever
#    configuration the working tree was last built in.
target="$PWD/../.build/cargo/shipping"
for owner in services drivers; do
	crate="$(tools/source-path.sh "$owner")"
	# Build from inside the crate, as every other recipe does: the build-std configuration
	# these targets need comes from the `.cargo` config found by walking up from the working
	# directory, not from wherever the manifest happens to be.
	(cd "$crate" && CARGO_TARGET_DIR="$target" cargo build --quiet --target x86_64-unknown-none >/dev/null)
done
for program in "${gated[@]}"; do
	if [[ -e "$target/x86_64-unknown-none/debug/$program" ]]; then
		fail "the shipping configuration built $program"
	fi
done

# 4. The volume package that was actually built carries exactly the configuration that was
#    asked for. This is the half that a crate-only check cannot see, and the half that failed
#    silently once: the userspace crates were built with the feature while the kernel staged
#    the shipping set, because a build script does not see `cfg!(feature = ...)`. Both halves
#    agreed they were shipping, so every assertion inside the build passed while the
#    development image had no development units in it.
package="../.build/boot/volume.pkg"
if [[ -e "$package" ]]; then
	entries="$(tools/system-manifest.sh check-volume-package "$package" 2>&1 || true)"
	wanted="shipping"
	[[ "${LIBER_DEVELOPMENT:-0}" == "1" ]] && wanted="development"
	if ! grep -q "($wanted configuration)" <<<"$entries"; then
		fail "the built volume package is not the $wanted configuration: $entries"
	fi
else
	echo "development-gate: note: no volume package built yet, so its configuration was not checked" >&2
fi

if ((status == 0)); then
	echo "development-gate: ${#gated[@]} development-only program(s) absent from the shipping configuration"
fi
exit "$status"
