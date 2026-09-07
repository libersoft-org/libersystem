#!/usr/bin/env bash
# Exercise the real boot volume reads with FAT and UDF moved to the opposite bus positions.
set -euo pipefail
cd "$(dirname "$0")/../.."
MEDIA_ORDER=swapped ./test.sh --arch x86_64 --tags boot
