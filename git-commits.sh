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
removed_width=7
added_width=5
net_width=3
commit_ids=()
commit_messages=()
commit_removed=()
commit_added=()
commit_net=()
commit_args=(--reverse)
if [[ -n "$commit_count" ]]; then
	commit_args+=(--max-count="$commit_count")
fi

separator() {
	printf '%*s' "$1" '' | tr ' ' '-'
}

while IFS= read -r commit; do
	read -r added removed < <(commit_stats "$commit")
	commit_id="$(git show -s --format=%h "$commit")"
	message="$(truncate_message "$(git show -s --format=%s "$commit")")"
	net=$((added - removed))
	removed_text="-$removed"
	added_text="+$added"
	printf -v net_text '%+d' "$net"
	total_added=$((total_added + added))
	total_removed=$((total_removed + removed))
	listed_commits=$((listed_commits + 1))
	commit_ids+=("$commit_id")
	commit_messages+=("$message")
	commit_removed+=("$removed_text")
	commit_added+=("$added_text")
	commit_net+=("$net_text")

	if ((${#removed_text} > removed_width)); then
		removed_width=${#removed_text}
	fi
	if ((${#added_text} > added_width)); then
		added_width=${#added_text}
	fi
	if ((${#net_text} > net_width)); then
		net_width=${#net_text}
	fi
done < <(git rev-list "${commit_args[@]}" HEAD)

printf '%-7s %-50s %*s %*s %*s\n' "Commit" "Message" "$removed_width" "Removed" "$added_width" "Added" "$net_width" "Net"
printf '%-7s %-50s %s %s %s\n' "-------" "--------------------------------------------------" "$(separator "$removed_width")" "$(separator "$added_width")" "$(separator "$net_width")"

for index in "${!commit_ids[@]}"; do
	printf '%-7s %-50s ' "${commit_ids[index]}" "${commit_messages[index]}"
	printf '%s%*s%s ' "$RED" "$removed_width" "${commit_removed[index]}" "$RESET"
	printf '%s%*s%s ' "$GREEN" "$added_width" "${commit_added[index]}" "$RESET"
	printf '%*s\n' "$net_width" "${commit_net[index]}"
done

total_net=$((total_added - total_removed))

printf '\nSummary (%d commits)\n' "$listed_commits"
printf 'Removed: %s%8s%s\n' "$RED" "-$total_removed" "$RESET"
printf 'Added:   %s%8s%s\n' "$GREEN" "+$total_added" "$RESET"
printf 'Net:     %+8d\n' "$total_net"
