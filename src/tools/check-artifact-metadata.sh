#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
# The manifest, or an injected one when the self-test below is proving the comparison refuses.
manifest_json="${ARTIFACT_METADATA_MANIFEST:-$("$root/tools/system-manifest.sh" export-json)}"
build_root="$root/../.build"
image_root="$build_root/image/x86_64-unknown-none"

expected="$(
	cat <<'EOF'
dynamic dmesg tools volume lsrt
dynamic du tools volume base-proto lsrt storage-proto volume-client wire
dynamic free tools volume base-proto lsrt
dynamic lscpu tools volume base-proto lsrt wire
dynamic lsirq tools volume base-proto lsrt wire
dynamic lsmem tools volume base-proto lsrt wire
dynamic lspci tools volume base-proto lsrt wire
dynamic readln tools volume lsrt
dynamic uname tools volume lsrt
dynamic uptime tools volume lsrt
EOF
)"

actual="$(jq -r '.programs[] | select(.linkage == "dynamic" and (.name | test("^(dmesg|du|free|lscpu|lsirq|lsmem|lspci|readln|uname|uptime)$"))) | "dynamic \(.name) \(.owner) \(.stage) \(.providers | join(" "))"' <<<"$manifest_json" | sort)"
if [[ "$actual" != "$expected" ]]; then
	echo "artifact-metadata: executable contracts differ from the manifest" >&2
	diff -u <(printf '%s\n' "$expected") <(printf '%s\n' "$actual") >&2 || true
	exit 1
fi

# `--contracts-only` stops here: the self-test below re-enters this script to prove the comparison
# above refuses what it should, and the ELF checks after it are about staged artifacts that an
# injected manifest does not describe.
[[ "${1:-}" == "--contracts-only" ]] && {
	echo "artifact-metadata: contracts match"
	exit 0
}

# Prove the comparison REFUSES before letting it approve.
#
# The block above is a hand-written table of what each dynamic executable owns, stages and links
# against, compared with what the manifest says. It passes because both are currently right, and it
# would pass just as well if the `jq` selector had stopped matching and `actual` had become empty
# against an `expected` that was also - no. That is exactly the case the third injection below
# catches, and it is the one a table-versus-table check is most likely to reach: a selector that
# matches nothing compares nothing.
#
# The manifest is injected through the environment rather than edited: this gate reads a tracked
# file, and a self-test that edits a tracked file in place can leave the tree damaged if it is
# killed between the edit and the repair. That happened here once, to a different gate.
if [[ "${ARTIFACT_METADATA_MANIFEST:-}" == "" ]]; then
	real="$("$root/tools/system-manifest.sh" export-json)"

	# A provider removed from one program: the contract changed and the table did not.
	dropped="$(jq -c '(.programs[] | select(.name == "du").providers) |= map(select(. != "wire"))' <<<"$real")"
	if ARTIFACT_METADATA_MANIFEST="$dropped" "$0" --contracts-only >/dev/null 2>&1; then
		echo "artifact-metadata: SELF-TEST FAILED - a program that lost a provider was accepted, so this gate is not comparing what it claims to" >&2
		exit 1
	fi

	# An owner changed: same providers, different crate. The table pins both.
	moved="$(jq -c '(.programs[] | select(.name == "free").owner) = "services"' <<<"$real")"
	if ARTIFACT_METADATA_MANIFEST="$moved" "$0" --contracts-only >/dev/null 2>&1; then
		echo "artifact-metadata: SELF-TEST FAILED - a program that changed owner was accepted" >&2
		exit 1
	fi

	# And the empty comparison: a manifest naming none of these programs at all. `actual` is then
	# empty, and a check that had lost its selector would produce exactly this and call it a match.
	if ARTIFACT_METADATA_MANIFEST='{"programs":[],"libraries":[]}' "$0" --contracts-only >/dev/null 2>&1; then
		echo "artifact-metadata: SELF-TEST FAILED - a manifest describing NO dynamic executables was accepted; a selector that matches nothing compares nothing" >&2
		exit 1
	fi
fi

command -v llvm-readelf >/dev/null
[[ -d "$image_root" ]] || {
	echo "artifact-metadata: missing x86_64 shared-image output" >&2
	exit 1
}
if [[ -n "$(find "$image_root" -type f \( -name '*.identity' -o -name '*.order' \) -print -quit)" || -n "$(find "$build_root/cache/x86_64-unknown-none" -maxdepth 1 -type f -name '*.order.sha256' -print -quit)" ]]; then
	echo "artifact-metadata: obsolete identity or provider-order sidecar remains" >&2
	exit 1
fi

while IFS= read -r artifact; do
	[[ -f "$artifact" ]] || {
		echo "artifact-metadata: missing staged dynamic image $artifact" >&2
		exit 1
	}
	if ! llvm-readelf -SW "$artifact" | awk '$2 == ".note.liber.identity" && $3 == "NOTE" && $0 ~ / A / { found = 1 } END { exit !found }'; then
		echo "artifact-metadata: $artifact has no allocated embedded identity note" >&2
		exit 1
	fi
done < <(
	jq -r --arg root "$image_root" '(.libraries[].destination | "\($root)/\(.)"), (.programs[] | select(.linkage == "dynamic" and .stage == "volume") | "\($root)/\(.destination | sub("\\.lsexe$"; ""))")' <<<"$manifest_json" | sort
)

echo "artifact-metadata: clean"
