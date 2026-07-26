#!/bin/bash
# Git daily stats — lines added / removed / net per day + total summary

if [[ -t 1 ]]; then
	RED=$'\033[31m'
	GREEN=$'\033[32m'
	NET_PLUS=$'\033[93m'
	NET_MINUS=$'\033[38;5;130m'
	RESET=$'\033[0m'
else
	RED=""
	GREEN=""
	NET_PLUS=""
	NET_MINUS=""
	RESET=""
fi

echo ""
echo "=== Git Daily Stats ==="
echo ""

printf "%-12s %8s %8s %8s %8s\n" "Date" "Commits" "Removed" "Added" "Net"
printf "%-12s %8s %8s %8s %8s\n" "----------" "-------" "------" "------" "------"

total_added=0
total_removed=0

git log --reverse --format="%ad" --date=short | sort -u | while read -r day; do
	commits=$(git log --after="$day 00:00:00" --before="$day 23:59:59" --date=short --format="%H" | wc -l | tr -d ' ')
	added=$(git log --after="$day 00:00:00" --before="$day 23:59:59" --date=short --numstat --pretty="" | awk '{ if ($1 != "-") s += $1 } END { print s+0 }')
	removed=$(git log --after="$day 00:00:00" --before="$day 23:59:59" --date=short --numstat --pretty="" | awk '{ if ($2 != "-") s += $2 } END { print s+0 }')
	net=$((added - removed))
	# Colour around the padded field, never inside it: the escape bytes have no width on
	# screen but printf counts them, which would shorten every coloured column.
	printf "%-12s %8d " "$day" "$commits"
	printf "%s%8s%s " "$RED" "-$removed" "$RESET"
	printf "%s%8s%s " "$GREEN" "+$added" "$RESET"
	if ((net < 0)); then
		printf "%s%+8d%s\n" "$NET_MINUS" "$net" "$RESET"
	else
		printf "%s%+8d%s\n" "$NET_PLUS" "$net" "$RESET"
	fi
	total_added=$((total_added + added))
	total_removed=$((total_removed + removed))
done

echo ""
echo "=== Total Summary ==="
echo ""

total_added=$(git log --numstat --pretty="" | awk '{ if ($1 != "-") s += $1 } END { print s+0 }')
total_removed=$(git log --numstat --pretty="" | awk '{ if ($2 != "-") s += $2 } END { print s+0 }')
total_net=$((total_added - total_removed))
total_commits=$(git rev-list --count HEAD)
active_days=$(git log --format="%ad" --date=short | sort -u | wc -l | tr -d ' ')

printf "Removed: %s%8s%s lines\n" "$RED" "-$total_removed" "$RESET"
printf "Added:   %s%8s%s lines\n" "$GREEN" "+$total_added" "$RESET"
if ((total_net < 0)); then
	printf "Net:     %s%+8d%s lines\n" "$NET_MINUS" "$total_net" "$RESET"
else
	printf "Net:     %s%+8d%s lines\n" "$NET_PLUS" "$total_net" "$RESET"
fi
printf "Commits: %8d\n" "$total_commits"
printf "Active days: %4d\n" "$active_days"
echo ""
