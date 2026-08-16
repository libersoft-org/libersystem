#!/usr/bin/env bash
# The digest of everything the SDK's wasm artifacts are built FROM.
#
# WHY THIS EXISTS. `panic_the_real_guest` reads a toolchain-built `liber_component.wasm` and fails
# when it is absent - which closed the "a check that did not run looks like a check that passed"
# hole. What it did not close is the one step further along: nothing said the artifact was built
# from the CURRENT sources. Build the SDK, change `report_panic()`, run the host-tests gate alone,
# and the stale artifact makes the test pass against an implementation that no longer exists. A
# green result over yesterday's binary is the same false green in a slower disguise.
#
# ONE IMPLEMENTATION, TWO CALLERS. `build.sh`'s `step_sdk` writes this beside each artifact and the
# test runs the same script and compares. Recomputing the digest in Rust would be two independently
# maintained answers that happen to agree, which is exactly the shape this tree keeps finding in its
# own tests.
#
# WHAT IS IN IT, and why each part:
#
#   - every tracked file under `src/sdk` - the library, the example, `Cargo.toml`, `Cargo.lock` and
#     `.cargo/config.toml`. A change to any of them changes what the artifact should contain.
#   - `rust-toolchain.toml`, hashed as content rather than as a version string, because the pin is
#     the thing that decides which instructions the guest emits and the interpreter must understand.
#     Its own comment says moving it is a deliberate act; this makes the artifact say which one it
#     was built under.
#   - the feature set, passed in by the caller, because the two artifacts differ ONLY by
#     `dev-diagnostics` and would otherwise carry identical digests.
#
# What is NOT in it: the `.wasm` itself. The digest answers "what were the inputs", so that a
# mismatch says the artifact is stale rather than merely different.
set -euo pipefail

cd "$(dirname "$0")/.."

features="${1:-default}"

# `find` rather than `git ls-files`: the gate must work in a tree with uncommitted edits, which is
# the case this is FOR - somebody changed `report_panic()` and did not rebuild.
#
# Sorted by path so the digest does not depend on directory order, and each file's PATH is hashed
# with its contents so a rename is a change.
{
	printf 'features\t%s\n' "$features"
	printf 'toolchain\t%s\n' "$(sha256sum sdk/rust-toolchain.toml | cut -d' ' -f1)"
	find sdk -type f \( -name '*.rs' -o -name '*.toml' -o -name '*.lock' \) -not -path '*/target/*' -print0 |
		sort -z |
		while IFS= read -r -d '' file; do
			printf '%s\t%s\n' "$file" "$(sha256sum "$file" | cut -d' ' -f1)"
		done
} | sha256sum | cut -d' ' -f1
