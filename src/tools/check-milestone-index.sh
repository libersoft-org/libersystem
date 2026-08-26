#!/usr/bin/env bash
# A milestone the index calls done must have no unfinished tasks in it.
#
# `docs/todo/TODO.md` is the index everyone reads first, and its checkbox is the only summary of a
# milestone anybody sees without opening the file. Three of them said `- [x]` over documents holding
# open `- [ ]` items - one of them reopened by an audit that same day, one carrying a defect its own
# text calls "Still open, and I do not know why". A reader following the index would have concluded
# the work was finished.
#
# Nothing catches this by being careful. Reopening a milestone means editing two files, and the
# second one is somewhere else, so it gets forgotten exactly when the news matters most: the moment
# something turns out not to be done.
#
# Only the dangerous direction is checked. `- [ ]` over a file with no open tasks is a milestone
# whose remaining work is prose, which is ordinary; `- [x]` over a file WITH open tasks is a claim
# the document itself contradicts.
#
# `[~]` COUNTS AS OPEN. It is a useful third state in a long document - "partly done" is a real thing
# to say - and it is not a state that may make a milestone read as finished to the one check that
# exists to prevent exactly that. One document passed this gate as COMPLETE over its own text saying its
# adversarial tests do not exist, because the marker its test item used was not the one being
# counted. One item in the whole tree at the time, which is why closing it while it was one was
# cheap.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
index="${MILESTONE_INDEX:-$root/../docs/todo/TODO.md}"
todo_dir="$(dirname "$index")"

check_index() {
	local index="$1" todo_dir="$2" failed=0
	while IFS= read -r line; do
		# `- [x] [<id> - title](<id>.md)` - the file is the last parenthesised group.
		local mark file
		mark="${line:3:1}"
		file="${line##*\(}"
		file="${file%%\)*}"
		[[ "$file" == *.md ]] || continue
		if [[ ! -f "$todo_dir/$file" ]]; then
			# Only a row claiming DONE is worth failing over here; an open row naming a file that
			# does not exist yet is a plan, which is what this index is for.
			[[ "$mark" == "x" ]] || continue
			echo "milestone-index: $file is in the index and not in $todo_dir" >&2
			failed=1
			continue
		fi

		# THE TITLE IS PART OF THE CLAIM, AND IT DRIFTS SILENTLY.
		#
		# This gate was written because a checkbox here is "the only summary of a milestone anybody
		# sees without opening the file" - and the title beside it is the rest of that summary. Two
		# milestones were retitled in one afternoon, and the index went on describing them by what
		# they used to be about: one still promised a stop that had moved to another milestone, the
		# other named a selection rule that had been deleted as wrong. Nothing caught it, for the
		# same reason nothing caught the checkbox: the news lives in one file and the summary in
		# another. Checked on every row, done or not, because an open milestone is the one a reader
		# is deciding whether to pick up.
		local row_title doc_title rest
		rest="${line#*\] \[}"
		row_title="${rest%%\]*}"
		doc_title="$(head -1 "$todo_dir/$file")"
		doc_title="${doc_title#\# }"
		if [[ "$row_title" != "$doc_title" ]]; then
			echo "milestone-index: the index calls $file" >&2
			echo "    \"$row_title\"" >&2
			echo "  and the document calls itself" >&2
			echo "    \"$doc_title\"" >&2
			failed=1
		fi

		[[ "$mark" == "x" ]] || continue
		local open
		open="$(grep -cE '^- \[[ ~]\]' "$todo_dir/$file" || true)"
		if [[ "$open" != "0" ]]; then
			echo "milestone-index: the index marks $file done and it has $open unfinished task(s)" >&2
			failed=1
		fi
	done < <(grep '^- \[.\] \[P' "$index" || true)
	return "$failed"
}

# Prove the gate REFUSES before letting it approve. The tree is consistent right now, so a run over
# the tree proves only that the tree is consistent - it would pass just as well if the `grep` above
# had stopped matching, which is how six gates came to report success for a month.
self_test() {
	local scratch
	scratch="$(mktemp -d)"
	trap 'rm -rf "$scratch"' RETURN

	printf '# P99M0001 - a\n\nStatus: DONE.\n\n- [x] finished\n' >"$scratch/P99M0001.md"
	printf '# P99M0002 - b\n\nStatus: IN PROGRESS.\n\n- [x] finished\n- [ ] not finished\n' >"$scratch/P99M0002.md"

	# A consistent index is accepted.
	printf -- '- [x] [P99M0001 - a](P99M0001.md)\n- [ ] [P99M0002 - b](P99M0002.md)\n' >"$scratch/ok.md"
	if ! check_index "$scratch/ok.md" "$scratch" 2>/dev/null; then
		echo "milestone-index: SELF-TEST FAILED - a consistent index was refused" >&2
		exit 1
	fi

	# A title the document no longer uses, on a row that is NOT marked done - which is where the
	# drift was actually found.
	printf -- '- [x] [P99M0001 - a](P99M0001.md)\n- [ ] [P99M0002 - what it used to be about](P99M0002.md)\n' >"$scratch/stale-title.md"
	if check_index "$scratch/stale-title.md" "$scratch" 2>/dev/null; then
		echo "milestone-index: SELF-TEST FAILED - an index row describing a milestone by a title the document no longer carries was accepted" >&2
		exit 1
	fi

	# The defect this exists for: done in the index, unfinished in the document.
	printf -- '- [x] [P99M0001 - a](P99M0001.md)\n- [x] [P99M0002 - b](P99M0002.md)\n' >"$scratch/bad.md"
	if check_index "$scratch/bad.md" "$scratch" 2>/dev/null; then
		echo "milestone-index: SELF-TEST FAILED - a milestone marked done over an unfinished document was accepted, which is the one thing this gate is for" >&2
		exit 1
	fi

	# And the PARTLY-done marker, which is the one that slipped through: `[~]` is a third state a
	# long document legitimately wants, and one read as COMPLETE over its own text saying its
	# adversarial tests do not exist because this gate was counting only `[ ]`. A fixture belongs
	# beside the `[ ]` one for the same reason that one is here.
	printf '# P99M0003\n\nStatus: DONE.\n\n- [x] finished\n- [~] partly done\n' >"$scratch/P99M0003.md"
	printf -- '- [x] [P99M0003 - c](P99M0003.md)\n' >"$scratch/partial.md"
	if check_index "$scratch/partial.md" "$scratch" 2>/dev/null; then
		echo "milestone-index: SELF-TEST FAILED - a milestone marked done over a PARTLY-done item was accepted" >&2
		exit 1
	fi

	# And a named file that is not there, which is how a rename presents.
	printf -- '- [x] [P99M0009 - gone](P99M0009.md)\n' >"$scratch/missing.md"
	if check_index "$scratch/missing.md" "$scratch" 2>/dev/null; then
		echo "milestone-index: SELF-TEST FAILED - an index entry naming a file that does not exist was accepted" >&2
		exit 1
	fi
}

self_test
check_index "$index" "$todo_dir"
echo "milestone-index: clean"
