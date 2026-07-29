#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
manifest_json="$("$root/tools/system-manifest.sh" export-json)"
build_root="$root/../.build"
image_root="$build_root/system-image/x86_64-unknown-none"

expected="$(
	cat <<'EOF'
dynamic dmesg tools volume lsrt
dynamic du tools volume lsrt storage-proto volume-client wire
dynamic free tools volume lsrt
dynamic lscpu tools volume lsrt wire
dynamic lsirq tools volume lsrt wire
dynamic lsmem tools volume lsrt wire
dynamic lspci tools volume lsrt wire
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

command -v llvm-readelf >/dev/null
[[ -d "$image_root" ]] || {
	echo "artifact-metadata: missing x86_64 shared-image output" >&2
	exit 1
}
if [[ -n "$(find "$image_root" -type f \( -name '*.identity' -o -name '*.order' \) -print -quit)" || -n "$(find "$build_root/image-artifacts-x86_64-unknown-none" -maxdepth 1 -type f -name '*.order.sha256' -print -quit)" ]]; then
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
