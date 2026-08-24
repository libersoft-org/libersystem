#!/usr/bin/env bash
# Fetch the pinned external verification artifacts named in `toolchain.lock`.
#
# ONE COMMAND, AND THE GATES DO NOT RUN IT. A verification gate that can reach the network is a gate
# whose answer depends on what a mirror served today; the gates here read the cache and, when the
# artifact is absent, print this command. So the fetch is a thing a person does once, deliberately,
# and the checking is a thing that happens offline against a digest.
#
# Usage:
#   ./bootstrap.sh              fetch everything the lock names that is missing or wrong
#   ./bootstrap.sh tla2tools    fetch one artifact by its lock section name
#   ./bootstrap.sh --verify     check what is cached and fetch nothing

set -euo pipefail

cd "$(dirname "$0")"
LOCK="toolchain.lock"
[[ -f "$LOCK" ]] || {
	echo "bootstrap: $LOCK is missing - there is nothing pinned to fetch" >&2
	exit 1
}

want="${1:-}"
verify_only=0
if [[ "$want" == "--verify" ]]; then
	verify_only=1
	want=""
fi

# One artifact's fields, read out of its `[section]` block. Deliberately a small reader rather than a
# TOML parser: the lock is written by hand and read by two scripts, and a dependency that has to be
# fetched before the fetcher runs is not a dependency this can have.
field() {
	local section="$1" key="$2"
	awk -v section="[$section]" -v key="$key" '
		$0 == section { inside = 1; next }
		/^\[/ { inside = 0 }
		inside && $1 == key {
			sub(/^[^=]*=[ \t]*/, "")
			gsub(/^"|"$/, "")
			print
			exit
		}
	' "$LOCK"
}

sections() {
	grep -oE '^\[[a-z0-9_-]+\]' "$LOCK" | tr -d '[]'
}

status=0
for section in $(sections); do
	[[ -z "$want" || "$want" == "$section" ]] || continue
	url="$(field "$section" url)"
	sha="$(field "$section" sha256)"
	cache="$(field "$section" cache)"
	purpose="$(field "$section" purpose)"
	if [[ -z "$url" || -z "$sha" || -z "$cache" ]]; then
		echo "bootstrap: [$section] is missing url, sha256 or cache" >&2
		status=1
		continue
	fi

	if [[ -f "$cache" ]]; then
		have="$(sha256sum "$cache" | awk '{print $1}')"
		if [[ "$have" == "$sha" ]]; then
			echo "bootstrap: $section is cached and matches its pin ($cache)"
			continue
		fi
		# NOT OVERWRITTEN IN PLACE. A cached file whose digest is wrong is evidence about how it got
		# there, and a fetch that silently replaces it destroys that before anybody has looked.
		echo "bootstrap: $cache does not match the pin in $LOCK" >&2
		echo "bootstrap:   pinned $sha" >&2
		echo "bootstrap:   cached $have" >&2
		echo "bootstrap: move it aside and run this again if the pin is the one you mean" >&2
		status=1
		continue
	fi

	if ((verify_only)); then
		echo "bootstrap: $section is NOT cached - run ./bootstrap.sh $section" >&2
		status=1
		continue
	fi

	echo "bootstrap: fetching $section - $purpose"
	mkdir -p "$(dirname "$cache")"
	tmp="$cache.fetching.$$"
	if ! curl -sSL --fail -o "$tmp" "$url"; then
		rm -f "$tmp"
		echo "bootstrap: could not fetch $url" >&2
		status=1
		continue
	fi
	have="$(sha256sum "$tmp" | awk '{print $1}')"
	if [[ "$have" != "$sha" ]]; then
		# VERIFIED BEFORE IT IS PUT WHERE ANYTHING WILL RUN IT. The whole point of the pin is that
		# what arrives is checked before it becomes the thing a gate executes.
		rm -f "$tmp"
		echo "bootstrap: what $url served does not match the pin" >&2
		echo "bootstrap:   pinned $sha" >&2
		echo "bootstrap:   served $have" >&2
		status=1
		continue
	fi
	mv "$tmp" "$cache"
	echo "bootstrap: $section verified and cached at $cache"
done

exit "$status"
