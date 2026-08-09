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
# The manifest, or an injected one when the self-test below is proving the checks refuse.
manifest_json="${DEVELOPMENT_GATE_MANIFEST:-$(tools/system-manifest.sh export-json)}"

status=0
fail() {
	echo "development-gate: $1" >&2
	status=1
}

# Prove the gate REFUSES before letting it approve.
#
# The whole point of this gate is that a runtime refusal is not a boundary: the development agent
# must be ABSENT from a shipped image, not present and declining to run. A gate asserting that is
# worth exactly as much as its ability to notice when it stops being true, and running the current
# version over a currently-correct tree demonstrates neither - the tree is correct, so it passes,
# and it would pass identically if `jq` had stopped selecting and `gated` had become empty.
#
# (The vacuous case IS guarded, at the top of check 1 - and that guard is itself one of the things
# below re-checks, because a guard nobody exercises is a comment.)
#
# `--manifest-only` runs checks 1 and 2 and stops: 3 and 4 build crates and read the built package,
# neither of which an injected manifest describes. The manifest arrives through the environment
# rather than by editing the tracked file, because a self-test killed between an edit and its repair
# leaves the tree damaged - which happened here once, to a different gate.
self_test() {
	local real
	real="$(tools/system-manifest.sh export-json)"

	# The valid direction first, so each refusal below is known to be about what it changed.
	if ! DEVELOPMENT_GATE_MANIFEST="$real" "$0" --manifest-only >/dev/null 2>&1; then
		echo "development-gate: SELF-TEST FAILED - the real manifest was rejected by the manifest-only checks, so this gate is broken in the direction that blocks work" >&2
		return 1
	fi

	# Nothing marked development-only. The gate would then have nothing to check and would say so
	# by passing, which is the failure mode a table-driven check reaches by losing its selector.
	local none
	none="$(jq -c '(.programs[] | select(.development == true).development) = false' <<<"$real")"
	if DEVELOPMENT_GATE_MANIFEST="$none" "$0" --manifest-only >/dev/null 2>&1; then
		echo "development-gate: SELF-TEST FAILED - a manifest marking NO program development-only was accepted; the gate would pass vacuously" >&2
		return 1
	fi

	# A program marked development-only whose crate does NOT gate it behind `required-features`.
	# `cat` is an ordinary shipped program, so the crate check must refuse it.
	#
	# The injection is verified to have LANDED before its result is read. The first version named a
	# program that does not exist in the manifest, so `jq` changed nothing, the gate was handed the
	# real manifest, it passed - and the self-test read that pass as "the injection was correctly
	# refused". A self-test that silently checks nothing is the precise failure these gates exist to
	# prevent, arriving in the thing meant to prevent it.
	local ungated before after
	before="$(jq -r '[.programs[] | select(.development == true).name] | length' <<<"$real")"
	ungated="$(jq -c '(.programs["cat"].development) = true' <<<"$real")"
	after="$(jq -r '[.programs[] | select(.development == true).name] | length' <<<"$ungated")"
	if [[ "$after" != "$((before + 1))" ]]; then
		echo "development-gate: SELF-TEST FAILED - the injection did not land ($before -> $after development-only programs); it was proving nothing" >&2
		return 1
	fi
	if DEVELOPMENT_GATE_MANIFEST="$ungated" "$0" --manifest-only >/dev/null 2>&1; then
		echo "development-gate: SELF-TEST FAILED - a development-only program with no required-features gate was accepted" >&2
		return 1
	fi
}

if [[ "${DEVELOPMENT_GATE_MANIFEST:-}" == "" ]]; then
	self_test || exit 1
fi

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

# `--manifest-only` stops here: what follows builds crates and reads the built volume package, and
# an injected manifest describes neither.
if [[ "${1:-}" == "--manifest-only" ]]; then
	((status == 0)) && echo "development-gate: manifest checks pass for ${#gated[@]} development-only program(s)"
	exit "$status"
fi

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
package="../.build/boot/volume-x86_64.pkg"
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
