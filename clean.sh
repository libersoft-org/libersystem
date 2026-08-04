#!/usr/bin/env bash
# Remove build output.
#
# The Justfile had one `clean` that deleted everything, which is the only option that never needs
# explaining and also the only one that costs a full rebuild to undo. Naming parts makes the
# cheaper cases possible without making the expensive one harder.

SCRIPT_NAME=clean.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

PARTS_ALL="cargo boot logs"

help() {
	usage_and_exit <<EOF
usage: clean.sh [--part PART[,PART...]] [--dry-run]

Removes build output from .build/. With no arguments: everything.

  --part PART   $PARTS_ALL | all   (default: all)
  --dry-run     print what would be removed and stop
  -h, --help    this text

parts:
  cargo   compiled artifacts (.build/cargo) - the expensive one to rebuild
  boot    images, packages, volumes and ESPs (.build/boot)
  logs    test and run logs (.build/logs)

examples:
  ./clean.sh --part boot        # rebuild images without recompiling
  ./clean.sh --part boot,logs
  ./clean.sh --dry-run
EOF
}

parts=()
dry=0

while [[ $# -gt 0 ]]; do
	case "$1" in
	-h | --help) help ;;
	--part)
		[[ $# -ge 2 ]] || die "--part needs a value"
		picked_raw="$(parse_list "$2" part "$PARTS_ALL")"
		mapfile -t picked <<<"$picked_raw"
		parts+=("${picked[@]}")
		shift 2
		;;
	--dry-run)
		dry=1
		shift
		;;
	*) die "unexpected argument '$1' (try --help)" ;;
	esac
done

if [[ ${#parts[@]} -eq 0 ]]; then
	# shellcheck disable=SC2206
	parts=($PARTS_ALL)
fi

for part in "${parts[@]}"; do
	target="$BUILD_DIR/$part"
	if [[ ! -e "$target" ]]; then
		note "$part: nothing there"
		continue
	fi
	if [[ $dry -eq 1 ]]; then
		note "would remove $target ($(du -sh "$target" 2>/dev/null | cut -f1))"
		continue
	fi
	note "removing $target"
	rm -rf "$target"
done
