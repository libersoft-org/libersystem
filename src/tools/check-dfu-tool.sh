#!/bin/bash
# THE SHIPPING FIRMWARE REQUESTER, cold, in a development image: `dfu` asks for an image to be written to a USB
# DFU target, AdminService puts the frozen operation on the protected screen, one key through QEMU's keyboard
# confirms it - and the USB DFU executor follows the runtime target into DFU mode and writes the image once.
# A second request, declined, writes nothing; and a backup asked of that target, which declares no upload, is refused
# before the protected screen is shown.
#
# AND THE BACKUP, on a second boot (`dfu-backup.toml`) with an upload-capable target: declined, it reads nothing and the
# runtime target is not even detached; confirmed, the executor follows the target into DFU mode and reads its firmware
# out, and the file on this run's stick is the target's image byte for byte, with the SHA-256 the tool and AdminService's
# record state; asked with a bound shorter than the image, the upload is stopped at the block past it and no file is
# written. The tool writes through a transactional writer, which publishes nothing it was not told to commit.
#
# WHAT IT RUNS. The target is `usbredir_device.py`'s runtime DFU device over QEMU's `usb-redir`: it waits, after
# DFU_DETACH, for the host's reset and comes back from it in DFU mode as product 0105 with the same serial. The
# images are on this run's USB stick (`USB_STICK_EXTRA`, which puts files on the run's copy and never on the
# shared fixture). `lab.sh scenario-cold` builds the development image and drives `dfu-tool.toml` through the
# emulated keyboard; this script then checks what the scenario cannot see: what the DEVICE received.
#
# WHAT IT DOES NOT CLAIM: physical approval on another architecture, or a device that needs a bootloader quirk.

set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
repo="$(cd "$root/.." && pwd)"
fail() {
	echo "qemu-dfu-tool: $*" >&2
	exit 1
}

case "${1:-}" in
"" | --arch)
	[[ "${2:-x86_64}" == x86_64 ]] || fail "this gate is the x86_64 one"
	;;
*) fail "unexpected argument '$1'" ;;
esac

scenario="$root/harness/scenarios/dfu-tool.toml"
backup_scenario="$root/harness/scenarios/dfu-backup.toml"
log="$repo/.build/boot/cold-x86_64.log"
work="$(mktemp -d "${TMPDIR:-/tmp}/liber-dfu-tool.XXXXXX")"
device_pid=""
cleanup() {
	if [[ -n "$device_pid" ]]; then
		kill "$device_pid" 2>/dev/null || true
		wait "$device_pid" 2>/dev/null || true
	fi
	[[ "$work" == */liber-dfu-tool.* ]] && rm -rf -- "$work"
}
trap cleanup EXIT

# THE IMAGES: the one the target verifies, with a DFU 1.1 suffix naming its runtime product, and the same image
# named for the DFU-mode product it becomes - the formula is the one the kernel's DFU oracles share.
python3 - "$work" <<'EOF' || fail "the images could not be written"
import struct
import sys


def firmware(seed, length):
    return bytes((n * 7 + seed * 13 + (n >> 8)) & 0xFF for n in range(length))


def suffixed(image, product):
    out = bytearray(image) + b"\xff\xff" + struct.pack("<HH", product, 0x1D6B) + b"\x00\x01UFD" + bytes([16])
    crc = 0xFFFFFFFF
    for byte in out:
        crc ^= byte
        for _ in range(8):
            crc = (crc >> 1) ^ 0xEDB88320 if crc & 1 else crc >> 1
    return bytes(out) + struct.pack("<I", crc)


image = firmware(5, 3000)
with open(f"{sys.argv[1]}/fw.dfu", "wb") as handle:
    handle.write(suffixed(image, 0x0104))
with open(f"{sys.argv[1]}/fw2.dfu", "wb") as handle:
    handle.write(suffixed(image, 0x0105))
EOF

# THE TARGET, listening before QEMU starts, because QEMU's chardev connects once.
python3 "$root/harness/usbredir_device.py" --emulate dfu-reset --socket "$work/device.sock" --ready "$work/ready" 2>"$work/device.log" &
device_pid="$!"
for _ in $(seq 1 100); do
	[[ -e "$work/ready" ]] && break
	sleep 0.05
done
[[ -e "$work/ready" ]] || fail "the DFU target did not start"

export USB_REDIR_SOCKET="$work/device.sock"
export USB_STICK_EXTRA="$work/fw.dfu $work/fw2.dfu"
"$repo/lab.sh" scenario-cold x86_64 "$scenario" || fail "the scenario failed (serial log: $log; target log: $work/device.log)"
[[ -f "$log" ]] || fail "the scenario left no serial log at $log"

if grep -aE 'dfu: (refused|failed|the end was not observed|the request went unanswered|not started)' "$log" >&2; then
	fail "the requester reported something other than a completion and a decline"
fi
grep -aqF "came back in DFU mode as" "$log" || fail "the executor never followed the target into DFU mode"

# WHAT THE DEVICE RECEIVED: one detach, one image, the expected one, manifested once - and nothing for the decline.
detaches="$(grep -ac 'DFU_DETACH' "$work/device.log" || true)"
images="$(grep -ac 'the expected image - manifesting' "$work/device.log" || true)"
others="$(grep -ac 'NOT the expected image' "$work/device.log" || true)"
[[ "$detaches" == 1 ]] || fail "the target was detached $detaches times; one confirmed request detaches it once"
[[ "$images" == 1 && "$others" == 0 ]] || fail "the target received $images expected image(s) and $others other(s); one confirmation writes one"
echo "qemu-dfu-tool: the target was detached once and received the confirmed image once; the declined request wrote nothing"

# THE BACKUP OF A TARGET THAT DECLARES NO UPLOAD: refused before the protected screen was shown.
python3 - "$log" <<'EOF' || fail "a backup of a target that declares no upload must be declined before the protected screen is shown"
import sys
text = open(sys.argv[1], 'rb').read()
asked = text.find(b'into vol://usb/nope.bin - confirm it on the protected screen')
declined = text.find(b'dfu: declined - nothing was read', asked)
if not 0 <= asked < declined:
    raise SystemExit(f'asked at {asked}, declined at {declined}')
if b'the protected screen is presented' in text[asked:declined]:
    raise SystemExit('the screen was shown for it')
EOF
echo "qemu-dfu-tool: a backup of the target, which declares no upload, was declined before the protected screen was shown"

# ------------------------------------------------------------------ the backup

kill "$device_pid" 2>/dev/null || true
wait "$device_pid" 2>/dev/null || true
rm -f "$work/ready" "$work/device.sock"
python3 "$root/harness/usbredir_device.py" --emulate dfu-upload --socket "$work/device.sock" --ready "$work/ready" 2>"$work/upload-device.log" &
device_pid="$!"
for _ in $(seq 1 100); do
	[[ -e "$work/ready" ]] && break
	sleep 0.05
done
[[ -e "$work/ready" ]] || fail "the upload-capable DFU target did not start"
unset USB_STICK_EXTRA
# THIS RUN'S STICK is the private copy made for it - the one that appears while it runs.
newest_stick() {
	python3 -c 'import glob, os, sys
found = sorted(glob.glob(sys.argv[1]), key=os.path.getmtime)
print(found[-1] if found else "")' "$repo/.build/boot/usb-media*.img"
}
before_stick="$(newest_stick)"
"$repo/lab.sh" scenario-cold x86_64 "$backup_scenario" || fail "the backup scenario failed (serial log: $log; target log: $work/upload-device.log)"
stick="$(newest_stick)"
[[ -n "$stick" && "$stick" != "$before_stick" ]] || fail "the backup run's own copy of the USB stick was not found"
mdir -b -i "$stick" :: >"$work/stick.list" 2>&1 || fail "the run's stick could not be listed (see $work/stick.list)"
mcopy -n -i "$stick" ::backup.bin "$work/backup.bin" || fail "the backup is not on the run's stick"
python3 - "$work/backup.bin" "$log" <<'EOF' || fail "the backup is not the target's image"
import hashlib, re, sys
def firmware(seed, length):
    return bytes((n * 7 + seed * 13 + (n >> 8)) & 0xFF for n in range(length))
got = open(sys.argv[1], 'rb').read()
want = firmware(3, 2500)
if got != want:
    raise SystemExit(f'{len(got)} bytes, not the 2500 the target holds')
digest = hashlib.sha256(want).hexdigest()
text = open(sys.argv[2], 'rb').read().decode(errors='replace')
said = re.findall(r'dfu: completed - 2500 bytes, SHA-256 ([0-9a-f]{64}), written to vol://usb/backup\.bin', text)
recorded = re.findall(r'AdminService: request \d+ completed: read 2500 bytes, SHA-256 ([0-9a-f]{64})', text)
if said != [digest] or recorded != [digest]:
    raise SystemExit(f'the tool said {said} and the record {recorded}, the image is {digest}')
EOF
for absent in DECLINED.BIN SHORT.BIN; do
	if grep -qi "$absent" "$work/stick.list"; then
		fail "the run's stick holds $absent - a backup that read nothing left a file (see $work/stick.list)"
	fi
done
# WHAT THE DEVICE SAW: one detach - the confirmed backup's, not the declined one's - one whole upload, and one stopped.
detaches="$(grep -ac 'DFU_DETACH' "$work/upload-device.log" || true)"
whole="$(grep -ac 'DFU_UPLOAD ended - 2500 bytes in 3 block(s)' "$work/upload-device.log" || true)"
stopped="$(grep -ac 'DFU_ABORT during an upload, after 2048 bytes' "$work/upload-device.log" || true)"
[[ "$detaches" == 1 ]] || fail "the upload target was detached $detaches times; the declined backup must not detach it"
[[ "$whole" == 1 && "$stopped" == 1 ]] || fail "the target saw $whole whole upload(s) and $stopped stopped one; expected one of each"
echo "qemu-dfu-tool: backup: declined, nothing was read; confirmed, the runtime target was followed into DFU mode and the file on the stick is its image byte for byte, with the SHA-256 the tool and the record state; past its bound, the upload was stopped and no file was written"
echo "qemu-dfu-tool: PASS - the shipping dfu command asked, a person confirmed on the protected screen, the executor followed the runtime target into DFU mode and wrote the image once, and a declined request wrote nothing; a backup was refused before the screen for a target without upload, read nothing when declined, wrote the target's image byte for byte when confirmed, and wrote no file past its bound"
