#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
manifest="$root/user/services/manifest.txt"

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

echo "artifact-metadata: clean"
