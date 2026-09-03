#!/usr/bin/env bash
# EVERY CAPABILITY THE MODEL DECLARES IS ONE THE GRANT LOOP WALKS.
#
# PermissionManager hands a launched program its capabilities by walking `VOCABULARY` and sending
# each one the program's manifest row grants. So a capability that is IN the model and NOT in that
# array is one no row can deliver, however plainly the row grants it - and the failure is silent on
# both sides: the manager sends nothing and reports success, and the program reads its grants
# positionally, so it takes the NEXT capability under the missing one's tag or waits for a message
# nobody will send.
#
# It happened twice, to two capabilities, and both were written down as known before this gate
# existed. `Session` was noted in a comment - "the loop never sends it ... it waits for the
# vocabulary to be fixed" - and `kill` shipped unable to end anything. `DevicePolicy` was not noted
# at all: `lsdev` has held it since the operator verbs were built, received the CONFIGURATION client
# under the policy tag, and answered "this boot granted no device-policy authority" - so `disable`,
# `enable`, `select` and `retry` were unreachable on every machine. The only check that drives one is
# a development check, and the development instance would not boot.
#
# The model is the IDL enum, which is generated, so this compares the delivery list against the
# declaration rather than against a second hand-written list.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
model="$root/../user/libs/protocol/security-proto/src/generated/liber/security/v1.rs"
loop_file="$root/../user/services/core/src/permission_manager.rs"
failed=0

for f in "$model" "$loop_file"; do
	[[ -f "$f" ]] || {
		echo "grant-vocabulary: $f is not there to read" >&2
		exit 1
	}
done

# The declared set: the `Capability` enum's variants, in the generated model.
declared="$(awk '/^pub enum Capability \{/{inside=1;next} inside && /^\}/{exit} inside && /= [0-9]+,/{gsub(/^[ \t]+|[ \t]*=.*$/,"");print}' "$model" | sort -u)"
[[ -n "$declared" ]] || {
	echo "grant-vocabulary: the model declares no capabilities, which means this gate is reading the wrong file" >&2
	exit 1
}

# The delivered set: what the grant loop walks.
delivered="$(awk '/^const VOCABULARY: \[Capability; [0-9]+\] = \[/{inside=1;next} inside && /^\];/{exit} inside && /Capability::/{gsub(/^[ \t]*Capability::|,[ \t]*$/,"");print}' "$loop_file" | sort)"
[[ -n "$delivered" ]] || {
	echo "grant-vocabulary: no VOCABULARY array was found in $loop_file" >&2
	exit 1
}

missing="$(comm -23 <(printf '%s\n' "$declared") <(printf '%s\n' "$delivered" | sort -u) || true)"
if [[ -n "$missing" ]]; then
	echo "grant-vocabulary: the model declares capabilities the grant loop never walks, so no manifest row can deliver them:" >&2
	printf '  %s\n' $missing >&2
	failed=1
fi

extra="$(comm -13 <(printf '%s\n' "$declared") <(printf '%s\n' "$delivered" | sort -u) || true)"
if [[ -n "$extra" ]]; then
	echo "grant-vocabulary: the grant loop walks capabilities the model does not declare:" >&2
	printf '  %s\n' $extra >&2
	failed=1
fi

# AND EACH ONE EXACTLY ONCE. A capability listed twice is sent twice, and a program reading its
# grants positionally takes the second copy under whatever tag it expected next.
duplicated="$(printf '%s\n' "$delivered" | uniq -d)"
if [[ -n "$duplicated" ]]; then
	echo "grant-vocabulary: the grant loop walks these more than once, so a positional reader takes the repeat under the next tag:" >&2
	printf '  %s\n' $duplicated >&2
	failed=1
fi

# AND THE DECLARED LENGTH IS THE REAL ONE, so the array cannot be extended without the count moving.
declared_len="$(grep -oE '^const VOCABULARY: \[Capability; [0-9]+\]' "$loop_file" | grep -oE '[0-9]+')"
actual_len="$(printf '%s\n' "$delivered" | wc -l)"
if [[ "$declared_len" != "$actual_len" ]]; then
	echo "grant-vocabulary: VOCABULARY says it holds $declared_len and holds $actual_len" >&2
	failed=1
fi

if ((failed != 0)); then
	exit 1
fi
echo "grant-vocabulary: all $actual_len declared capabilities are walked by the grant loop, exactly once each"
