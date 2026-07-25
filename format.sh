#!/usr/bin/env bash
# Format the whole project, or only changed source files with --changed.
set -euo pipefail

ROOT="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
cd "$ROOT"

usage() {
	echo "usage: format.sh [--changed]" >&2
	exit 2
}

if (($# > 1)); then
	usage
fi

if (($# == 0)); then
	(cd "$ROOT/src" && just fmt)
	shopt -s nullglob
	root_shell_files=("$ROOT"/*.sh)
	if ((${#root_shell_files[@]} > 0)); then
		shfmt -w "${root_shell_files[@]}"
	fi
	exit 0
fi

if [[ "$1" != "--changed" ]]; then
	usage
fi

mapfile -d '' -t changed_files < <(
	{
		git diff --name-only -z --diff-filter=ACMR
		git diff --cached --name-only -z --diff-filter=ACMR
		git ls-files --others --exclude-standard -z
	} | LC_ALL=C sort -zu
)

rust_files=()
shell_files=()
toml_files=()
format_justfile=0

for file in "${changed_files[@]}"; do
	[[ -f "$file" ]] || continue
	case "$file" in
	*.rs) rust_files+=("$file") ;;
	*.sh) shell_files+=("$file") ;;
	*.toml) toml_files+=("$file") ;;
	src/Justfile) format_justfile=1 ;;
	esac
done

if ((${#rust_files[@]} > 0)); then
	rustfmt +nightly --edition 2024 --config-path "$ROOT/rustfmt.toml" "${rust_files[@]}"
fi
if ((${#shell_files[@]} > 0)); then
	shfmt -w "${shell_files[@]}"
fi
if ((${#toml_files[@]} > 0)); then
	taplo fmt "${toml_files[@]}"
fi
if ((format_justfile)); then
	(cd "$ROOT/src" && just --fmt)
fi
