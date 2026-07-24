#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
manifest="$root/user/services/manifest.txt"
build_root="$root/../.build"
image_root="$build_root/system-image/x86_64-unknown-none"

expected="$(
	cat <<'EOF'
dynamic dmesg tools volume lsrt
dynamic du tools volume volume-client storage-proto wire lsrt
dynamic free tools volume lsrt
dynamic lscpu tools volume wire lsrt
dynamic lsirq tools volume wire lsrt
dynamic lsmem tools volume wire lsrt
dynamic lspci tools volume wire lsrt
dynamic readln tools volume lsrt
dynamic uname tools volume lsrt
dynamic uptime tools volume lsrt
EOF
)"

actual="$(awk '$1 == "dynamic" && $2 ~ /^(dmesg|du|free|lscpu|lsirq|lsmem|lspci|readln|uname|uptime)$/ {$1=$1; print}' "$manifest" | sort)"
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
if find "$image_root" -type f \( -name '*.identity' -o -name '*.order' \) -print -quit | grep -q . || find "$build_root/image-artifacts-x86_64-unknown-none" -maxdepth 1 -type f -name '*.order.sha256' -print -quit | grep -q .; then
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
	awk -v root="$image_root" '
		$1 == "library" && $4 == "volume" {print root "/lib/" $2 ".lslib"}
		($1 == "dynamic" || $1 == "dynamic-service") && $4 == "volume" {print root "/bin/" $2}
	' "$manifest" | sort
)

echo "artifact-metadata: clean"
