#!/usr/bin/env bash
# Regenerate the protocol bindings from the LSIDL interface definitions in src/idl.
#
# A package per schema, each generated from the WHOLE schema set and each keeping only its own -
# which is why every invocation ends in `idl/*.lsidl` and differs only in `--rust-package` and which
# other packages it is told to reach by name rather than regenerate. That was three Justfile recipes
# of sixteen near-identical lines each, forty-eight lines differing in two words, and a package added
# to one and not the others is a drift nothing would have reported. The count is printed from the
# list rather than written in the prose, for the same reason.
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
# EVERY RUN ENDS WITH A VERDICT, from the one EXIT dispatcher in lib.sh - and cleanups REGISTER with
# it rather than installing traps of their own. A run that fails is otherwise indistinguishable from
# a run that is still going, which is exactly how a failed build came to be waited on for half an
# hour. `docs/TESTING.md` states what the line and the terminal record do and do not promise.
arm_run_verdict

# The packages, in dependency order: a package may only name one already generated above it.
PACKAGES=(base audio device log network observability resources time config process display display-device security session input storage font graphics bluetooth)

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
	[display]="base process graphics"
	# THE DEVICE SIDE OF THE DISPLAY, which imports the shared graphics values and the base error and
	# declares no client-facing type of its own: a driver's wire and an application's wire are two
	# contracts, and one package carrying both would make every application depend on the device one.
	# QUOTED, because an unquoted hyphen inside a subscript reads as arithmetic to a shell formatter
	# and comes back as `[display - device]` - a key nothing looks up, which stops the whole loop with
	# `unbound variable` on the first hyphenated package.
	["display-device"]="base graphics"
	[security]="base process"
	[session]="base process"
	[input]="base"
	[storage]="base"
	[font]="base"
	# A VALUE-ONLY PACKAGE. It declares no interface and imports nothing: an extent and a pixel
	# format need no error type, and a shared vocabulary that depended on another package would make
	# every importer depend on that one too.
	[graphics]=""
	# THE HOST STACK'S CLIENT CONTRACTS. It imports the base error and nothing else: the HCI
	# transport a controller publishes is a DEVICE contract and lives in `device`, for the same
	# reason the display's device side is separate from the display's - a driver's wire and an
	# application's wire are two contracts, and one package carrying both would make every client
	# depend on the device one.
	[bluetooth]="base"
)

# The aggregate crate: no `--rust-package` of its own, every other package external, and the ONE
# invocation that writes docs/gen - the ABI manifests and the reference pages.
AGGREGATE_EXTERNAL=(audio base bluetooth config device display display-device font graphics input log network observability process resources security session storage time)

help() {
	usage_and_exit <<EOF
usage: gen.sh [--check | --accept-breaking] [--list] [--dry-run]

Regenerates the protocol bindings in src/user/libs/protocol/*, the aggregate crate src/proto, and
the ABI manifests and reference pages under docs/gen, from src/idl/*.lsidl - and the graphics
profile documents under docs/gen/render2d and docs/gen/render3d, from the profiles themselves.

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
	# THE ARGUMENTS AS A NAMED ARRAY, because the owned runner is a FUNCTION and `$@` inside it is its
	# own. A local is visible to what it calls; a positional parameter is not.
	local generate_args=("$@")
	if ((dry_run)); then
		# The words, joined with single spaces - so a mode with no flag does not print a double one
		# and read as a difference from the recipe it replaces.
		local words=(cargo run --quiet -- "${mode_flag[@]}" --rust-dir "../../$out/src" "$@" '../../idl/*.lsidl')
		printf '%s\n' "${words[*]}"
		return 0
	fi
	# Owned and waited for, so an interrupted generation stops now rather than after the generator
	# finishes - and so the verdict names the interruption.
	run_owned_shell '(cd "$SRC_DIR/tools/lsidl-gen" && cargo run --quiet -- "${mode_flag[@]}" --rust-dir "../../$out/src" "${generate_args[@]}" ../../idl/*.lsidl)'
}

for package in "${PACKAGES[@]}"; do
	out="$(crate_dir "$package")"
	args=(--rust-package "liber:$package@1")
	for external in ${EXTERNAL[$package]}; do
		# THE RUST SIDE SPELLS A HYPHEN AS AN UNDERSCORE and the wire side does not: `liber:display-device@1`
		# is the package, `display_device_proto` is the crate and `display_device` is the module. One
		# substitution here rather than a second list nobody keeps in step.
		args+=(--external-rust-package "liber:$external@1=${external//-/_}_proto::generated::liber::${external//-/_}")
	done
	generate "$out" "${args[@]}"
done

out="$(crate_dir proto)"
args=()
for external in "${AGGREGATE_EXTERNAL[@]}"; do
	args+=(--external-rust-package "liber:$external@1=${external//-/_}_proto::generated::liber::${external//-/_}")
done
args+=(--docs-dir ../../../docs/gen)
generate "$out" "${args[@]}"

# THE PROFILES THAT ARE CODE, AND ALSO WRITE UNDER docs/gen. Not LSIDL and not a package above:
# `Render2D Core Profile 1`, `Render3D Core Profile 1` and `OpenType Profile 1` are closed
# enumerations in `user/libs/graphics/profile` and `user/libs/text/opentype-profile`, and the table, the backend checklist, the conformance matrix and the
# capability report are generated from them with a hash over the canonical form. They are here
# because a reader who has just edited a profile runs the command the generated file names, and
# every generated file under docs/gen names this one.
# THE LAST-RESORT FACE, WHICH IS GENERATED AND NOT IMPORTED. The licence decision for fonts admits
# public domain and Unlicense only, so the only face this tree can stage is one it wrote - and a
# staged binary nobody can regenerate is a binary nobody can audit. `--check` proves the bytes on
# disk are the bytes this generator produces.
font_gen=(cargo run --quiet --offline --manifest-path tools/font-gen/Cargo.toml --)
[[ "$mode" == check ]] && font_gen+=(--check)
if ((dry_run)); then
	printf '%s\n' "${font_gen[*]}"
else
	(cd "$SRC_DIR" && "${font_gen[@]}")
fi

profile_doc=(cargo run --quiet --offline --manifest-path tools/profile-doc/Cargo.toml --)
[[ "$mode" == check ]] && profile_doc+=(--check)
if ((dry_run)); then
	printf '%s\n' "${profile_doc[*]}"
else
	(cd "$SRC_DIR" && "${profile_doc[@]}")
fi

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
check) note "no drift: ${#PACKAGES[@]} packages, the aggregate and every profile regenerates to what is on disk" ;;
*) note "${#PACKAGES[@]} packages and the aggregate regenerated and formatted, and every profile written" ;;
esac
