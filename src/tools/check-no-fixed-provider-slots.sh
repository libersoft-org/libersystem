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
# AND THE BOOT HAND-OFF CARRIES NO COUNT OF ITS OWN (2026-09-02).
#
# This used to REQUIRE `BOOT_BLOCK_TAGS`, on the reasoning that one named list is better than four
# variables and a `send` each. It is - and it was still a four compiled into the manager: `BLOCK`,
# `BLOCK2`, `BLOCK3`, `BLOCK4`, with a four-entry array behind two of them, so a fifth disk had
# nowhere to go however many the registry allowed a driver to publish. The Definition of Done says
# the number of providers of a kind is bounded by what the registry allows and by NOTHING COMPILED
# INTO THE MANAGER, and a list of tags is a number compiled into the manager.
#
# So the rule is inverted: the tag list must be GONE, and the count must TRAVEL. `BLOCKS` is the
# message that carries it, and the arrays behind it are grown rather than declared.
if offenders="$(printf '%s\n' "$code" | grep -nE 'BLOCK2|BLOCK3|BLOCK4|BOOT_BLOCK_TAGS')"; then
	echo "provider-slots: DeviceManager names numbered block tags again:" >&2
	printf '%s\n' "$offenders" >&2
	echo "    A tag per disk is a count of disks compiled into the manager. The hand-off says how" >&2
	echo "    many follow - see the 'BLOCKS' message - and the reader takes exactly that many." >&2
	fail=1
fi
if ! printf '%s\n' "$code" | grep -q "b'B', b'L', b'O', b'C', b'K', b'S'"; then
	echo "provider-slots: the boot hand-off no longer carries how many block providers follow" >&2
	echo "    Without the count the reader is back to assuming one, which is the fixed slot again." >&2
	fail=1
fi

((fail == 0)) || exit 1
slots="$(printf '%s\n' "$code" | grep -c 'catalogue.take(' || true)"
echo "provider-slots: no numbered provider local or block tag in DeviceManager, the hand-off carries its own count, and every route goes through the catalogue ($slots take site(s))"
