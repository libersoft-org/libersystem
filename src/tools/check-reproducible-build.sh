#!/usr/bin/env bash
# The same source compiles to the same bytes wherever the tree is.
#
# WHY THIS EXISTS. Every crate here is its own Cargo workspace and reaches its neighbours by `../`
# paths, and cargo hashes the absolute path of such a dependency into `-C metadata`, which rustc turns
# into each crate's identity: the hash in every mangled symbol, and which upstream crate's copy of a
# shared generic a dependent links. So the release, built from a sealed copy of the tree at another
# path, gave other binaries than the same commit built in place, and the tracked dynamic report could
# not pass there. `rustc-wrapper.sh`, which `.cargo/config.toml` names, replaces that value for this
# tree's crates; this gate holds it to what it is for.
#
# Against a fixture: three crates in a mini-tree with the tracked wrapper and config, one depending on
# the other two by `../` paths, with a panic (an embedded source path) and a generic instantiated in
# two crates. Built at two paths of different lengths, it must give identical binaries, release and
# debug; and built without the wrapper it must NOT - a fixture that stopped reaching outside its
# workspace would pass the first check while testing nothing. Then what the wrapper must not break:
# an unchanged build compiles nothing, a changed source is compiled again, a dependency compiled
# before the wrapper keeps cargo's identity instead of failing, and a call that is not a compile runs
# unchanged. Host target only, no network: a few seconds.
set -uo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
repo="$(cd "$root/.." && pwd)"
wrapper="$root/tools/rustc-wrapper.sh"

failures=0
cases=0

note() { printf 'reproducible-build: %s\n' "$1"; }
fail() {
	printf 'reproducible-build: FAIL - %s\n' "$1" >&2
	failures=$((failures + 1))
}
check() {
	cases=$((cases + 1))
	if "${@:2}"; then
		note "ok - $1"
	else
		fail "$1"
	fi
}

grep -qx 'rustc-wrapper = "src/tools/rustc-wrapper.sh"' "$repo/.cargo/config.toml" || {
	fail "$repo/.cargo/config.toml does not name src/tools/rustc-wrapper.sh as the rustc wrapper"
	exit 1
}

fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT

# --- the mini-tree --------------------------------------------------------------------------------

tree() {
	local at="$1"
	mkdir -p "$at/.cargo" "$at/src/tools" "$at/src/libs/base/src" "$at/src/libs/mid/src" "$at/src/apps/app/src"
	cp "$repo/.cargo/config.toml" "$at/.cargo/config.toml"
	cp "$wrapper" "$at/src/tools/rustc-wrapper.sh"
	cat >"$at/src/libs/base/Cargo.toml" <<'EOF'
[package]
name = "base"
version = "0.1.0"
edition = "2021"
EOF
	cat >"$at/src/libs/base/src/lib.rs" <<'EOF'
pub fn checked(values: &[u32], index: usize) -> u32 {
	if index >= values.len() {
		panic!("index {index} past {}", values.len());
	}
	values[index]
}

pub fn grow<T: Clone>(items: &mut Vec<T>, item: T) {
	items.push(item);
}
EOF
	cat >"$at/src/libs/mid/Cargo.toml" <<'EOF'
[package]
name = "mid"
version = "0.1.0"
edition = "2021"

[dependencies]
base = { path = "../base" }
EOF
	cat >"$at/src/libs/mid/src/lib.rs" <<'EOF'
pub fn total(values: &mut Vec<u16>) -> u32 {
	base::grow(values, 7);
	values.iter().map(|value| u32::from(*value)).sum()
}
EOF
	cat >"$at/src/apps/app/Cargo.toml" <<'EOF'
[package]
name = "app"
version = "0.1.0"
edition = "2021"

[dependencies]
base = { path = "../../libs/base" }
mid = { path = "../../libs/mid" }
EOF
	cat >"$at/src/apps/app/src/main.rs" <<'EOF'
fn main() {
	let mut values = vec![1u16, 2];
	base::grow(&mut values, 3);
	let count = std::env::args().count();
	println!("{} {}", mid::total(&mut values), base::checked(&[1, 2], count));
}
EOF
}

# Two places whose paths differ in length as well as in name, as the repository and its sealed
# snapshot beside it do not - a fixed-width difference could hide a size that depends on the path.
near="$fixture/a/tree"
far="$fixture/a-much-longer-directory-name/tree"
tree "$near"
tree "$far"

# Every build runs from the app's directory with the environment the tree's own builds have: no
# wrapper or flags of the caller's, which would override the config this gate is about.
build() {
	local at="$1" target="$2"
	(cd "$at/src/apps/app" && env -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER -u RUSTFLAGS -u CARGO_ENCODED_RUSTFLAGS -u CARGO_BUILD_RUSTC_WRAPPER CARGO_TARGET_DIR="$target" cargo build --offline "${BUILD_FLAGS[@]}" 2>&1)
}
plain() {
	local at="$1" target="$2"
	(cd "$at/src/apps/app" && env -u RUSTFLAGS -u CARGO_ENCODED_RUSTFLAGS RUSTC_WRAPPER= CARGO_TARGET_DIR="$target" cargo build --offline "${BUILD_FLAGS[@]}" 2>&1)
}

same_bytes() { cmp -s "$1" "$2"; }
differ() { ! cmp -s "$1" "$2"; }

# --- the same bytes at two paths, and not without the wrapper ------------------------------------

BUILD_FLAGS=(--release -q)
log="$(build "$near" "$fixture/near-release")" || fail "release build at $near failed: $log"
log="$(build "$far" "$fixture/far-release")" || fail "release build at $far failed: $log"
check "a release build at two paths gives identical binaries" same_bytes "$fixture/near-release/release/app" "$fixture/far-release/release/app"

# The panic's location, relative to the tree, and neither tree's path anywhere.
embedded() {
	grep -aq './src/libs/base/src/lib.rs' "$1" && ! grep -aqF "$fixture" "$1"
}
check "the binary names its sources relative to the tree, never by the tree's path" embedded "$fixture/near-release/release/app"

# Every crate of the tree recorded its identity as one the wrapper derived.
recorded() {
	local deps="$1" crate record
	for crate in base mid; do
		record=("$deps/.liber-crate-id/lib$crate"-*)
		[[ -f "${record[0]}" && "$(<"${record[0]}")" == stable:* ]] || return 1
	done
}
check "the wrapper recorded a path-independent identity for each library of the tree" recorded "$fixture/near-release/release/deps"

BUILD_FLAGS=(-q)
log="$(build "$near" "$fixture/near-debug")" || fail "debug build at $near failed: $log"
log="$(build "$far" "$fixture/far-debug")" || fail "debug build at $far failed: $log"
# A debug build shares generics between crates, so this is where a dependent links one upstream copy
# of `grow::<u16>` - chosen by the crates' identities, which is the reported symptom.
check "a debug build, which shares generics between crates, gives identical binaries at two paths" same_bytes "$fixture/near-debug/debug/app" "$fixture/far-debug/debug/app"

BUILD_FLAGS=(--release -q)
log="$(plain "$near" "$fixture/near-plain")" || fail "release build without the wrapper at $near failed: $log"
log="$(plain "$far" "$fixture/far-plain")" || fail "release build without the wrapper at $far failed: $log"
check "without the wrapper the two paths give different binaries, so the fixture reaches the defect" differ "$fixture/near-plain/release/app" "$fixture/far-plain/release/app"

# --- what the wrapper must not break -------------------------------------------------------------

BUILD_FLAGS=(--release -v)
compiles_nothing() {
	local log
	log="$(build "$near" "$fixture/near-release")" || return 1
	! grep -q 'Compiling\|Dirty' <<<"$log"
}
check "an unchanged tree compiles nothing the second time (the embedded-path mapping leaves dep-info alone)" compiles_nothing

recompiles_changed() {
	local log
	touch "$near/src/libs/base/src/lib.rs"
	log="$(build "$near" "$fixture/near-release")" || return 1
	grep -q 'Compiling base' <<<"$log" && grep -q 'Compiling app' <<<"$log"
}
check "a changed dependency is compiled again, and so is what depends on it" recompiles_changed

# A target directory from before the wrapper: its libraries have no record. Changing `mid` compiles
# it through the wrapper while `base` stays as it was - `mid` cannot know `base`'s identity, so it
# keeps cargo's, and the build succeeds.
BUILD_FLAGS=(--release -q)
keeps_cargo_identity() {
	local log record
	log="$(plain "$near" "$fixture/near-before")" || return 1
	touch "$near/src/libs/mid/src/lib.rs"
	log="$(build "$near" "$fixture/near-before")" || return 1
	record=("$fixture/near-before/release/deps/.liber-crate-id/libmid"-*)
	[[ -f "${record[0]}" && "$(<"${record[0]}")" == cargo:* ]] || return 1
	[[ "$("$fixture/near-before/release/app")" == "13 2" ]]
}
check "a dependency compiled before the wrapper keeps cargo's identity for its dependents, and the build runs" keeps_cargo_identity

passes_through() {
	[[ "$("$wrapper" rustc -vV)" == "$(rustc -vV)" ]]
}
check "a call that is not a compile runs unchanged" passes_through

note "$cases cases, $failures failed"
((failures == 0))
