#!/usr/bin/env bash
# Every driver in the image declares the bring-up protocol version it speaks, IN THE BYTES THAT SHIP.
#
# The version has to be knowable BEFORE the device is claimed: refusing a driver afterwards means
# taking a device back from something that should never have held it. That is what the ELF note is
# for, and it is the one part of this that fails SILENTLY when it goes wrong - a driver with no note
# reads exactly like a driver that declares no version.
#
# AND IT WOULD HAVE GONE WRONG. All three linker scripts end with
#
#     /DISCARD/ : { *(.eh_frame*) *(.note .note.*) }
#
# and `mkpackages` then runs `llvm-strip --strip-all` over what is left. A note emitted into an
# ordinary `.note.*` section is gone twice over. So the note lives in a section the discard does not
# match, `KEEP`-ed inside `.rodata` where SHF_ALLOC saves it from the strip - and this gate reads
# what `mkpackages` PRODUCED rather than what the linker emitted, because the linker having been
# asked nicely is not evidence about what shipped.
#
# TWO PLACES, BECAUSE THE DRIVERS LIVE IN TWO PLACES. The boot-critical one is staged as a real file
# under `bootstrap-<arch>/libexec`, so it is checked by name. The rest are inside the system volume
# archive, where file boundaries are not visible to a byte scan - so those are COUNTED, against a
# floor, and the count is reported rather than assumed.
#
# EVERY ARCHITECTURE THAT HAS BEEN BUILT, because a note that survives on one and not another would
# make the version refusal an architecture-dependent accident.
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$(cd .. && pwd)"
BUILD_DIR="${BUILD_DIR:-$ROOT/.build}"

# The one source of truth for both numbers. Read out of the Rust rather than repeated here: a gate
# carrying its own copy of the constant it is checking would agree with itself while disagreeing
# with the system.
PROTOCOL_SRC="user/libs/driver/protocol/src/lib.rs"
magic_hex="$(sed -n 's/^pub const MAGIC: u32 = 0x\([0-9A-Fa-f_]*\);.*/\1/p' "$PROTOCOL_SRC" | tr -d '_')"
version="$(sed -n 's/^pub const VERSION: u16 = \([0-9]*\);.*/\1/p' "$PROTOCOL_SRC")"
if [[ -z "$magic_hex" || -z "$version" ]]; then
	echo "driver-note: cannot read MAGIC/VERSION out of $PROTOCOL_SRC - the gate has no constant to check against" >&2
	exit 1
fi

# HOW MANY DRIVERS EACH PLACE MUST HOLD, DERIVED FROM THE MANIFEST RATHER THAN WRITTEN DOWN HERE.
#
# A hand-written inventory is how a scan quietly stops covering things: adding a driver would leave
# the number behind, and a scan that silently finds fewer returns success over what it did find -
# which is indistinguishable from an image whose drivers all carry the note. Reading the manifest
# means adding a driver raises the floor on the next run, with nobody remembering to do anything.
#
# The development-only drivers are excluded, because a shipping build does not stage them and a
# floor that counted them would fail every shipping build. A development build simply exceeds it,
# which the reported count makes visible.
read_floor() {
	awk -v want="$1" '
		/^\[\[programs\]\]/ { if (role == "driver" && stage == want && dev == 0) n++; role = ""; stage = ""; dev = 0; next }
		/^role = "driver"$/ { role = "driver" }
		/^stage = / { gsub(/"/, "", $3); stage = $3 }
		/^development = true$/ { dev = 1 }
		END { if (role == "driver" && stage == want && dev == 0) n++; print n + 0 }
	' user/services/manifest.toml
}

MINIMUM_STAGED="$(read_floor pinned)"
MINIMUM_IN_VOLUME="$(read_floor volume)"
if [[ "$MINIMUM_STAGED" -lt 1 || "$MINIMUM_IN_VOLUME" -lt 1 ]]; then
	echo "driver-note: the manifest reports $MINIMUM_STAGED pinned and $MINIMUM_IN_VOLUME volume drivers - the floor is reading nothing" >&2
	exit 1
fi

# Count how many times the note appears in a file, for a given version.
count_notes() {
	python3 - "$1" "$magic_hex" "$2" <<'PY'
import sys
path, magic, version = sys.argv[1], int(sys.argv[2], 16), int(sys.argv[3])
name = b"LiberDriver\0"
blob = (len(name)).to_bytes(4, "little") + (8).to_bytes(4, "little") + (1).to_bytes(4, "little") + name + magic.to_bytes(4, "little") + version.to_bytes(2, "little")
data = open(path, "rb").read()
count, at = 0, data.find(blob)
while at >= 0:
    count += 1
    at = data.find(blob, at + 1)
print(count)
PY
}

checked=0
archs_seen=0
for arch in x86_64 aarch64 riscv64; do
	staged="$BUILD_DIR/boot/bootstrap-$arch/libexec"
	volume="$BUILD_DIR/boot/volume-$arch.pkg"
	[[ -d "$staged" && -f "$volume" ]] || continue
	archs_seen=$((archs_seen + 1))

	# The staged files, by name. Exact attribution: this driver, this note.
	found=0
	for file in "$staged"/*.lsexe; do
		[[ -e "$file" ]] || continue
		name="$(basename "$file" .lsexe)"
		case "$name" in
		virtio_* | xhci | dev_channel) ;;
		*) continue ;;
		esac
		found=$((found + 1))
		if [[ "$(count_notes "$file" "$version")" == 0 ]]; then
			echo "driver-note: $arch/$name carries no protocol note in its staged bytes." >&2
			echo "    The note is emitted into .liberdrv.note and must be KEEP-ed inside .rodata by" >&2
			echo "    src/user/user.ld, user-aarch64.ld and user-riscv64.ld - the /DISCARD/ at the end of" >&2
			echo "    each throws away every .note.* section, and mkpackages strips what is left." >&2
			exit 1
		fi
		# PROVE THE CHECK REFUSES BEFORE TRUSTING IT TO APPROVE. Handed a version these drivers were
		# not built with, it must find nothing - a matcher that ignored the version would otherwise
		# pass this gate for the life of the tree.
		if [[ "$(count_notes "$file" "$((version + 1))")" != 0 ]]; then
			echo "driver-note: SELF-TEST FAILED - a note declaring version $((version + 1)) was found in $arch/$name, so the matcher is not matching the version" >&2
			exit 1
		fi
		checked=$((checked + 1))
	done
	if [[ "$found" -lt "$MINIMUM_STAGED" ]]; then
		echo "driver-note: $arch staged only $found driver file(s), below the floor of $MINIMUM_STAGED - the scan has stopped finding them" >&2
		exit 1
	fi

	# And the volume archive, counted. The file boundaries are not visible here, so what is checked
	# is that AT LEAST as many notes are present as there are drivers inside it.
	in_volume="$(count_notes "$volume" "$version")"
	if [[ "$in_volume" -lt "$MINIMUM_IN_VOLUME" ]]; then
		echo "driver-note: $arch's system volume carries $in_volume protocol note(s), below the floor of $MINIMUM_IN_VOLUME." >&2
		echo "    Every driver on the volume must declare its protocol version, and this count is how" >&2
		echo "    a driver that lost its note is caught where the archive hides the file boundaries." >&2
		exit 1
	fi
	if [[ "$(count_notes "$volume" "$((version + 1))")" != 0 ]]; then
		echo "driver-note: SELF-TEST FAILED - a note declaring version $((version + 1)) was found in $arch's volume" >&2
		exit 1
	fi
	checked=$((checked + in_volume))
	echo "driver-note: $arch - $found staged driver file(s) and $in_volume note(s) in the volume, all declaring protocol version $version"
done

if [[ "$archs_seen" == 0 ]]; then
	echo "driver-note: no architecture has both a staged bootstrap set and a system volume. Build first:  ./build.sh --arch all" >&2
	exit 1
fi
echo "driver-note: $checked driver artifacts across $archs_seen architecture(s) carry the protocol note their own source emits"
