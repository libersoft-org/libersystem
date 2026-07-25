#!/usr/bin/env bash
# List every commit with its subject and line changes, then print total changes.
set -euo pipefail

MAX_MESSAGE_LENGTH=50

usage() {
	echo "usage: git-commits.sh [commit-count]" >&2
	exit 2
}

if (($# > 1)); then
	usage
fi

commit_count="${1:-}"
if [[ -n "$commit_count" && ! "$commit_count" =~ ^[1-9][0-9]*$ ]]; then
	echo "git-commits: commit-count must be a positive integer" >&2
	usage
fi

repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" || {
	echo "git-commits: not inside a Git repository" >&2
	exit 1
}
cd "$repo_root"

if ! git rev-parse --verify HEAD >/dev/null 2>&1; then
	echo "git-commits: repository has no commits" >&2
	exit 1
fi

if [[ -t 1 ]]; then
	RED=$'\033[31m'
	GREEN=$'\033[32m'
	RESET=$'\033[0m'
else
	RED=""
	GREEN=""
	RESET=""
fi

truncate_message() {
	local message="$1"
	if ((${#message} <= MAX_MESSAGE_LENGTH)); then
		printf '%s' "$message"
	else
		printf '%s...' "${message:0:MAX_MESSAGE_LENGTH-3}"
	fi
}

commit_stats() {
	local commit="$1"
	git show --format= --numstat "$commit" |
		awk '
			$1 ~ /^[0-9]+$/ && $2 ~ /^[0-9]+$/ {
				added += $1
				removed += $2
			}
			END { printf "%d %d\n", added, removed }
		'
}

total_added=0
total_removed=0
listed_commits=0
commit_args=(--reverse)
if [[ -n "$commit_count" ]]; then
	commit_args+=(--max-count="$commit_count")
fi

printf '%-7s %-50s %s %s %s\n' "Commit" "Message" "Removed" "Added" "Net"
printf '%-7s %-50s %s %s %s\n' "-------" "--------------------------------------------------" "-------" "-----" "---"

while IFS= read -r commit; do
	read -r added removed < <(commit_stats "$commit")
	message="$(truncate_message "$(git show -s --format=%s "$commit")")"
	net=$((added - removed))
	total_added=$((total_added + added))
	total_removed=$((total_removed + removed))
	listed_commits=$((listed_commits + 1))

	printf '%-7s %-50s %s-%d%s %s+%d%s %+d\n' "$(git show -s --format=%h "$commit")" "$message" "$RED" "$removed" "$RESET" "$GREEN" "$added" "$RESET" "$net"
done < <(git rev-list "${commit_args[@]}" HEAD)

total_net=$((total_added - total_removed))

printf '\nSummary (%d commits)\n' "$listed_commits"
printf 'Removed: %s%8s%s\n' "$RED" "-$total_removed" "$RESET"
printf 'Added:   %s%8s%s\n' "$GREEN" "+$total_added" "$RESET"
printf 'Net:     %+8d\n' "$total_net"
