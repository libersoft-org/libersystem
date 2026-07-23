#!/usr/bin/env bash
set -euo pipefail

owner="${1:?usage: source-path.sh <logical-owner>}"
root="$(cd "$(dirname "$0")/.." && pwd)"
manifest="$root/user/services/manifest.txt"
path="$(awk -v owner="$owner" '$1 == "source" && $2 == owner {print $3; count++} END {if (count != 1) exit 1}' "$manifest")" || {
	echo "source-path: $owner has no unique source path" >&2
	exit 1
}
printf '%s\n' "$path"
