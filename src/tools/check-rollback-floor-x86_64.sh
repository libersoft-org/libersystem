#!/usr/bin/env bash
# The monotonic boot floor, proved on ONE persistent OVMF variables image across a whole sequence.
#
# WHAT THIS GATE ADDS TO `secure-boot` AND `signed-boot`. Those two prove that the firmware verifies
# the loader and that the loader verifies its manifests. Neither says anything about AGE: a correctly
# signed old release is still correctly signed, and an attacker who can replace boot media can put
# one back. This gate builds the `rollback-enforcing` loader, signs it for the firmware with a signer
# of its own, provisions the floor with the ceremony tool, and then boots a sequence of releases at
# three security generations against the SAME variables store - accepting the equal one, refusing the
# older one from the original and a cloned disk, advancing on the newer one, and then taking the
# store apart one variable at a time to show that no deletion, tear, cross-slot copy or interrupted
# advance lowers the floor. The recovery purpose, the cross-use negatives, the mixed-generation set,
# the partial provisioning states, the pre-policy loader under the old and the rotated signer, and
# the current non-enforcing loader are each their own boot.
#
# THE INTERRUPTED ADVANCE IS CONSTRUCTED, NOT INJECTED. QEMU cannot cut power between two UEFI
# variable writes on demand; what an interruption leaves is the state `{A=N, B=N+1}`, which the
# ceremony tool writes directly. The host fixtures in `bootproto::rollback` are where a torn write,
# a refused write and a readback mismatch are driven through the same code the loader runs.
#
# x86_64 ONLY, deliberately - the one firmware this tree qualifies Secure Boot on. It BUILDS MEDIA
# (four `image.sh` runs) and leaves the tree's image at the default generation afterwards, so it is
# minutes of work and runs last, like every other gate that assembles media.
set -euo pipefail
# THE PHASE LOGS OUTLIVE THIS SCRIPT when a run is collecting evidence: copied into the run from the
# EXIT trap, before the directory is removed - on failure too, which is when they matter.
# shellcheck source=evidence.sh
source "$(dirname "${BASH_SOURCE[0]}")/evidence.sh"
export LIBER_RUN_MODE="${LIBER_RUN_MODE:-gate}"

cd "$(dirname "$0")/../.."
BUILD=".build/boot"
SECDIR=".build/secureboot"
OVMF_SECBOOT="${OVMF_SECBOOT:-/usr/share/OVMF/OVMF_CODE_4M.secboot.fd}"
OVMF_VARS_TEMPLATE="${OVMF_VARS_SRC:-/usr/share/OVMF/OVMF_VARS_4M.fd}"
# The two signer owners: the test platform key `secure-boot` already enrols (the PREVIOUS trust
# state, which signs the non-enforcing loaders) and the enforcing signer this gate introduces.
OLD_OWNER_GUID="6b7c9e4d-0f2a-4c31-9a5e-1d8f3b7c2a90"
NEW_OWNER_GUID="2f5a8c1e-6d3b-4e97-b1c4-7a0d9e5f3b62"
# The floor's namespace and names, as `bootproto::rollback` freezes them.
VENDOR_GUID="4c696265-7253-7973-2d52-6f6c6c626b31"
SLOT_A="LiberSystemRollbackA"
SLOT_B="LiberSystemRollbackB"
MARKER="LiberSystemRollbackProvisioned"
# The generations: the tree's default is 1, the installed release is 2, the update is 3.
OLD=1
CURRENT=2
NEXT=3

fail() {
	echo "rollback-floor: $*" >&2
	exit 1
}

missing=0
for tool in openssl sbsign sbverify virt-fw-vars qemu-system-x86_64 xorriso mcopy python3; do
	command -v "$tool" >/dev/null || {
		echo "rollback-floor: $tool is required by this gate and is not installed" >&2
		missing=1
	}
done
((missing == 0)) || fail "install the missing tools (setup.sh lists them)"
[[ -f "$OVMF_SECBOOT" ]] || fail "no Secure-Boot-capable OVMF at $OVMF_SECBOOT"
[[ -f "$OVMF_VARS_TEMPLATE" ]] || fail "no OVMF variable template at $OVMF_VARS_TEMPLATE"

work="$(mktemp -d)"
restore_needed=0
cleanup() {
	# THE TREE'S IMAGE IS LEFT AT THE DEFAULT GENERATION. The last medium this gate builds is the
	# generation-1 one, which is the tree's own; if the gate died before that, rebuild it now so the
	# next `./run.sh` does not boot a medium nobody asked for.
	if ((restore_needed == 1)); then
		./image.sh --format iso --dma-mode enforcing-required >/dev/null 2>&1 || echo "rollback-floor: could not restore the default image - run ./image.sh --format iso" >&2
	fi
	evidence_keep_gate "$work"/*.log
	rm -rf "$work"
}
trap cleanup EXIT

signer="$PWD/src/tools/sign-manifest"
sign() {
	(cd "$signer" && cargo run --quiet -- "$@")
}

# THE RECORDS AND THE MARKER, from the same codec the loader validates with.
record_hex() {
	local slot="$1" generation="$2"
	sign --rollback-record "$slot" --product LiberSystem --generation "$generation"
}
marker_hex="$(sign --rollback-marker)"
[[ "$marker_hex" == "01" ]] || fail "the marker byte is $marker_hex, not 01"
declare -A REC_A REC_B
for g in $OLD $CURRENT $NEXT; do
	REC_A[$g]="$(record_hex a "$g")"
	REC_B[$g]="$(record_hex b "$g")"
	[[ ${#REC_A[$g]} -eq 128 && ${#REC_B[$g]} -eq 128 ]] || fail "a record for generation $g is not sixty-four bytes"
done
echo "rollback-floor: the ceremony tool encodes sixty-four-byte records for generations $OLD, $CURRENT and $NEXT"

# THE STORE, READ AND WRITTEN FROM THE HOST. `store_state` prints what each variable holds against
# the records above: a generation, `absent`, or `other`. `set_vars` writes variables from name=hex
# pairs; `del_var` removes one.
store_json() {
	virt-fw-vars -i "$1" --output-json /dev/stdout 2>/dev/null
}
store_state() {
	local store="$1"
	python3 - "$store" "$VENDOR_GUID" "$SLOT_A" "$SLOT_B" "$MARKER" "$OLD:${REC_A[$OLD]}:${REC_B[$OLD]}" "$CURRENT:${REC_A[$CURRENT]}:${REC_B[$CURRENT]}" "$NEXT:${REC_A[$NEXT]}:${REC_B[$NEXT]}" <<'PY'
import json, subprocess, sys
store, guid, a_name, b_name, marker_name = sys.argv[1:6]
known = {}
for spec in sys.argv[6:]:
    gen, a, b = spec.split(":")
    known[("a", a)] = gen
    known[("b", b)] = gen
dump = json.loads(subprocess.run(["virt-fw-vars", "-i", store, "--output-json", "/dev/stdout"], capture_output=True, text=True, check=True).stdout)
held = {v["name"]: v for v in dump["variables"] if v["guid"].lower() == guid}
def slot(name, which):
    if name not in held:
        return "absent"
    v = held[name]
    if v["attr"] != 3:
        return "attr%d" % v["attr"]
    return known.get((which, v["data"].lower()), "other")
marker = "absent"
if marker_name in held:
    m = held[marker_name]
    marker = "present" if (m["attr"] == 3 and m["data"].lower() == "01") else "other"
print("A=%s B=%s marker=%s" % (slot(a_name, "a"), slot(b_name, "b"), marker))
PY
}
set_vars() {
	# set_vars STORE name=hex [name=hex ...]
	local store="$1"
	shift
	local json="$work/set.json"
	python3 - "$json" "$VENDOR_GUID" "$@" <<'PY'
import json, sys
out, guid = sys.argv[1], sys.argv[2]
variables = []
for pair in sys.argv[3:]:
    name, data = pair.split("=", 1)
    variables.append({"name": name, "guid": guid, "attr": 3, "data": data})
json.dump({"version": 2, "variables": variables}, open(out, "w"))
PY
	virt-fw-vars -i "$store" --set-json "$json" -o "$store.next" >/dev/null 2>&1 || fail "virt-fw-vars could not set variables in $store"
	mv "$store.next" "$store"
}
del_var() {
	virt-fw-vars -i "$1" -d "$2" -o "$1.next" >/dev/null 2>&1 || fail "virt-fw-vars could not delete $2 from $1"
	mv "$1.next" "$1"
}
expect_state() {
	local store="$1" want="$2" what="$3"
	local have
	have="$(store_state "$store")"
	[[ "$have" == "$want" ]] || fail "$what: the store holds '$have', expected '$want'"
}

# THE CEREMONY: slot A, read back; slot B, read back; only then the marker - the commit point. An
# interrupted ceremony is one of its prefixes, which `ceremony_until` produces on purpose.
ceremony_until() {
	# ceremony_until STORE GENERATION STEPS   (steps: 1 = slot A, 2 = both slots, 3 = complete)
	local store="$1" generation="$2" steps="$3"
	set_vars "$store" "$SLOT_A=${REC_A[$generation]}"
	[[ "$(store_state "$store")" == A=$generation* ]] || fail "ceremony: slot A did not read back at generation $generation"
	((steps >= 2)) || return 0
	set_vars "$store" "$SLOT_B=${REC_B[$generation]}"
	[[ "$(store_state "$store")" == "A=$generation B=$generation marker="* ]] || fail "ceremony: slot B did not read back at generation $generation"
	((steps >= 3)) || return 0
	set_vars "$store" "$MARKER=$marker_hex"
	expect_state "$store" "A=$generation B=$generation marker=present" "ceremony at generation $generation"
}
ceremony() {
	ceremony_until "$1" "$2" 3
}

# 1. THE TWO LOADERS. The enforcing one is built privately with its own profile; the non-enforcing
#    one is the tree's ordinary `test-trust` loader, copied under the producer lock.
echo "rollback-floor: building the rollback-enforcing loader"
src/tools/build-loader-private.sh rollback-enforcing "$work/enforcing.efi" || fail "the rollback-enforcing loader did not build"
grep -aq "ROLLBACK FLOOR ENFORCED" "$work/enforcing.efi" || fail "the enforcing loader does not carry its marker"
ordinary=".build/cargo/loader/x86_64-unknown-uefi/debug/libersystem-loader.efi"
[[ -f "$ordinary" ]] || fail "no ordinary loader at $ordinary - run ./build.sh --arch x86_64 --part loader"
mkdir -p .build/state
(
	flock 9
	cp "$ordinary" "$work/ordinary.efi"
) 9>.build/state/kernel-test-build.lock
if grep -aq "ROLLBACK FLOOR ENFORCED" "$work/ordinary.efi"; then
	fail "the tree's ordinary loader carries the enforcing marker - it must not keep a floor"
fi
echo "rollback-floor: two loaders - one that enforces the floor and one that does not"

# 2. THE TWO SIGNERS, generated once each and cached. The old one is `secure-boot`'s test platform
#    key; the new one signs enforcing loaders and nothing else.
mkdir -p "$SECDIR"
(
	flock 9
	if [[ ! -f "$SECDIR/test-pk.pem" || ! -f "$SECDIR/test-pk.key" ]]; then
		openssl req -new -x509 -newkey rsa:2048 -nodes -sha256 -days 3650 -subj "/CN=LiberSystem test platform key/" -keyout "$SECDIR/test-pk.key" -out "$SECDIR/test-pk.pem" >/dev/null 2>&1 || fail "could not generate the test platform key"
	fi
	if [[ ! -f "$SECDIR/rollback-pk.pem" || ! -f "$SECDIR/rollback-pk.key" ]]; then
		openssl req -new -x509 -newkey rsa:2048 -nodes -sha256 -days 3650 -subj "/CN=LiberSystem rollback-enforcing loader signer/" -keyout "$SECDIR/rollback-pk.key" -out "$SECDIR/rollback-pk.pem" >/dev/null 2>&1 || fail "could not generate the enforcing signer"
		echo "rollback-floor: generated the enforcing loader signer in $SECDIR"
	fi
) 9>"$SECDIR/key.lock"
sbsign --key "$SECDIR/rollback-pk.key" --cert "$SECDIR/rollback-pk.pem" --output "$work/enforcing-signed.efi" "$work/enforcing.efi" >/dev/null 2>&1 || fail "sbsign could not sign the enforcing loader"
sbverify --cert "$SECDIR/rollback-pk.pem" "$work/enforcing-signed.efi" >/dev/null 2>&1 || fail "the enforcing loader's signature does not verify"
sbsign --key "$SECDIR/test-pk.key" --cert "$SECDIR/test-pk.pem" --output "$work/ordinary-old-signed.efi" "$work/ordinary.efi" >/dev/null 2>&1 || fail "sbsign could not sign the ordinary loader with the old signer"
# THE MUTATION THE SIGNER SEPARATION EXISTS FOR: the loader WITHOUT the comparison, signed by the
# enforcing signer. It is what a loader mutated to skip the floor would be, and the last boot below
# shows the assertions of this gate are what it would fail.
sbsign --key "$SECDIR/rollback-pk.key" --cert "$SECDIR/rollback-pk.pem" --output "$work/ordinary-new-signed.efi" "$work/ordinary.efi" >/dev/null 2>&1 || fail "sbsign could not sign the ordinary loader with the enforcing signer"
echo "rollback-floor: the enforcing loader is signed by the enforcing signer, the ordinary one by the previous signer"

# 3. THE VARIABLE STORES. `old.fd` is the previous trust state: the old signer enrolled, nothing
#    else. `new.fd` is the rotated one: the enforcing signer enrolled as PK, KEK and db, and the old
#    signer's certificate in dbx - revoked, so a loader it signed is refused whatever it says.
old_store="$work/old.fd"
virt-fw-vars --input "$OVMF_VARS_TEMPLATE" --output "$old_store" --set-pk "$OLD_OWNER_GUID" "$SECDIR/test-pk.pem" --add-kek "$OLD_OWNER_GUID" "$SECDIR/test-pk.pem" --add-db "$OLD_OWNER_GUID" "$SECDIR/test-pk.pem" --secure-boot >/dev/null 2>&1 || fail "virt-fw-vars could not enrol the old signer"
new_store="$work/new.fd"
virt-fw-vars --input "$OVMF_VARS_TEMPLATE" --output "$new_store" --set-pk "$NEW_OWNER_GUID" "$SECDIR/rollback-pk.pem" --add-kek "$NEW_OWNER_GUID" "$SECDIR/rollback-pk.pem" --add-db "$NEW_OWNER_GUID" "$SECDIR/rollback-pk.pem" --secure-boot >/dev/null 2>&1 || fail "virt-fw-vars could not enrol the enforcing signer"
openssl x509 -in "$SECDIR/test-pk.pem" -outform DER -out "$work/old-signer.der" >/dev/null 2>&1 || fail "could not convert the old signer's certificate"
python3 - "$work/old-signer.der" "$OLD_OWNER_GUID" "$work/dbx.json" <<'PY'
import json, struct, sys, uuid
cert = open(sys.argv[1], "rb").read()
owner = uuid.UUID(sys.argv[2])
x509 = uuid.UUID("a5c059a1-94e4-4aa7-87b5-ab155c2bf072")
signature = owner.bytes_le + cert
esl = x509.bytes_le + struct.pack("<III", 28 + len(signature), 0, len(signature)) + signature
json.dump({"version": 2, "variables": [{"name": "dbx", "guid": "d719b2cb-3d3a-4596-a3bc-dad00e67656f", "attr": 39, "data": esl.hex()}]}, open(sys.argv[3], "w"))
PY
virt-fw-vars -i "$new_store" --set-json "$work/dbx.json" -o "$new_store.next" >/dev/null 2>&1 || fail "virt-fw-vars could not write dbx"
mv "$new_store.next" "$new_store"
rotated_json="$(store_json "$new_store")"
grep -q '"name": "dbx"' <<<"$rotated_json" || fail "the rotated store carries no dbx"
echo "rollback-floor: two stores - the previous trust state, and the rotated one with the old signer revoked"

# 4. THE MEDIA, one per generation and purpose. Each is a full `image.sh` run, because the volume's
#    own manifest carries the generation too and LiberFS checksums every block - so a generation
#    cannot be spliced into an existing image. Built newest first and the tree's default LAST, which
#    leaves `.build/boot` as the tree expects it.
restore_needed=1
build_medium() {
	local generation="$1" purpose="$2" out="$3"
	echo "rollback-floor: building the generation-$generation $purpose medium"
	LIBER_SECURITY_GENERATION="$generation" LIBER_MANIFEST_PURPOSE="$purpose" ./image.sh --format iso --dma-mode enforcing-required >"$work/image-$generation-$purpose.log" 2>&1 || {
		tail -20 "$work/image-$generation-$purpose.log" >&2
		fail "image.sh could not build the generation-$generation $purpose medium"
	}
	xorriso -osirrox on -indev "$BUILD/libersystem.iso" -extract /boot/efiboot.img "$out" >/dev/null 2>&1 || fail "could not read the ESP out of the generation-$generation medium"
	chmod u+w "$out"
	mcopy -i "$out" ::/etc/boot.manifest2 "$out.manifest2" 2>/dev/null || fail "the generation-$generation medium carries no signed manifest"
	local inspected
	inspected="$(sign --inspect "$out.manifest2")"
	grep -q "^generation: $generation$" <<<"$inspected" || fail "the medium built for generation $generation says: $(grep generation <<<"$inspected")"
	grep -q "^purpose: $purpose$" <<<"$inspected" || fail "the medium built for purpose $purpose says: $(grep purpose <<<"$inspected")"
}
build_medium $NEXT boot "$work/esp-$NEXT.img"
build_medium $CURRENT recovery "$work/esp-$CURRENT-recovery.img"
build_medium $CURRENT boot "$work/esp-$CURRENT.img"
build_medium $OLD boot "$work/esp-$OLD.img"
restore_needed=0
echo "rollback-floor: four media - generations $OLD, $CURRENT and $NEXT, and a recovery set at $CURRENT"

# A medium with a given loader in it.
medium_with() {
	local esp="$1" loader="$2" out="$3"
	cp "$esp" "$out"
	mcopy -o -i "$out" "$loader" ::/EFI/BOOT/BOOTX64.EFI
}
# The medium's manifest re-signed with one thing changed, over the medium's own kernel and live
# volume - right about the content, wrong about the context, as `signed-boot` does it.
resign_esp() {
	# resign_esp ESP OUT-MANIFEST [signer args...]
	local esp="$1" out="$2"
	shift 2
	mcopy -i "$esp" ::/kernel "$work/kernel.bin" 2>/dev/null || return 1
	mcopy -i "$esp" ::/system-volume.img "$work/live.img" 2>/dev/null || return 1
	local paired
	paired="$(sign --inspect "$esp.manifest2" | grep '^volume-uuid:' | cut -d' ' -f2)"
	local release
	release="$(sign --inspect "$esp.manifest2" | grep '^release:' | cut -d' ' -f2)"
	sign --profile test-trust --product LiberSystem --arch x86_64 --source boot-medium --release "$release" --volume-uuid "$paired" --dma-mode enforcing-required \
		--row "kernel:kernel=$work/kernel.bin" --row "system-volume:system-volume.img=$work/live.img" "$@" --out "$out" >/dev/null 2>&1
}

boot() {
	# boot MEDIUM STORE VERDICT LOG   - the store is used IN PLACE: what a boot writes persists.
	local medium="$1" store="$2" verdict="$3" log="$4"
	local accel=()
	[[ -w /dev/kvm ]] && accel=(-enable-kvm -cpu host)
	src/tools/guest-verdict.py "$verdict" "$log" -- qemu-system-x86_64 \
		"${accel[@]}" -machine q35,smm=on -m 2G -display none -no-reboot \
		-global driver=cfi.pflash01,property=secure,value=on \
		-drive "if=pflash,format=raw,unit=0,readonly=on,file=$OVMF_SECBOOT" \
		-drive "if=pflash,format=raw,unit=1,file=$store" \
		-drive "format=raw,file=$medium" \
		-serial "file:$log"
}
boot_or_fail() {
	# boot_or_fail MEDIUM STORE VERDICT WHAT [line the log must carry]
	local medium="$1" store="$2" verdict="$3" what="$4" line="${5:-}"
	local log="$work/boot-$RANDOM.log"
	boot "$medium" "$store" "$verdict" "$log" || {
		echo "rollback-floor: $what - the boot did not reach the verdict '$verdict'" >&2
		grep -a "loader:" "$log" | sed -n '1,20p' >&2 || true
		exit 1
	}
	if [[ -n "$line" ]] && ! grep -aq -- "$line" "$log"; then
		echo "rollback-floor: $what - the log does not carry '$line'" >&2
		grep -a "loader:" "$log" | sed -n '1,20p' >&2 || true
		exit 1
	fi
}

for g in $OLD $CURRENT $NEXT; do
	medium_with "$work/esp-$g.img" "$work/enforcing-signed.efi" "$work/boot-$g.img"
done
medium_with "$work/esp-$CURRENT-recovery.img" "$work/enforcing-signed.efi" "$work/recovery-$CURRENT.img"

# 5. THE SEQUENCE, on one persistent store provisioned at the installed generation.
store="$work/persistent.fd"
cp "$new_store" "$store"
ceremony "$store" $CURRENT
echo "rollback-floor: provisioned the store at generation $CURRENT - slot A, slot B, then the marker"

boot_or_fail "$work/boot-$CURRENT.img" "$store" rollback-accepted "generation $CURRENT on a floor of $CURRENT" "loader: rollback floor $CURRENT - generation $CURRENT accepted, equal to the floor of $CURRENT"
expect_state "$store" "A=$CURRENT B=$CURRENT marker=present" "after the equal boot"
echo "rollback-floor: generation $CURRENT boots on a floor of $CURRENT and writes nothing"

boot_or_fail "$work/boot-$OLD.img" "$store" rollback-refused "generation $OLD below a floor of $CURRENT" "loader: FATAL - rollback floor $CURRENT REFUSES generation $OLD"
cp "$work/boot-$OLD.img" "$work/boot-$OLD-clone.img"
boot_or_fail "$work/boot-$OLD-clone.img" "$store" rollback-refused "generation $OLD from a cloned disk" "REFUSES generation $OLD"
expect_state "$store" "A=$CURRENT B=$CURRENT marker=present" "after the refused boots"
echo "rollback-floor: generation $OLD is refused from the original and from a cloned disk, and the floor stands"

boot_or_fail "$work/recovery-$CURRENT.img" "$store" rollback-accepted "the recovery set at generation $CURRENT" "loader: rollback floor $CURRENT - generation $CURRENT accepted"
expect_state "$store" "A=$CURRENT B=$CURRENT marker=present" "after the recovery boot"
echo "rollback-floor: a recovery set at the floor boots, and cannot clear or lower the state"

boot_or_fail "$work/boot-$NEXT.img" "$store" rollback-accepted "generation $NEXT advancing the floor" "loader: rollback floor $NEXT - generation $NEXT accepted, floor advanced from $CURRENT, slot A written and read back, slot B written and read back"
expect_state "$store" "A=$NEXT B=$NEXT marker=present" "after the advance"
boot_or_fail "$work/boot-$NEXT.img" "$store" rollback-accepted "generation $NEXT again" "accepted, equal to the floor of $NEXT"
expect_state "$store" "A=$NEXT B=$NEXT marker=present" "after the second boot of $NEXT"
echo "rollback-floor: generation $NEXT advances the floor to $NEXT, both slots written and read back, and boots again on it"

boot_or_fail "$work/boot-$CURRENT.img" "$store" rollback-refused "generation $CURRENT below the advanced floor" "REFUSES generation $CURRENT"
boot_or_fail "$work/recovery-$CURRENT.img" "$store" rollback-refused "the recovery set below the advanced floor" "REFUSES generation $CURRENT"
echo "rollback-floor: below the advanced floor, both the ordinary and the recovery set are refused"

# 6. DELETING EITHER SLOT AFTER THE ADVANCE. The survivor is authoritative, the old generation stays
#    refused, and the next accepted boot repairs the missing slot BEFORE control is transferred.
for victim in "$SLOT_A" "$SLOT_B"; do
	del_var "$store" "$victim"
	boot_or_fail "$work/boot-$CURRENT.img" "$store" rollback-refused "generation $CURRENT with $victim deleted" "REFUSES generation $CURRENT"
	boot_or_fail "$work/boot-$NEXT.img" "$store" rollback-accepted "generation $NEXT repairing $victim" "written and read back"
	expect_state "$store" "A=$NEXT B=$NEXT marker=present" "after repairing $victim"
done
echo "rollback-floor: with either slot deleted the old generation is still refused, and the equal boot restores the slot"

# 7. THE INTERRUPTED ADVANCE: power lost between the two writes leaves {A=$CURRENT, B=$NEXT}. The
#    equal boot converges A first; then deleting B - the slot that carried the advance - must still
#    leave $CURRENT refused.
set_vars "$store" "$SLOT_A=${REC_A[$CURRENT]}" "$SLOT_B=${REC_B[$NEXT]}"
expect_state "$store" "A=$CURRENT B=$NEXT marker=present" "the constructed interrupted state"
boot_or_fail "$work/boot-$NEXT.img" "$store" rollback-accepted "generation $NEXT on the interrupted state" "accepted, equal to the floor of $NEXT, slot A written and read back"
expect_state "$store" "A=$NEXT B=$NEXT marker=present" "after converging the interrupted advance"
del_var "$store" "$SLOT_B"
boot_or_fail "$work/boot-$CURRENT.img" "$store" rollback-refused "generation $CURRENT after the interrupted advance lost its carrying slot" "REFUSES generation $CURRENT"
boot_or_fail "$work/boot-$NEXT.img" "$store" rollback-accepted "generation $NEXT repairing slot B" "slot B written and read back"
expect_state "$store" "A=$NEXT B=$NEXT marker=present" "after the interrupted sequence"
echo "rollback-floor: an interrupted advance is completed by the equal boot, so losing the slot that carried it lowers nothing"

# 8. DAMAGED AND SUBSTITUTED RECORDS. A torn slot is repaired from the survivor; slot A's bytes in
#    slot B are not a lower floor but an invalid slot; both damaged refuses and is not reset - the
#    ceremony recovers it.
torn="${REC_A[$NEXT]:0:120}00000000"
set_vars "$store" "$SLOT_A=$torn"
expect_state "$store" "A=other B=$NEXT marker=present" "the torn slot"
boot_or_fail "$work/boot-$NEXT.img" "$store" rollback-accepted "a torn slot A" "slot A written and read back"
expect_state "$store" "A=$NEXT B=$NEXT marker=present" "after repairing the torn slot"
set_vars "$store" "$SLOT_B=${REC_A[$OLD]}"
boot_or_fail "$work/boot-$CURRENT.img" "$store" rollback-refused "slot A's old record copied into slot B" "REFUSES generation $CURRENT"
boot_or_fail "$work/boot-$NEXT.img" "$store" rollback-accepted "repairing the cross-slot copy" "slot B written and read back"
expect_state "$store" "A=$NEXT B=$NEXT marker=present" "after repairing the cross-slot copy"
set_vars "$store" "$SLOT_A=$torn" "$SLOT_B=${REC_A[$OLD]}"
boot_or_fail "$work/boot-$NEXT.img" "$store" rollback-refused "both slots damaged" "neither slot holds a valid record"
expect_state "$store" "A=other B=other marker=present" "a provisioned machine with no believable state is not reset by a boot"
ceremony "$store" $NEXT
boot_or_fail "$work/boot-$NEXT.img" "$store" rollback-accepted "after the ceremony recovered the state" "accepted, equal to the floor of $NEXT"
echo "rollback-floor: a damaged or substituted slot never lowers the floor, two damaged slots refuse, and the ceremony recovers them"

# 9. THE MANIFEST NEGATIVES, all on the store at floor $NEXT and none moving it: cross-use in both
#    directions, a mixed-purpose set, and a mixed-generation set.
negative() {
	local what="$1" line="$2"
	shift 2
	resign_esp "$work/esp-$NEXT.img" "$work/negative.manifest2" "$@" || fail "could not sign the $what case"
	cp "$work/boot-$NEXT.img" "$work/negative.img"
	mcopy -o -i "$work/negative.img" "$work/negative.manifest2" ::/etc/boot.manifest2
	boot_or_fail "$work/negative.img" "$store" rollback-manifest-refused "$what" "$line"
	expect_state "$store" "A=$NEXT B=$NEXT marker=present" "after the $what case"
}
negative "an ordinary set signed with the recovery key" "purpose is not one its key may sign for" --generation $NEXT --purpose boot --signing-key recovery
negative "a recovery set signed with the boot key" "purpose is not one its key may sign for" --generation $NEXT --purpose recovery --signing-key boot
negative "a recovery medium over an ordinary volume" "different purpose" --generation $NEXT --purpose recovery --signing-key recovery
negative "a medium at generation $((NEXT + 1)) over a volume at $NEXT" "two generations" --generation $((NEXT + 1)) --purpose boot --signing-key boot
echo "rollback-floor: the cross-use, mixed-purpose and mixed-generation sets are refused without advancing the floor"

# 10. THE PARTIAL PROVISIONING STATES, each booted: an interrupted ceremony is unprovisioned, boots,
#     and refuses to advance; re-running it from step one provisions; a deleted marker costs the
#     floor's availability and not its integrity.
fresh="$work/fresh.fd"
for steps in 1 2; do
	cp "$new_store" "$fresh"
	ceremony_until "$fresh" $CURRENT "$steps"
	before="$(store_state "$fresh")"
	boot_or_fail "$work/boot-$NEXT.img" "$fresh" rollback-unprovisioned "a ceremony interrupted after step $steps" "UNPROVISIONED"
	expect_state "$fresh" "$before" "the unprovisioned boot after step $steps changed the store"
	ceremony "$fresh" $CURRENT
	boot_or_fail "$work/boot-$OLD.img" "$fresh" rollback-refused "generation $OLD after the re-run ceremony" "REFUSES generation $OLD"
done
cp "$new_store" "$fresh"
boot_or_fail "$work/boot-$NEXT.img" "$fresh" rollback-unprovisioned "a never-provisioned machine" "UNPROVISIONED"
expect_state "$fresh" "A=absent B=absent marker=absent" "a never-provisioned machine after a boot"
del_var "$store" "$MARKER"
boot_or_fail "$work/boot-$OLD.img" "$store" rollback-unprovisioned "the marker deleted from a provisioned machine" "the floor is NOT advanced"
expect_state "$store" "A=$NEXT B=$NEXT marker=absent" "after the marker was deleted"
ceremony "$store" $NEXT
boot_or_fail "$work/boot-$OLD.img" "$store" rollback-refused "generation $OLD once the ceremony re-ran" "REFUSES generation $OLD"
echo "rollback-floor: every partial provisioning state boots as unprovisioned and advances nothing; the ceremony recovers each"

# 11. THE LOADERS AGAINST THE SIGNERS. The pre-policy loader - the ordinary one, signed by the old
#     signer - is accepted under the old store and refused under the rotated one; it is also the
#     CURRENT, correctly signed, NON-ENFORCING loader, and the rotated store refuses it for the same
#     reason. The enforcing loader is accepted only under the rotated store.
old_copy="$work/old-fresh.fd"
cp "$old_store" "$old_copy"
medium_with "$work/esp-$OLD.img" "$work/ordinary-old-signed.efi" "$work/prepolicy.img"
boot_or_fail "$work/prepolicy.img" "$old_copy" rollback-not-enforced "the pre-policy loader under the previous trust state" "loader: rollback floor - not enforced by this build"
echo "rollback-floor: the pre-policy loader is accepted under the previous variables image, and keeps no floor"
log="$work/prepolicy-rotated.log"
boot "$work/prepolicy.img" "$store" secure-unsigned "$log" || fail "the rotated store's boot of the pre-policy loader did not run to its verdict"
if grep -aq "loader: TEST TRUST\|loader: release trust" "$log"; then
	fail "the pre-policy loader RAN under the rotated store - the old signer was not revoked"
fi
echo "rollback-floor: the same loader - correctly signed, non-enforcing - is refused by the rotated store"
cp "$new_store" "$fresh"
medium_with "$work/esp-$OLD.img" "$work/enforcing.efi" "$work/enforcing-unsigned.img"
log="$work/enforcing-unsigned.log"
boot "$work/enforcing-unsigned.img" "$fresh" secure-unsigned "$log" || fail "the unsigned enforcing loader's boot did not run to its verdict"
if grep -aq "loader: TEST TRUST\|loader: release trust" "$log"; then
	fail "an UNSIGNED enforcing loader ran under the rotated store"
fi
echo "rollback-floor: an unsigned enforcing loader does not run either"

# 12. WHAT THE ASSERTIONS ABOVE WOULD MISS WITHOUT THE SIGNER SEPARATION: the loader without the
#     comparison, signed by the enforcing signer, boots generation $OLD straight past a floor of
#     $NEXT. That is the mutation, and this gate's refusal rows are what it fails.
medium_with "$work/esp-$OLD.img" "$work/ordinary-new-signed.efi" "$work/mutant.img"
boot_or_fail "$work/mutant.img" "$store" rollback-not-enforced "a loader without the comparison, signed by the enforcing signer" "loader: rollback floor - not enforced by this build"
expect_state "$store" "A=$NEXT B=$NEXT marker=present" "the mutant loader"
echo "rollback-floor: a loader that skips the comparison boots the old generation - which is the failure the enforcing signer keeps off enforcing machines"

echo "rollback-floor: one persistent store, generations $OLD/$CURRENT/$NEXT - nothing on replaceable media lowers the floor"
