#!/usr/bin/env bash
# Regenerate the protocol bindings from the LSIDL interface definitions in src/idl.
#
# Sixteen packages, each generated from the WHOLE schema set and each keeping only its own - which
# is why every invocation ends in `idl/*.lsidl` and differs only in `--rust-package` and which other
# packages it is told to reach by name rather than regenerate. That was three Justfile recipes of
# sixteen near-identical lines each, forty-eight lines differing in two words, and a package added
# to one and not the others is a drift nothing would have reported.
#
# The three modes are the same table with one flag changed:
#
#   (none)             write the generated Rust, the docs and the ABI manifests, then format
#   --check            regenerate in memory and fail on any drift, writing nothing
#   --accept-breaking  write, and accept an intentional pre-release ABI-manifest break
#
# `--accept-breaking` is not a stronger `--write`: it is the answer to the ABI check refusing a
# change as breaking, which for a pre-release schema is an ordinary thing to do deliberately and
# never an accident.

SCRIPT_NAME=gen.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

# The packages, in dependency order: a package may only name one already generated above it.
PACKAGES=(base audio device log network observability resources time config process display security session input storage)

# What each package reaches by NAME instead of regenerating. Derived from the schema's own imports;
# written here because the generator is told, not asked.
declare -A EXTERNAL=(
	[base]=""
	[audio]="base"
	[device]="base"
	[log]="base"
	[network]="base"
	[observability]="base"
	[resources]="base"
	[time]="base"
	[config]="base storage"
	[process]="base resources"
	[display]="base process"
	[security]="base process"
	[session]="base process"
	[input]="base"
	[storage]="base"
)

# The aggregate crate: no `--rust-package` of its own, every other package external, and the ONE
# invocation that writes docs/gen - the ABI manifests and the reference pages.
AGGREGATE_EXTERNAL=(audio base config device display input log network observability process resources security session storage time)

help() {
	usage_and_exit <<EOF
usage: gen.sh [--check | --accept-breaking] [--list] [--dry-run]

Regenerates the protocol bindings in src/user/libs/protocol/*, the aggregate crate src/proto, and
the ABI manifests and reference pages under docs/gen, from src/idl/*.lsidl.

  --check             regenerate in memory and fail on Rust, docs, ABI or stale-output drift
  --accept-breaking   write, accepting an intentional pre-release ABI-manifest break
  --list              print the packages in the order they are generated, and exit
  --dry-run           print the command line for each package instead of running it
  -h, --help          this text

With no mode it writes and then formats what it wrote.

examples:
  ./gen.sh                    # after editing a schema
  ./gen.sh --check            # what CI asks: has anything drifted
  ./gen.sh --accept-breaking  # the ABI check refused a change you meant to make
EOF
}

mode=write
dry_run=0

while [[ $# -gt 0 ]]; do
	case "$1" in
	-h | --help) help ;;
	--check)
		mode=check
		shift
		;;
	--accept-breaking)
		mode=accept-breaking
		shift
		;;
	--dry-run)
		dry_run=1
		shift
		;;
	--list)
		printf '%s\n' "${PACKAGES[@]}" proto
		exit 0
		;;
	*) die "unexpected argument '$1' (try --help)" ;;
	esac
done

mode_flag=()
case "$mode" in
check) mode_flag=(--check) ;;
accept-breaking) mode_flag=(--accept-breaking) ;;
esac

# The crate directory for a package, asked of the same script the rest of the build asks.
crate_dir() {
	local package="$1" crate
	crate="$package-proto"
	[[ "$package" == proto ]] && crate=proto
	(cd "$SRC_DIR" && tools/source-path.sh "$crate")
}

# One generator invocation, from `tools/lsidl-gen` so cargo reads ITS configuration - the same
# reason every line of the recipes this replaces began with a `cd`.
generate() {
	local out="$1"
	shift
	if ((dry_run)); then
		# The words, joined with single spaces - so a mode with no flag does not print a double one
		# and read as a difference from the recipe it replaces.
		local words=(cargo run --quiet -- "${mode_flag[@]}" --rust-dir "../../$out/src" "$@" '../../idl/*.lsidl')
		printf '%s\n' "${words[*]}"
		return 0
	fi
	(cd "$SRC_DIR/tools/lsidl-gen" && cargo run --quiet -- "${mode_flag[@]}" --rust-dir "../../$out/src" "$@" ../../idl/*.lsidl)
}

for package in "${PACKAGES[@]}"; do
	out="$(crate_dir "$package")"
	args=(--rust-package "liber:$package@1")
	for external in ${EXTERNAL[$package]}; do
		args+=(--external-rust-package "liber:$external@1=${external}_proto::generated::liber::$external")
	done
	generate "$out" "${args[@]}"
done

out="$(crate_dir proto)"
args=()
for external in "${AGGREGATE_EXTERNAL[@]}"; do
	args+=(--external-rust-package "liber:$external@1=${external}_proto::generated::liber::$external")
done
args+=(--docs-dir ../../../docs/gen)
generate "$out" "${args[@]}"

# FORMAT ONLY WHAT WAS WRITTEN. `--check` writes nothing, so formatting after it would reformat
# whatever is on disk and report that as part of a check that is supposed to change nothing.
if [[ "$mode" != check ]]; then
	for package in "${PACKAGES[@]}" proto; do
		out="$(crate_dir "$package")"
		if ((dry_run)); then
			printf 'cargo fmt in %s\n' "$out"
			continue
		fi
		(cd "$SRC_DIR/$out" && cargo fmt)
	done
fi

case "$mode" in
check) note "no drift: sixteen packages regenerate to what is on disk" ;;
*) note "sixteen packages regenerated and formatted" ;;
esac
