#!/usr/bin/env bash
# Evidence publication for producers: sourced by the gate runner, the guest runner, the image
# builder, the release orchestration and the multi-boot gates.
#
# ACTIVE ONLY INSIDE A RUN. `LIBER_VERIFY_RUN` names the durable run directory `verify-model
# run-start` created (outside the worktree); when it is unset, every function here is a no-op and an
# ordinary scoped run publishes nothing. When it is set, a producer KEEPS its logs in the run before
# its own cleanup and PUBLISHES one envelope per plan-item key it discharged, atomically, naming the
# run, the key, the identity, its inputs and outputs by digest, its outcome and its duration. The
# collector (`verify-model dossier`) refuses a run whose required keys lack one.
#
# THE FUNCTIONS NEVER FAIL THE PRODUCER. Losing an envelope is reported and costs the dossier - a
# required key with no envelope is a refusal there - not the run that produced the evidence.
#
# `evidence_image` is the one function with a verdict: inside a run, a gate reads the image THIS run
# produced or fails; it never falls back to whatever `.build/boot` holds.

EVIDENCE_REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

evidence_active() {
	[[ -n "${LIBER_VERIFY_RUN:-}" && -d "${LIBER_VERIFY_RUN:-}" ]]
}

# The model's binary, run from its own crate directory so cargo finds the workspace's target
# directory whichever directory the producer is in.
evidence_model() {
	(cd "$EVIDENCE_REPO/src/tools/verify-model" && cargo run --quiet --manifest-path "$EVIDENCE_REPO/src/tools/verify-model/Cargo.toml" -- "$@")
}

# evidence_keep KEY FILE... - copy each existing, non-empty file into the run under KEY. Called from
# a producer's cleanup, BEFORE it removes its temporary directory, so the bytes outlive the producer
# whatever its outcome; the envelope written afterwards names the copies.
evidence_keep() {
	evidence_active || return 0
	local key="$1" file
	shift
	local -a files=()
	for file in "$@"; do
		[[ -s "$file" ]] && files+=(--log "$file")
	done
	((${#files[@]})) || return 0
	if ! evidence_model keep-log --run "$LIBER_VERIFY_RUN" --key "$key" "${files[@]}" >/dev/null 2>"$LIBER_VERIFY_RUN/.keep-log.err"; then
		echo "evidence: the logs for '$key' were NOT kept: $(tail -1 "$LIBER_VERIFY_RUN/.keep-log.err" 2>/dev/null)" >&2
	fi
	rm -f "$LIBER_VERIFY_RUN/.keep-log.err"
	return 0
}

# evidence_keep_gate FILE... - keep under the key of the gate check.sh is running, which check.sh
# exports as `LIBER_GATE_KEY`. A gate started by hand has no key and keeps nothing.
evidence_keep_gate() {
	[[ -n "${LIBER_GATE_KEY:-}" ]] || return 0
	evidence_keep "$LIBER_GATE_KEY" "$@"
}

# evidence_publish KEY PRODUCER OUTCOME SECONDS [envelope arguments...]
#
# Extra arguments: `--input PATH`, `--output PATH` (recorded by path and digest), `--log PATH`
# (copied into the run), `--discharges-file FILE` (one key per line this envelope discharges beside
# its own), `--if-absent` (write nothing when the key already has an envelope - the runner's fallback
# for a producer that publishes its own).
evidence_publish() {
	evidence_active || return 0
	local key="$1" producer="$2" outcome="$3" seconds="$4"
	shift 4
	local report
	if ! report="$(evidence_model envelope --run "$LIBER_VERIFY_RUN" --key "$key" --producer "$producer" --outcome "$outcome" --seconds "$seconds" "$@" 2>&1)"; then
		echo "evidence: the envelope for '$key' was NOT published - the dossier will say so: ${report##*$'\n'}" >&2
	else
		echo "evidence: ${report##*$'\n'}" >&2
	fi
	return 0
}

# evidence_store_artifact NAME PATH [SIDECAR...] - copy a produced artifact into the run's artifact
# store, immutable and atomically, with its receipt sidecars beside it. Prints the stored path. An
# artifact already stored under NAME is never replaced: the first producer's bytes are the run's.
evidence_store_artifact() {
	evidence_active || return 0
	local name="$1" path="$2" store="$LIBER_VERIFY_RUN/artifacts" sidecar
	shift 2
	mkdir -p "$store"
	if [[ -e "$store/$name" ]]; then
		echo "evidence: $store/$name is already stored - this run's artifact is the first one published, and the copy assembled now is not stored over it" >&2
		echo "$store/$name"
		return 0
	fi
	cp "$path" "$store/$name.$$.tmp" && chmod 0444 "$store/$name.$$.tmp" && mv -f "$store/$name.$$.tmp" "$store/$name" || {
		echo "evidence: could not store $path as $store/$name" >&2
		rm -f "$store/$name.$$.tmp"
		return 1
	}
	for sidecar in "$@"; do
		[[ -f "$sidecar" ]] || continue
		cp "$sidecar" "$store/$name${sidecar#"$path"}.$$.tmp" && chmod 0444 "$store/$name${sidecar#"$path"}.$$.tmp" && mv -f "$store/$name${sidecar#"$path"}.$$.tmp" "$store/$name${sidecar#"$path"}"
	done
	echo "$store/$name"
}

# evidence_image NAME - the shipping image a gate boots. Inside a run: the artifact the run's image
# producer stored, and a FAILURE when the run has none - a gate whose named input was not produced by
# this run does not read a shared `.build/boot` file by convention. Outside a run: the tree's own.
evidence_image() {
	local name="$1"
	if evidence_active; then
		if [[ -f "$LIBER_VERIFY_RUN/artifacts/$name" ]]; then
			echo "$LIBER_VERIFY_RUN/artifacts/$name"
			return 0
		fi
		echo "evidence: this run produced no $name - the image producer did not run or did not publish, and a gate does not take one from the tree instead" >&2
		return 1
	fi
	echo "$EVIDENCE_REPO/.build/boot/$name"
}
