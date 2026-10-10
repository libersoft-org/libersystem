#!/usr/bin/env bash
# Extended has its own verdict; Core's mandatory scene and performance rows stay in qemu-3d-demo.
exec "$(dirname "${BASH_SOURCE[0]}")/check-qemu-3d-demo.sh" --extended "$@"
