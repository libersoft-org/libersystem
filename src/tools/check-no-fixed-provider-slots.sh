#!/usr/bin/env bash
# NO FIXED PROVIDER SLOT IN DEVICEMANAGER, checked against the source rather than remembered.
#
# `block_client`, `block2_client`, `block3_client` and `block4_client` were four variables, each
# routed by hand to the service that owns that kind. So a second disk DID have somewhere to go and a
# fifth did not, and which volume was which depended on which driver finished first.
#
# THE DEFECT IS A COUNT COMPILED INTO THE MANAGER, not the existence of a count: the boot hand-off
# carries four block tags and that is a property of the WIRE, named in one list. What this refuses is
# a numbered per-provider variable coming back - which is the shape the defect had, and the shape it
# would come back in.
#
# Comments may name them, because the file explains what it stopped doing.

# `src/`, the way every other gate in this directory finds it.
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

MANAGER="$root/user/services/core/src/device_manager.rs"
if [[ ! -f "$MANAGER" ]]; then
	echo "provider-slots: $MANAGER is missing" >&2
	exit 1
fi

# Code only: everything from the first non-blank, non-comment column onward. A `//` line is prose.
code="$(grep -vE '^[[:space:]]*//' "$MANAGER")"

fail=0
# A numbered provider local - `block2_client`, `net3_client`, `snd2` - is the shape that was removed.
if offenders="$(printf '%s\n' "$code" | grep -nE '\b(let|&mut)[[:space:]]+(mut[[:space:]]+)?[a-z_]+[0-9]+_client\b')"; then
	echo "provider-slots: a numbered per-provider local is back in DeviceManager:" >&2
	printf '%s\n' "$offenders" >&2
	echo "    That is the fixed-slot defect: a second of a kind has somewhere to go and the next one" >&2
	echo "    does not, and which is which depends on arrival order. Route from the catalogue." >&2
	fail=1
fi

# The catalogue is what holds providers, and its bound is not the wire's.
if ! printf '%s\n' "$code" | grep -q 'MAX_PROVIDERS'; then
	echo "provider-slots: DeviceManager no longer names MAX_PROVIDERS - the catalogue is what bounds providers" >&2
	fail=1
fi
# AND THE NUMBER IS THE REGISTRY'S, NOT THIS FILE'S.
#
# Requiring the symbol was all this checked, so `const MAX_PROVIDERS: usize = 32;` written here
# satisfied it - which is the same fixed-slot defect the numbered locals were, with a larger
# constant, and the definition of done says the count is bounded by what drivers DECLARE and by
# nothing compiled into DeviceManager. A definition in this file is that constant coming back.
if offenders="$(printf '%s\n' "$code" | grep -nE '^[[:space:]]*const[[:space:]]+MAX_PROVIDERS')"; then
	echo "provider-slots: DeviceManager DEFINES MAX_PROVIDERS:" >&2
	printf '%s\n' "$offenders" >&2
	echo "    The bound is the sum of every 'provides' declaration in the registry, emitted by" >&2
	echo "    build.rs beside the registry itself. A number written here is one the manifest cannot" >&2
	echo "    move: an image declaring more loses a publication, one declaring fewer carries slots" >&2
	echo "    it can never fill, and neither is something a reader of the manifest would expect." >&2
	fail=1
fi
if ! grep -q 'MAX_PROVIDERS' "$(dirname "$0")/../user/services/core/build.rs"; then
	echo "provider-slots: build.rs no longer emits MAX_PROVIDERS - nothing derives the bound from the registry" >&2
	fail=1
fi
if ! printf '%s\n' "$code" | grep -q 'BOOT_BLOCK_TAGS'; then
	echo "provider-slots: the boot hand-off's block tags are no longer one named list" >&2
	fail=1
fi

((fail == 0)) || exit 1
slots="$(printf '%s\n' "$code" | grep -c 'catalogue.take(' || true)"
echo "provider-slots: no numbered provider local in DeviceManager; every route goes through the catalogue ($slots take site(s))"
