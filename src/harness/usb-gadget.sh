#!/usr/bin/env bash
# A USB device the harness BUILDS, for the classes QEMU has no model of.
#
# WHY THIS EXISTS. QEMU implements fifteen USB device models and none of them is a CDC-ACM serial
# port, a multi-touch digitiser, a gamepad or an audio capture endpoint - so four driver items read
# as "a device model to write", which is a month of work nobody had. That reading was wrong. This
# QEMU has `usb-host`, and Linux can BE a USB device: `dummy_hcd` is a gadget controller with no
# hardware behind it, a configfs tree binds a function to it, and the host then sees an ordinary USB
# device that `usb-host` hands to the guest. `usb_f_hid` takes an ARBITRARY report descriptor, which
# is exactly the multi-touch collection and the gamepad nobody had a model for.
#
# WHAT IT COSTS IS A PERMISSION, AND THE PERMISSION IS NARROW. Loading modules into the developer's
# kernel and writing under `/sys/kernel/config` is a test suite reaching outside its own process, and
# the owner's answer (2026-09-21) was yes WITH A CONDITION: carefully, nothing that brings the
# machine down and nothing deleted. So the rules below are the permission, written as code that
# either obeys them or refuses:
#
#   1. IT TOUCHES ONLY WHAT IT CREATED. The gadget directory is named for this run. A name that
#      already exists is a REFUSAL, never a directory to clear - somebody else's gadget is somebody
#      else's.
#   2. IT UNLOADS ONLY WHAT IT LOADED. The removal set is computed at setup as "absent before,
#      present after", not assumed. A module the host was already using stays, because removing it
#      is exactly what "do not bring the machine down" forbids.
#   3. IT DELETES NOTHING IT DID NOT MAKE. Teardown removes configfs OBJECTS - its own directory and
#      the one symlink inside it - and, in its own state directory, the files and the empty mount
#      points it made there: its state records, an emulator's ready file, a FunctionFS or gadgetfs
#      mount point once unmounted. Nothing here calls `rm` on any other path, and never `rm -r`.
#   4. IT REFUSES RATHER THAN FORCES. No `rmmod -f`, no taking a UDC something else has bound, no
#      retry that escalates. A precondition that is not as expected ends the run with the reason
#      named, and the tests that wanted it report unavailable.
#   5. IT CLEANS UP ON EVERY EXIT PATH and the teardown is idempotent: run twice, or against a setup
#      that never finished, it does nothing and says so.
#   6. AND THE HOST IS LEFT AS IT WAS FOUND, which is CHECKED and not merely intended - `verify`
#      says whether anything of this script's remains.
#   7. INCLUDING WHAT THE HOST LOADED BECAUSE OF THE GADGET. A gadget on `dummy_hcd` is a USB device on
#      the host's OWN bus first, and the host's USB core loads a driver for it - `usblp` for the printer,
#      `snd-usb-audio` and its raw MIDI chain for the MIDI device - before `usb-host` takes the device for
#      the guest. Those modules were not loaded before the run and nothing unloads them after it, so the
#      teardown does: the USB interface drivers that registered during the run, whose module was not loaded
#      before it, and the modules those pulled in that were not loaded before it either - by `rmmod`, one
#      at a time, never cascading into a module that was already there, and never forced.
set -euo pipefail

CONFIGFS=/sys/kernel/config
GADGET_ROOT="$CONFIGFS/usb_gadget"
STATE_DIR="${LIBER_GADGET_STATE:-${TMPDIR:-/tmp}/liber-usb-gadget}"

# The vendor and product this gadget presents. Linux Foundation's own id and its "Multifunction
# Composite Gadget" product, which is what a gadget with no vendor of its own is supposed to use -
# and what `usb-host` is then pointed at.
VENDOR_ID=1d6b
PRODUCT_ID=0104

# Every module a gadget of any supported kind needs, in dependency order. `modprobe` pulls what it
# needs anyway; the named modules absent before setup seed the removal set, together with new
# host USB/HID drivers and their dependencies that were also absent before setup.
# The MIDI function is the one that reaches past the gadget stack: it is an ALSA card as well, so the sound
# core's raw MIDI modules come with it - named here so a run that loaded them is the run that unloads them.
ALL_MODULES=(udc_core libcomposite dummy_hcd u_serial usb_f_acm usb_f_hid usb_f_uac2 usb_f_printer snd_seq_device snd_rawmidi usb_f_midi usb_f_fs)

say() {
	echo "usb-gadget: $*" >&2
}

refuse() {
	say "$*"
	exit 1
}

loaded_modules() {
	local module
	for module in /sys/module/*; do
		# sysfs also contains builtins and a non-directory compression attribute.
		[[ -d "$module" && -f "$module/initstate" ]] || continue
		printf '%s\n' "${module##*/}"
	done
}

loaded() {
	[[ -f "/sys/module/${1//-/_}/initstate" ]]
}

# What this run is allowed to take apart, recorded the moment it is created.
state_file() {
	echo "$STATE_DIR/$1"
}

gadget_dir() {
	echo "$GADGET_ROOT/$(cat "$(state_file name)" 2>/dev/null || true)"
}

# WHAT A KIND IS MADE OF: one line per function, as `<config> <function directory> <shape>`.
#
# IT IS A LIST AND NOT A NAME because two of the things the serial item has to be exercised against
# are not single-function devices. "Several simultaneous adapters" is two serial functions at once,
# and "the serial function in a later configuration" is a device whose FIRST configuration is
# something else - and a fixture that can only hold one function in one configuration can express
# neither. The shape is what the setup configures the function as; the config and the directory are
# what the teardown undoes, which is why the whole plan is written down before the first mkdir.
#
# A kind this script does not know is a refusal: inventing a descriptor for it would be inventing
# the device the test is about.
plan_for() {
	case "$1" in
	acm) printf 'c.1 acm.usb0 acm\n' ;;
	# TWO ADAPTERS ON ONE DEVICE, which is the harder half of "several simultaneous adapters" and
	# the one a driver gets wrong: each function has its own union descriptor naming its own data
	# interface, so a driver that takes "the lowest bulk pair in the configuration" binds function
	# zero's endpoints to function one's control interface and the two streams cross.
	acm-pair) printf 'c.1 acm.usb0 acm\nc.1 acm.usb1 acm\n' ;;
	# THE SERIAL FUNCTION IN A LATER CONFIGURATION. The first configuration holds a printer, which
	# is a class nothing in this tree binds - so what the driver has to do to find the serial port
	# is read past a configuration it cannot use, which is the whole of what this fixture is for. A
	# first configuration holding a function some OTHER module here binds would test the dispatch
	# order instead, which is a different question.
	acm-late) printf 'c.1 printer.usb0 printer\nc.2 acm.usb0 acm\n' ;;
	hid-touch) printf 'c.1 hid.usb0 hid-touch\n' ;;
	hid-gamepad) printf 'c.1 hid.usb0 hid-gamepad\n' ;;
	# TWO GAMEPADS ON ONE DEVICE, a HID function each, as `acm-pair` puts two serial ports on one: which is a
	# composite adapter with a HID interface per player, and the case a driver that binds only a device's first
	# HID interface gets wrong. ONE COMPOSITE DEVICE, UNPLUGGED WHOLE - the harness has one gadget controller and
	# one free root port, and a function cannot be unlinked from a bound gadget.
	hid-gamepad-pair) printf 'c.1 hid.usb0 hid-gamepad\nc.1 hid.usb1 hid-gamepad\n' ;;
	uac2) printf 'c.1 uac2.usb0 uac2\n' ;;
	# A PRINTER, for the USB printer class: the kernel's own printer function, whose far end is
	# `/dev/g_printer0` on this host - where `printer-sink.py` reads what the guest prints and answers
	# through the port status byte whether it was what the guest meant to send.
	printer) printf 'c.1 printer.usb0 printer-sink\n' ;;
	# A MIDI 1.0 DEVICE, the kernel's own function: to this host an ALSA card named `LiberMidi`, whose raw MIDI
	# output `midi-source.py` writes and whose USB side delivers that as event packets on two cables.
	midi) printf 'c.1 midi.usb0 midi\n' ;;
	# THE SAME FUNCTION, for the transmit oracle: `midi-source.py --echo` plays back what arrives.
	midi-echo) printf 'c.1 midi.usb0 midi\n' ;;
	# A UPS: the kernel's HID function with a HID Power Device report descriptor, answered by `ups-sim.py`.
	ups) printf 'c.1 hid.usb0 hid-ups\n' ;;
	# A MONITOR: the kernel's HID function with a Monitor Control collection - its brightness and the start of its
	# EDID - and an Ambient Light collection, answered by `monitor-sim.py`.
	monitor) printf 'c.1 hid.usb0 hid-monitor\n' ;;
	# A CONSUMER CONTROL KEYPAD with the two brightness keys, pressed by `keys-sim.py`.
	consumer-keys) printf 'c.1 hid.usb0 hid-consumer\n' ;;
	# THE CLASSES THE KERNEL HAS NO FUNCTION FOR, each a FunctionFS function whose far end is an emulator in
	# `usb_ffs.py`: a CCID reader with a PIV card in it.
	ccid) printf 'c.1 ffs.liber ffs-ccid\n' ;;
	# A DFU target in DFU mode, which takes an image and checks it at manifestation.
	dfu) printf 'c.1 ffs.liber ffs-dfu\n' ;;
	# A Bluetooth controller's HCI transport, looping ACL data back.
	bt) printf 'c.1 ffs.liber ffs-bt\n' ;;
	# THE CLASSES WHOSE DESCRIPTORS NEITHER KIND OF FUNCTION CARRIES - CDC's and UVC's class-specific interface
	# records - as whole devices through gadgetfs, played by `usb_gadgetfs.py`: a mobile-broadband modem.
	mbim) printf 'gadgetfs mbim\n' ;;
	# And a video camera streaming over bulk.
	uvc) printf 'gadgetfs uvc\n' ;;
	*) refuse "unknown gadget kind '$1' - known kinds are acm, acm-pair, acm-late, hid-touch, hid-gamepad, hid-gamepad-pair, uac2, printer, midi, midi-echo, ups, monitor, consumer-keys, ccid, dfu, bt, mbim, uvc" ;;
	esac
}

# THE REPORT DESCRIPTOR IS THE DEVICE, for the two HID kinds. `usb_f_hid` takes whatever bytes are
# written here and presents them, which is why these two classes needed no model written: the
# multi-touch collection and the gamepad ARE these byte strings.
#
# Written as escaped bytes for `printf %b`, because a descriptor is binary and a shell here-document
# is not.
hid_descriptor() {
	case "$1" in
	# A TWO-FINGER MULTI-TOUCH COLLECTION, and the shape is the point rather than the count.
	#
	# EACH FINGER IS ITS OWN LOGICAL COLLECTION holding a CONTACT IDENTIFIER, a tip switch and a
	# pair of axes, which is how a touch report says where one finger ends and the next begins -
	# and a decoder begins a new contact at each identifier. A descriptor without one carries
	# fields that belong to nobody, so it exercises the digitizer page and none of the decoding
	# the provider is about; the first version of this fixture was exactly that.
	#
	# AND A CONTACT COUNT, because a digitizer declares slots for every finger it can ever carry
	# and leaves the unused ones holding whatever was there before. A reader that takes every
	# declared slot reports phantom fingers at stale positions that nothing is touching.
	#
	# Per finger: four bits of identifier, one of tip, three of padding, then two 16-bit axes -
	# five bytes each, plus one byte of count, so eleven in a report.
	hid-touch)
		local finger='\x05\x0d\x09\x22\xa1\x02\x09\x51\x15\x00\x25\x0f\x75\x04\x95\x01\x81\x02\x09\x42\x25\x01\x75\x01\x81\x02\x75\x03\x81\x03\x05\x01\x09\x30\x26\xff\x7f\x75\x10\x81\x02\x09\x31\x81\x02\xc0'
		printf '%b' '\x05\x0d\x09\x04\xa1\x01'"$finger""$finger"'\x05\x0d\x09\x54\x15\x00\x25\x02\x75\x08\x95\x01\x81\x02\xc0'
		;;
	# A gamepad: two sticks as four axes, sixteen buttons, and a HAT - a four-bit Hat Switch over 0..7
	# (physical 0..315 degrees) with the NULL STATE flag, then four bits of padding, so a report is seven
	# bytes. The mapping under test is from this shape to the input vocabulary, so the shape is the fixture:
	# byte for byte what the driver library's parser tests call the gadget's gamepad.
	hid-gamepad)
		printf '%b' '\x05\x01\x09\x05\xa1\x01\x09\x30\x09\x31\x09\x32\x09\x35\x15\x00\x26\xff\x00\x75\x08\x95\x04\x81\x02\x05\x09\x19\x01\x29\x10\x15\x00\x25\x01\x75\x01\x95\x10\x81\x02\x05\x01\x09\x39\x15\x00\x25\x07\x35\x00\x46\x3b\x01\x65\x14\x75\x04\x95\x01\x81\x42\x65\x00\x75\x04\x95\x01\x81\x03\xc0'
		;;
	# A UPS, on the Power Device and Battery System pages. Byte for byte the descriptor the driver library's
	# `hid_power` tests carry: five status flags, a percentage capacity and a run time in input report 1; the
	# capacity mode, three capacities and the battery voltage in feature report 2; a writable
	# DelayBeforeShutdown in feature report 3.
	hid-ups)
		printf '%b' '\x05\x84\x09\x04\xa1\x01\x09\x24\xa1\x00\x85\x01\x05\x85\x09\xd0\x09\x44\x09\x45\x09\x42\x09\x4b\x15\x00\x25\x01\x75\x01\x95\x05\x81\x02\x75\x03\x95\x01\x81\x01\x09\x66\x25\x64\x75\x08\x81\x02\x09\x68\x27\xff\xff\x00\x00\x66\x01\x10\x75\x10\x81\x02\x65\x00\x85\x02\x09\x2c\x25\x03\x75\x08\xb1\x02\x09\x67\x09\x83\x09\x29\x25\x64\x95\x03\xb1\x02\x05\x84\x09\x30\x27\xff\xff\x00\x00\x67\x21\xd1\xf0\x00\x55\x05\x75\x10\x95\x01\xb1\x02\x65\x00\x55\x00\x85\x03\x09\x57\x16\xff\xff\x26\xff\x7f\x66\x01\x10\x75\x10\xb1\x02\x65\x00\xc0\xc0'
		;;
	# A MONITOR, on the Monitor and VESA Virtual Controls pages and the Sensors page. Byte for byte the descriptor the
	# driver library's `hid_display` tests carry: feature report 1 the EDID's first thirty-two bytes - the most a HID
	# gadget answers GET_REPORT with leaves no room for the whole block, and the identity is in the first sixteen -
	# feature report 2 a 0..100 Brightness, and input report 3 a sixteen-bit illuminance in lux.
	hid-monitor)
		printf '%b' '\x05\x80\x09\x01\xa1\x01\x85\x01\x09\x02\x15\x00\x26\xff\x00\x75\x08\x95\x20\xb2\x02\x01\x05\x82\x85\x02\x09\x10\x15\x00\x25\x64\x75\x08\x95\x01\xb1\x02\x85\x04\x09\x10\x81\x02\xc0\x05\x20\x09\x41\xa1\x01\x85\x03\x0a\xd1\x04\x15\x00\x27\xff\xff\x00\x00\x75\x10\x95\x01\x55\x00\x81\x02\xc0'
		;;
	# A CONSUMER CONTROL collection, one sixteen-bit usage array: the brightness increment (0x6f) and decrement (0x70)
	# among the usages it may report, and zero for none pressed.
	hid-consumer)
		printf '%b' '\x05\x0c\x09\x01\xa1\x01\x15\x00\x26\xff\x03\x19\x00\x2a\xff\x03\x75\x10\x95\x01\x81\x00\xc0'
		;;
	esac
}

# WRITE ONE CONFIGFS ATTRIBUTE, WAITING OUT A TRANSIENT REFUSAL.
#
# A function module holds a reference for a moment after the gadget that used it is torn down, and
# an attribute written in that window answers EBUSY - measured here as one setup in two or three
# failing on `c_srate` while the same write by hand always succeeded. That is a race with the
# previous run's teardown and not a precondition that is wrong.
#
# WAITING IS NOT FORCING, which is the distinction rule 4 draws. Nothing here retries with a bigger
# hammer: the same write is offered again for a bounded time and then the setup REFUSES, so a module
# that is genuinely held by something else ends the run with the reason named rather than being
# taken from whatever is holding it.
write_attr() {
	local path="$1" value="$2" attempt
	for attempt in $(seq 1 50); do
		if printf '%s\n' "$value" >"$path" 2>/dev/null; then
			return 0
		fi
		sleep 0.1
	done
	refuse "$path would not take '$value' - something else is holding this function"
}

# The same wait, answering instead of refusing, for a caller with a better sentence to say.
write_attr_quietly() {
	local path="$1" value="$2" attempt
	for attempt in $(seq 1 50); do
		if printf '%s\n' "$value" >"$path" 2>/dev/null; then
			return 0
		fi
		sleep 0.1
	done
	return 1
}

hid_report_length() {
	case "$1" in
	hid-touch) echo 11 ;;
	hid-gamepad) echo 7 ;;
	# The longest report with its ID byte: feature report 2 is seven.
	hid-ups) echo 8 ;;
	# The EDID feature report with its ID byte: the function caps both GET_REPORT's answer and SET_REPORT's data at
	# this length.
	hid-monitor) echo 33 ;;
	hid-consumer) echo 2 ;;
	esac
}

# A setup failure must clean its own effects: callers only claim a successfully returned gadget.
# Read ownership from the persisted records, not cmd_setup locals that may already be out of scope.
setup_exit() {
	local setup_status="$1"
	trap - EXIT
	if [[ "$setup_status" != "0" ]]; then
		# Keep the original failure even if teardown refuses. The child retains errexit for its
		# filesystem operations; testing cmd_teardown directly with `if` would disable that.
		set +e
		(
			set -e
			cmd_teardown
		)
		local cleanup_status=$?
		[[ "$cleanup_status" == "0" ]] || say "failed setup cleanup is incomplete; ownership records are retained"
	fi
	exit "$setup_status"
}

cmd_setup() {
	local kind="${1:-}"
	[[ -n "$kind" ]] || refuse "setup needs a kind"
	local plan
	plan="$(plan_for "$kind")"

	# RULE 4, FIRST: every precondition, checked and named.
	[[ "$(id -u)" == "0" ]] || refuse "building a gadget needs root, and this is not it"
	mountpoint -q "$CONFIGFS" || refuse "configfs is not mounted at $CONFIGFS"

	mkdir -p "$STATE_DIR"
	[[ ! -e "$(state_file name)" ]] || refuse "a gadget from this harness is already set up - run teardown first"

	# Check a foreign-name collision before recording ownership or installing cleanup.
	local name="liber$$"
	local dir="$GADGET_ROOT/$name"
	[[ ! -e "$dir" ]] || refuse "$dir already exists - this harness never clears a gadget it did not make"

	# RULE 7: what the host had loaded, and which USB drivers it had, before anything of this run's existed.
	loaded_modules | sort >"$(state_file modules-before)" || refuse "cannot record the preexisting modules"
	host_drivers | sort >"$(state_file usb-drivers-before)" || refuse "cannot record the preexisting host drivers"
	local rfkill_before
	rfkill_before="$(systemctl is-active systemd-rfkill.socket 2>/dev/null || true)"
	printf '%s\n' "$rfkill_before" >"$(state_file rfkill-socket-before)" || refuse "cannot record the preexisting rfkill socket"

	# Record the run BEFORE loading anything, including failures before a UDC exists.
	printf '%s\n' "$plan" >"$(state_file plan)" || refuse "cannot record the gadget plan"
	: >"$(state_file modules)" || refuse "cannot create the module ownership ledger"
	printf '%s\n' "$name" >"$(state_file name)" || refuse "cannot record the gadget name"
	trap 'setup_exit "$?"' EXIT

	# A GADGETFS DEVICE uses only its whole-device stack, not configfs function modules.
	local modules_wanted=("${ALL_MODULES[@]}")
	[[ "$plan" != gadgetfs* ]] || modules_wanted=(udc_core dummy_hcd gadgetfs)
	local module
	for module in "${modules_wanted[@]}"; do
		module="${module//-/_}"
		# Write ahead of each side effect. Unload ignores entries that never became loaded,
		# and its dependency closure excludes every module from the saved before inventory.
		if ! grep -qxF "$module" "$(state_file modules-before)"; then
			printf '%s\n' "$module" >>"$(state_file modules)" || refuse "cannot record module ownership before loading $module"
		fi
		modprobe "$module" 2>/dev/null || true
	done
	# udc_core and dummy_hcd are the two that must actually be there; the function modules are
	# pulled in by the gadget when its function directory is created, and a kernel that ships them
	# built in has them without a module to load.
	loaded dummy_hcd || refuse "dummy_hcd did not load for this kernel - there is no gadget controller to bind to"

	local udc=""
	local candidate
	for candidate in /sys/class/udc/*; do
		[[ -e "$candidate" ]] || continue
		udc="$(basename "$candidate")"
		break
	done
	[[ -n "$udc" ]] || refuse "no UDC appeared after loading dummy_hcd"
	# RULE 4 AGAIN: a controller something else has bound is not one to take.
	local holder
	holder="$(cat "/sys/class/udc/$udc/function" 2>/dev/null || true)"
	[[ -z "$holder" ]] || refuse "UDC $udc is already driving '$holder' - this harness does not take a controller from whatever is using it"

	printf '%s\n' "$udc" >"$(state_file udc)"

	if [[ "$plan" == gadgetfs* ]]; then
		start_gadgetfs "${plan#gadgetfs }"
		say "$kind is bound to $udc through gadgetfs ($VENDOR_ID:$PRODUCT_ID)"
		printf '%s:%s\n' "$VENDOR_ID" "$PRODUCT_ID"
		trap - EXIT
		return 0
	fi

	mkdir "$dir"
	printf '0x%s\n' "$VENDOR_ID" >"$dir/idVendor"
	printf '0x%s\n' "$PRODUCT_ID" >"$dir/idProduct"
	mkdir "$dir/strings/0x409"
	printf 'LiberSystem harness\n' >"$dir/strings/0x409/manufacturer"
	printf '%s\n' "$kind" >"$dir/strings/0x409/product"
	printf '%s\n' "$name" >"$dir/strings/0x409/serialnumber"
	local config function_name shape
	local made_configs=" "
	while read -r config function_name shape; do
		[[ -n "$config" ]] || continue
		# EACH CONFIGURATION MADE ONCE, however many functions land in it.
		if [[ "$made_configs" != *" $config "* ]]; then
			mkdir "$dir/configs/$config"
			mkdir "$dir/configs/$config/strings/0x409"
			printf '%s %s\n' "$kind" "$config" >"$dir/configs/$config/strings/0x409/configuration"
			made_configs+="$config "
		fi
		mkdir "$dir/functions/$function_name"
		configure_function "$dir" "$function_name" "$shape"
		ln -s "$dir/functions/$function_name" "$dir/configs/$config/$function_name"
	done <<<"$plan"

	# THE BIND HAS TWO FAILURES THAT LOOK IDENTICAL FROM HERE, and the wait tells them apart.
	#
	# A controller whose PREVIOUS gadget is still unbinding answers EBUSY for a moment, which is a
	# race with this harness's own teardown and is what the wait is for. A FUNCTION THIS CONTROLLER
	# CANNOT CARRY answers the same way for ever - measured with `uac2` on `dummy_hcd`, where the
	# kernel log says `afunc_bind ... Error!` and `failed to start: -19`, because the isochronous
	# endpoints that function needs are not ones this controller provides. The second is not a race
	# and no amount of waiting changes it, so the refusal names both rather than guessing.
	if ! write_attr_quietly "$dir/UDC" "$udc"; then
		refuse "$udc would not take this gadget - either something else is binding to it, or $kind is a function this controller cannot carry (the kernel log says which)"
	fi

	say "$kind is bound to $udc as $name ($VENDOR_ID:$PRODUCT_ID)"
	printf '%s:%s\n' "$VENDOR_ID" "$PRODUCT_ID"
	trap - EXIT
}

# What one function needs written into it before it is linked into a configuration. A shape with
# nothing to set is not an omission: `acm` and `printer` are what their modules make them.
configure_function() {
	local dir="$1" function_name="$2" shape="$3"
	case "$shape" in
	# THE CAPTURE RATE, BECAUSE THE DEFAULT IS NOT ONE THIS SYSTEM ADMITS. `usb_f_uac2` comes up
	# with playback at 48 kHz and CAPTURE AT 64 - and the driver above refuses anything but 48 kHz
	# stereo 16-bit rather than resampling, for the reason its own item states: a driver that
	# quietly took 44.1 for 48 plays everything a semitone out with nothing reporting it. A
	# fixture presenting a rate the driver is right to refuse would be testing the refusal.
	#
	# The sample size and the channel mask already match - two bytes and stereo on both
	# directions - and are left as the module set them.
	uac2)
		write_attr "$dir/functions/$function_name/c_srate" 48000
		;;
	# THE DEVICE ID THE GUEST MUST HAND ON EXACTLY, and a queue short enough that a host which stops reading
	# is felt: two requests of the function's buffer, so the guest's writes wait on this host within a few
	# pages rather than after the eighty kilobytes the default ten would swallow.
	printer-sink)
		write_attr "$dir/functions/$function_name/pnp_string" "MFG:LiberSystem;MDL:LiberSystem harness printer;CMD:PCL,PS;CLS:PRINTER;"
		write_attr "$dir/functions/$function_name/q_len" 2
		;;
	# TWO CABLES TOWARDS THE GUEST, because a cable is an identity the transport must keep: one receive
	# endpoint, two embedded jacks, and bytes written to the card's second port arrive as cable one's.
	#
	# BOTH COUNTS ARE TWO, AND THAT IS MEASURED RATHER THAN READ. On this kernel `in_ports` is what the IN
	# endpoint declares - the guest's receive side - while the card's raw MIDI OUTPUT substreams number
	# `out_ports` and each one opens only below `in_ports`. With `out_ports` 2 and `in_ports` 1 the guest saw
	# one cable and the second port refused to open (EINVAL); with both at 2 the two line up.
	midi)
		write_attr "$dir/functions/$function_name/id" LiberMidi
		write_attr "$dir/functions/$function_name/in_ports" 2
		write_attr "$dir/functions/$function_name/out_ports" 2
		;;
	# A FUNCTIONFS FUNCTION IS NOT READY UNTIL ITS PROCESS HAS SPOKEN: the instance is mounted, the emulator
	# writes the descriptors through its `ep0`, and the endpoint files it then has are the sign it did. A gadget
	# bound before that is refused by the kernel, so the setup waits for them - and refuses if they never come.
	ffs-*)
		start_functionfs "$function_name" "${shape#ffs-}"
		;;
	hid-touch | hid-gamepad | hid-ups | hid-monitor | hid-consumer)
		write_attr "$dir/functions/$function_name/protocol" 0
		write_attr "$dir/functions/$function_name/subclass" 0
		write_attr "$dir/functions/$function_name/report_length" "$(hid_report_length "$shape")"
		# ONE WRITE, because each write of this attribute REPLACES the descriptor: bash's printf hands its output over
		# at every newline byte, and a descriptor holding 0x0a - the monitor's two-byte Illuminance usage is `0a d1 04`
		# - reached the function as only what followed the last one. `dd` gathers the whole of it first.
		hid_descriptor "$shape" | dd of="$dir/functions/$function_name/report_desc" bs=4096 iflag=fullblock status=none
		# NO OUT ENDPOINT FOR THE UPS, so SET_REPORT comes over the control pipe - which is how a host writes a
		# UPS's feature reports - and reaches `ups-sim.py` through the function's read side.
		if [[ "$shape" == "hid-ups" || "$shape" == "hid-monitor" ]]; then
			write_attr "$dir/functions/$function_name/no_out_endpoint" 1
		fi
		;;
	esac
}

# The drivers the host's USB stack binds a device's pieces with: its interfaces on the USB bus, and - for a HID
# function - the HID devices `usbhid` makes of them, which `hid-generic` binds on the HID bus.
host_drivers() {
	local bus driver
	for bus in usb hid; do
		for driver in /sys/bus/"$bus"/drivers/*; do
			[[ -e "$driver" ]] || continue
			printf '%s/%s\n' "$bus" "$(basename "$driver")"
		done
	done
}

# An individual module removal must not hold teardown forever if the host kernel is unhealthy.
# GNU timeout owns only this invocation's process group; no unrelated process is signalled.
remove_owned_module() {
	local status=0
	timeout --signal=TERM --kill-after=1 5 rmmod "$1" 2>/dev/null || status=$?
	if [[ "$status" == "124" || "$status" == "137" ]]; then
		say "removing $1 timed out; no further module removal is attempted"
		return 2
	fi
	return "$status"
}

# RULES 2 AND 7: explicit gadget modules, new USB/HID drivers, and dependencies that were absent
# before this run. Remove each one independently: modprobe -r also removes unused PREEXISTING
# dependencies, even when the named module itself was ours.
unload_owned_modules() {
	local before_modules before_drivers ledger
	before_modules="$(state_file modules-before)"
	before_drivers="$(state_file usb-drivers-before)"
	ledger="$(state_file modules)"
	[[ -f "$before_modules" && -f "$before_drivers" && -f "$ledger" ]] || {
		say "module ownership records are missing; no module is removed"
		return 1
	}
	local -A chain=()
	local driver module holder dependency
	while read -r module; do
		[[ -n "$module" ]] || continue
		if grep -qxF "$module" "$before_modules"; then
			say "$module was present before this run but appears in its removal ledger"
			return 1
		fi
		chain["$module"]=1
	done <"$ledger"
	for driver in /sys/bus/usb/drivers/* /sys/bus/hid/drivers/*; do
		[[ -e "$driver/module" ]] || continue
		grep -qxF "$(basename "$(dirname "$(dirname "$driver")")")/$(basename "$driver")" "$before_drivers" && continue
		module="$(basename "$(readlink -f "$driver/module")")"
		grep -qxF "$module" "$before_modules" && continue
		loaded "$module" && chain["$module"]=1
	done
	# holders is the reverse dependency relation: an owned holder can only release a dependency
	# after it leaves. Builtins have no initstate and are not candidates.
	local grew=1
	while [[ "$grew" == "1" ]]; do
		grew=0
		while read -r dependency; do
			[[ -z "${chain[$dependency]:-}" ]] || continue
			grep -qxF "$dependency" "$before_modules" && continue
			for holder in "/sys/module/$dependency/holders/"*; do
				[[ -e "$holder" && -n "${chain[${holder##*/}]:-}" ]] || continue
				chain["$dependency"]=1
				grew=1
				break
			done
		done < <(loaded_modules)
	done
	# Keep the complete ownership fact before the first removal: on refusal, an already orphaned
	# dependency still belongs to this run and the next teardown can identify it without guessing.
	for module in "${!chain[@]}"; do
		if ! grep -qxF "$module" "$ledger"; then
			printf '%s\n' "$module" >>"$ledger" || {
				say "the module ownership ledger could not be extended; no module is removed"
				return 1
			}
		fi
	done
	local round progress status
	for round in $(seq 1 20); do
		if [[ -n "${chain[rfkill]:-}" ]] && [[ "$(cat "$(state_file rfkill-socket-before)" 2>/dev/null)" != "active" ]] && [[ "$(systemctl is-active systemd-rfkill.socket 2>/dev/null)" == "active" ]]; then
			systemctl stop systemd-rfkill.socket 2>/dev/null && say "stopped systemd-rfkill.socket, which rfkill's arrival started"
		fi
		progress=1
		while [[ "$progress" == "1" ]]; do
			progress=0
			for module in "${!chain[@]}"; do
				loaded "$module" || {
					unset "chain[$module]"
					continue
				}
				[[ -z "$(ls "/sys/module/$module/holders" 2>/dev/null)" ]] || continue
				[[ "$(cat "/sys/module/$module/refcnt" 2>/dev/null || echo 1)" == "0" ]] || continue
				status=0
				remove_owned_module "$module" || status=$?
				if [[ "$status" == "0" ]]; then
					say "unloaded $module, which this run loaded for the gadget"
					unset "chain[$module]"
					progress=1
				elif [[ "$status" == "2" ]]; then
					return 1
				fi
			done
		done
		[[ "${#chain[@]}" == "0" ]] && break
		sleep 0.25
	done
	for module in "${!chain[@]}"; do
		say "$module was loaded for the gadget and is in use - it was left loaded"
	done
	[[ "${#chain[@]}" == "0" ]] || return 1
	while read -r module; do
		[[ -n "$module" ]] || continue
		loaded "$module" || {
			say "preexisting module $module is missing after teardown"
			return 1
		}
	done <"$before_modules"
}

# RULES 1 AND 3 FOR A FUNCTIONFS INSTANCE: its mount point is a directory this run makes inside its own state
# directory, recorded before the mount, and the emulator's process is recorded the moment it starts - so the
# teardown stops exactly that process and unmounts exactly that mount, and nothing else.
start_functionfs() {
	local function_name="$1" emulate="$2"
	local instance="${function_name#ffs.}"
	local mount="$STATE_DIR/ffs-$instance"
	mkdir "$mount"
	printf '%s\n' "$mount" >>"$(state_file mounts)"
	mount -t functionfs "$instance" "$mount" || refuse "the FunctionFS instance $instance would not mount"
	# Its output is not this script's: a process that kept this script's standard output open would hold the
	# caller's command substitution open for as long as it runs.
	local ready="$mount.ready"
	printf '%s\n' "$ready" >>"$(state_file mounts)"
	python3 "$(dirname "${BASH_SOURCE[0]}")/usb_ffs.py" --emulate "$emulate" --mount "$mount" --ready "$ready" >/dev/null &
	printf '%s\n' "$!" >>"$(state_file emulators)"
	local attempt
	for attempt in $(seq 1 100); do
		[[ -e "$ready" ]] && return 0
		sleep 0.1
	done
	refuse "the $emulate emulator never wrote its descriptors"
}

# THE EMULATORS STOP BEFORE THE GADGET IS UNBOUND, AND THAT ORDER IS MEASURED. An emulator waiting for its next
# event sits in a read of `ep0` holding the instance's lock, and the unbind takes that lock - so unbinding first
# left the teardown in an uninterruptible wait for as long as the emulator lived, which was until something killed
# it. Stopped first, the emulator's files close and the unbind has nothing to wait for.
# A GADGETFS DEVICE: the mount in this run's state directory, the emulator on it, and the sign it wrote its
# descriptors - after which the gadget is bound, because gadgetfs binds on that write.
start_gadgetfs() {
	local emulate="$1"
	local mount="$STATE_DIR/gadgetfs"
	mkdir "$mount"
	printf '%s\n' "$mount" >>"$(state_file mounts)"
	mount -t gadgetfs gadgetfs "$mount" || refuse "gadgetfs would not mount"
	local ready="$mount.ready"
	printf '%s\n' "$ready" >>"$(state_file mounts)"
	python3 "$(dirname "${BASH_SOURCE[0]}")/usb_gadgetfs.py" --emulate "$emulate" --mount "$mount" --ready "$ready" >/dev/null &
	printf '%s\n' "$!" >>"$(state_file emulators)"
	local attempt
	for attempt in $(seq 1 100); do
		[[ -e "$ready" ]] && return 0
		sleep 0.1
	done
	refuse "the $emulate emulator never wrote its descriptors"
}

stop_emulators() {
	local pid
	while read -r pid; do
		[[ -n "$pid" ]] || continue
		# OURS BY WHAT IT IS RUNNING, not by a number that may have been reused.
		if grep -qaE "usb_ffs.py|usb_gadgetfs.py" "/proc/$pid/cmdline" 2>/dev/null; then
			kill "$pid" 2>/dev/null || true
			local attempt
			for attempt in $(seq 1 50); do
				[[ -e "/proc/$pid" ]] || break
				sleep 0.1
			done
		fi
	done < <(cat "$(state_file emulators)" 2>/dev/null || true)
}

# And the mounts, once nothing is bound: an instance still in a bound gadget is busy.
unmount_functionfs() {
	local mount
	while read -r mount; do
		[[ -n "$mount" && ("$mount" == "$STATE_DIR"/ffs-* || "$mount" == "$STATE_DIR"/gadgetfs*) ]] || continue
		# The emulator's ready file, which this script named in its own state directory.
		if [[ "$mount" == *.ready ]]; then
			rm -f "$mount"
			continue
		fi
		if mountpoint -q "$mount"; then
			umount "$mount" || say "$mount would not unmount"
		fi
		[[ ! -d "$mount" ]] || rmdir "$mount" 2>/dev/null || true
	done < <(cat "$(state_file mounts)" 2>/dev/null || true)
}

# RULE 5: idempotent, and safe against a setup that never finished. Every step is guarded, because
# teardown running against half a tree is the ordinary case rather than the exceptional one.
cmd_teardown() {
	local name
	name="$(cat "$(state_file name)" 2>/dev/null || true)"
	if [[ -z "$name" ]]; then
		say "nothing of this harness's is set up"
		return 0
	fi
	local dir="$GADGET_ROOT/$name"
	local plan
	plan="$(cat "$(state_file plan)" 2>/dev/null || true)"

	if [[ -d "$dir" ]]; then
		stop_emulators
		# Unbind first: a gadget still bound to a UDC refuses every rmdir under it.
		[[ ! -e "$dir/UDC" ]] || printf '\n' >"$dir/UDC" 2>/dev/null || true
		# THEN THE FUNCTIONFS MOUNTS: an instance still mounted refuses the rmdir of its function directory.
		unmount_functionfs
		# EXACTLY WHAT THE PLAN SAYS WAS MADE, and nothing else. Rule 3's whole extent: `rm` is
		# never called on anything outside `$dir`, and the paths it is called on are the ones setup
		# wrote down before it created them.
		local config function_name shape
		local configs=""
		while read -r config function_name shape; do
			[[ -n "$config" ]] || continue
			[[ ! -L "$dir/configs/$config/$function_name" ]] || rm "$dir/configs/$config/$function_name"
			[[ ! -d "$dir/functions/$function_name" ]] || rmdir "$dir/functions/$function_name"
			[[ "$configs" == *" $config "* ]] || configs+=" $config "
		done <<<"$plan"
		for config in $configs; do
			[[ ! -d "$dir/configs/$config/strings/0x409" ]] || rmdir "$dir/configs/$config/strings/0x409"
			[[ ! -d "$dir/configs/$config" ]] || rmdir "$dir/configs/$config"
		done
		[[ ! -d "$dir/strings/0x409" ]] || rmdir "$dir/strings/0x409"
		rmdir "$dir"
	else
		# A GADGETFS DEVICE has no configfs tree: stopping its emulator closes the controller file, which is what
		# unbinds the gadget, and its mount goes before any module can.
		stop_emulators
		unmount_functionfs
	fi

	unload_owned_modules || return 1

	# A setup that died before the gadget directory existed can still have started an emulator.
	stop_emulators
	unmount_functionfs
	rm -f "$(state_file name)" "$(state_file plan)" "$(state_file function)" "$(state_file udc)" "$(state_file modules)" "$(state_file modules-before)" "$(state_file usb-drivers-before)" "$(state_file mounts)" "$(state_file emulators)" "$(state_file rfkill-socket-before)"
	rmdir "$STATE_DIR" 2>/dev/null || true
	say "torn down"
}

# RULE 6: the host is left as it was found, CHECKED. Nothing of this script's may remain.
cmd_verify() {
	local remaining=0
	local dir
	for dir in "$GADGET_ROOT"/liber*; do
		[[ -e "$dir" ]] || continue
		say "a gadget of this harness's remains: $dir"
		remaining=1
	done
	if [[ -e "$(state_file name)" ]]; then
		say "state from an earlier setup remains: $(state_file name)"
		remaining=1
	fi
	local owned_mounts
	owned_mounts="$(
		python3 - "$STATE_DIR" <<'PY_MOUNTS'
from pathlib import Path
import re
import sys
state = sys.argv[1]
for line in Path('/proc/self/mountinfo').read_text().splitlines():
    mount = re.sub(r'\\(040|011|012|134)', lambda match: chr(int(match[1], 8)), line.split()[4])
    if mount.startswith((state + '/ffs-', state + '/gadgetfs')):
        print(mount)
PY_MOUNTS
	)" || {
		say "the host mount inventory could not be read"
		return 1
	}
	if [[ -n "$owned_mounts" ]]; then
		say "a FunctionFS or gadgetfs mount of this harness's is still there"
		remaining=1
	fi
	[[ "$remaining" == "0" ]] || return 1
	say "the host carries nothing of this harness's"
}

case "${1:-}" in
setup)
	shift
	cmd_setup "$@"
	;;
teardown) cmd_teardown ;;
verify) cmd_verify ;;
*)
	echo "usage: usb-gadget.sh setup <acm|acm-pair|acm-late|hid-touch|hid-gamepad|hid-gamepad-pair|uac2|printer|midi|midi-echo|ups|monitor|consumer-keys|ccid|dfu|bt|mbim|uvc> | teardown | verify" >&2
	exit 2
	;;
esac
