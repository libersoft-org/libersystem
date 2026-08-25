#!/usr/bin/env bash
# Update dependencies for every Cargo project in the repository.
set -euo pipefail

SCRIPT_NAME=update.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

help() {
	usage_and_exit <<EOF
usage: update.sh

Updates every Cargo.lock in the repository with cargo update. Cargo workspaces
are updated once, even when more than one member contains a Cargo.toml.

Generated copies in .build/, Git metadata and target/ directories are ignored.
EOF
}

if [[ $# -gt 0 ]]; then
	case "$1" in
	-h | --help)
		[[ $# -eq 1 ]] || die "unexpected argument '$2' (try --help)"
		help
		;;
	*) die "unexpected argument '$1' (try --help)" ;;
	esac
fi

command -v cargo >/dev/null 2>&1 || die "cargo was not found (run ./setup.sh first)"

# Discover manifests instead of maintaining another package list. Several manifests may belong to
# one workspace and therefore share one lockfile; `cargo locate-project --workspace` gives that
# lockfile's owning manifest so it can be updated exactly once.
declare -A projects=()
manifest_count=0

while IFS= read -r -d '' manifest; do
	((manifest_count += 1))
	manifest_dir="$(dirname -- "$manifest")"
	project_manifest="$({
		cd "$manifest_dir"
		cargo locate-project --workspace --message-format plain
	})" || die "cannot locate Cargo project for ${manifest#"$REPO_ROOT"/}"
	[[ -f "$project_manifest" ]] || die "Cargo returned a missing manifest: $project_manifest"
	projects["$project_manifest"]=1
done < <(
	find "$REPO_ROOT" \
		-type d \( -name .git -o -name .build -o -name target \) -prune -o \
		-type f -name Cargo.toml -print0 | LC_ALL=C sort -z
)

((manifest_count > 0)) || die "no Cargo.toml files found"
mapfile -t project_manifests < <(printf '%s\n' "${!projects[@]}" | LC_ALL=C sort)

note "found $manifest_count manifests in ${#project_manifests[@]} Cargo projects"

for project_manifest in "${project_manifests[@]}"; do
	project_dir="$(dirname -- "$project_manifest")"
	if [[ "$project_dir" == "$REPO_ROOT" ]]; then
		relative_dir=.
	else
		relative_dir="${project_dir#"$REPO_ROOT"/}"
	fi
	note "updating $relative_dir"
	(
		cd "$project_dir"
		cargo update
	)
done

note "updated ${#project_manifests[@]} Cargo projects"
