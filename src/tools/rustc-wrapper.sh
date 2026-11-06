#!/usr/bin/env bash
# The compiler, as cargo runs it for every build of this tree (`.cargo/config.toml` names this file):
# the same source compiles to the same bytes wherever the tree is.
#
# WHY. rustc derives a crate's identity - the hash in each of its mangled symbols, and which upstream
# crate's copy of a shared generic a dependent links - from `-C metadata`. Cargo puts into that value
# the absolute path of every path dependency outside the package's own workspace, and every crate here
# is its own workspace that reaches its neighbours by `../` paths. So one commit built in place and in
# a sealed copy of the tree at another path gave two images: other symbol names, other providers
# owning the same generic, other sizes - and the tracked dynamic report, written in place, could not
# match a release built anywhere else. Panic locations carried the tree's absolute path as well.
#
# WHAT. Cargo calls this with the compiler as the first argument. For a crate of this tree - cargo
# runs rustc inside the package's directory, so the working directory says which - `-C metadata` is
# replaced by a hash of the command line cargo itself wrote, with the tree's path taken out, and of
# the identities of the crates it links, which this file recorded when it compiled them. What cargo
# appends after its own arguments is not part of it - RUSTFLAGS, `cargo rustc -- ...` and what build
# scripts print - because cargo leaves those out of `-C metadata` too: the quarantine consumers are
# compiled with `-C force-unwind-tables=no` and call the image's runtime by its identity, which a hash
# of every argument made two identities. A crate from outside the tree - the
# standard library that build-std compiles, a registry crate - keeps cargo's value, which does not
# depend on where this tree is (cargo hashes those by sysroot-relative path and registry URL). A
# dependency compiled before this file existed has no record, and then cargo's value stays: that is
# the old behaviour, not a wrong one, and the build's cache keys name this file so that a build which
# must be reproducible starts from nothing. `-C extra-filename` names files in the target directory
# and is compiled into nothing, so it stays cargo's. Every compile also maps the tree's path to `.` in
# the paths it embeds.
#
# Anything that is not a crate compile (`-vV`, `--print`) runs unchanged.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
compiler="$1"
shift

# A response file holds the arguments one per line; cargo writes one when a command line is too long.
args=()
response=0
for arg in "$@"; do
	if [[ "$arg" == @* && -f "${arg#@}" ]]; then
		mapfile -t lines <"${arg#@}"
		args+=("${lines[@]}")
		response=1
	else
		args+=("$arg")
	fi
done

metadata=""
metadata_index=-1
extra=""
out_dir=""
library=0
unresolved=0
stable_dependency=0
hashed=()
links=()

# A linked crate is named by the identity recorded beside its file, never by the file's path: the
# file name carries cargo's `extra-filename`, which is as path-dependent as the metadata it repeats.
link() {
	local spec="$1" name path file stem id
	name="${spec%%=*}"
	if [[ "$spec" != *=* ]]; then
		links+=("$name")
		return
	fi
	path="${spec#*=}"
	file="${path##*/}"
	stem="${file%.*}"
	id=""
	if [[ -f "${path%/*}/.liber-crate-id/$stem" ]]; then
		read -r id <"${path%/*}/.liber-crate-id/$stem" || true
	fi
	if [[ -z "$id" ]]; then
		unresolved=1
		return
	fi
	if [[ "$id" == stable:* ]]; then stable_dependency=1; fi
	links+=("$name=$id")
}

crate_type() {
	case "$1" in
	lib | rlib | dylib | proc-macro) library=1 ;;
	esac
}

# WHERE CARGO'S OWN ARGUMENTS END: after the dependencies, `-L dependency=` and the `--extern`s, which
# cargo writes last before what it appends from RUSTFLAGS, `cargo rustc --` and build scripts. Only
# those two: a build script's search path is `-L native=` (or another kind) and comes after RUSTFLAGS.
identity_end=${#args[@]}
for ((index = 0; index < ${#args[@]}; index++)); do
	case "${args[index]}" in
	--extern) identity_end=$((index + 2)) ;;
	--extern=* | -Ldependency=*) identity_end=$((index + 1)) ;;
	-L) if [[ "${args[index + 1]-}" == dependency=* ]]; then identity_end=$((index + 2)); fi ;;
	esac
done

for ((index = 0; index < ${#args[@]}; index++)); do
	arg="${args[index]}"
	if ((index >= identity_end)); then
		case "$arg" in
		-C | --codegen)
			index=$((index + 1))
			case "${args[index]-}" in
			metadata=*)
				metadata="${args[index]#metadata=}"
				metadata_index=$index
				;;
			esac
			;;
		-Cmetadata=*)
			metadata="${arg#-Cmetadata=}"
			metadata_index=$index
			;;
		esac
		continue
	fi
	case "$arg" in
	-C | --codegen)
		index=$((index + 1))
		value="${args[index]-}"
		case "$value" in
		metadata=*)
			metadata="${value#metadata=}"
			metadata_index=$index
			;;
		extra-filename=*) extra="${value#extra-filename=}" ;;
		incremental=*) ;;
		*) hashed+=(-C "${value//"$root"/.}") ;;
		esac
		;;
	-Cmetadata=*)
		metadata="${arg#-Cmetadata=}"
		metadata_index=$index
		;;
	-Cextra-filename=*) extra="${arg#-Cextra-filename=}" ;;
	-Cincremental=*) ;;
	--out-dir)
		index=$((index + 1))
		out_dir="${args[index]-}"
		;;
	--out-dir=*) out_dir="${arg#--out-dir=}" ;;
	-L) index=$((index + 1)) ;;
	-L*) ;;
	--extern)
		index=$((index + 1))
		link "${args[index]-}"
		;;
	--extern=*) link "${arg#--extern=}" ;;
	--crate-type)
		index=$((index + 1))
		crate_type "${args[index]-}"
		hashed+=("$arg" "${args[index]-}")
		;;
	--crate-type=*)
		crate_type "${arg#--crate-type=}"
		hashed+=("$arg")
		;;
	# How diagnostics are printed, which the terminal's width and colour decide.
	--error-format=* | --json=* | --diagnostic-width=* | --color=*) ;;
	--error-format | --json | --color) index=$((index + 1)) ;;
	*) hashed+=("${arg//"$root"/.}") ;;
	esac
done

if [[ -z "$metadata" ]]; then
	exec "$compiler" "$@"
fi

directory="$(pwd -P)"
id="cargo:$metadata"
if ((unresolved == 0)) && [[ "$directory" == "$root" || "$directory" == "$root"/* || "$stable_dependency" == 1 ]]; then
	mapfile -t links < <(printf '%s\n' "${links[@]}" | LC_ALL=C sort)
	digest="$(printf '%s\0' "directory=${directory//"$root"/.}" "${hashed[@]}" "linked" "${links[@]}" | sha256sum)"
	metadata="${digest:0:16}"
	id="stable:$metadata"
	if [[ "${args[metadata_index]}" == -Cmetadata=* ]]; then
		args[metadata_index]="-Cmetadata=$metadata"
	else
		args[metadata_index]="metadata=$metadata"
	fi
fi

if ((library == 1)) && [[ -n "$out_dir" ]]; then
	crate_name=""
	for ((index = 0; index + 1 < ${#args[@]}; index++)); do
		if [[ "${args[index]}" == --crate-name ]]; then crate_name="${args[index + 1]}"; fi
	done
	if [[ -n "$crate_name" ]]; then
		mkdir -p "$out_dir/.liber-crate-id"
		record="$out_dir/.liber-crate-id/lib$crate_name$extra"
		printf '%s\n' "$id" >"$record.$$"
		mv -f "$record.$$" "$record"
	fi
fi

args+=("--remap-path-prefix=$root=.")
if ((response == 1)); then
	file="$(mktemp)"
	trap 'rm -f "$file"' EXIT
	printf '%s\n' "${args[@]}" >"$file"
	"$compiler" "@$file"
	exit
fi
exec "$compiler" "${args[@]}"
