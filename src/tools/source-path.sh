#!/usr/bin/env bash
set -euo pipefail

owner="${1:?usage: source-path.sh <logical-owner>}"
root="$(cd "$(dirname "$0")/.." && pwd)"
exec "$root/tools/system-manifest.sh" source-path "$owner"
