AUDITOR'S REVIEW OF PLAN P02M0199 (2026-09-25T22:59:57Z):

**Rating: 3/10.** The providers and the policy are the right ones. But the output the whole contract hangs on does not exist in DisplayService, no process can publish the ACPI provider as written, and neither the key path nor the named USB fixture can deliver what the gates assume.

Reviewed [the plan](/data/yellow/libersystem/docs/todo/P02M0199.md) at commit `07371c44af82a11c1d275b0cbd832899f712e62a`; the plan itself is a new, untracked file, as are P02M0197 to P02M0202, and P02M0190, P02M0196 and P02M0099 carry uncommitted edits; all were read in the working tree, and line numbers refer to it. Also reviewed the plans it leans on: [P02M0196](/data/yellow/libersystem/docs/todo/P02M0196.md:29) for identity, AML and the fixture, and [P02M0099's DDC/AUX blocker](/data/yellow/libersystem/docs/todo/P02M0099.md:258). In the tree, the review covered:

- DisplayService and the `display` and `display-device` IDL;
- the provider catalogue and its closed kind set;
- the key path from the USB HID and virtio-input drivers through InputService;
- PermissionManager's display grants;
- the `usb-redir` and gadget fixtures.

ACPI and USB behaviour was checked against Linux's ACPI video, namespace-scan and backlight-selection code and the [USB Monitor Control Class specification](https://usb.org/sites/default/files/usbmon10.pdf). The findings concern decisions the plan must make before its parts get their own plans. They do not concern the expected absence of implementation.

1. **High - Brightness is specified as a property of a display output, joined to its provider, but DisplayService has no outputs and neither named provider can be joined to one.**

   The plan addresses everything to an output:
   - the [contract item](/data/yellow/libersystem/docs/todo/P02M0199.md:15) makes brightness "a property of a display OUTPUT in DisplayService's vocabulary";
   - every provider is ["joined to that output"](/data/yellow/libersystem/docs/todo/P02M0199.md:18);
   - the [tool](/data/yellow/libersystem/docs/todo/P02M0199.md:47) lists "the outputs that can be dimmed";
   - keys go to ["the output being worked on"](/data/yellow/libersystem/docs/todo/P02M0199.md:42);
   - the policy restores ["the last level of each output"](/data/yellow/libersystem/docs/todo/P02M0199.md:43) at boot.

   DisplayService has one scanout and no output identity:
   - the surface configuration's [`output` field](/data/yellow/libersystem/src/idl/display.lsidl:126) is [always 0](/data/yellow/libersystem/src/user/services/core/src/display_service.rs:468);
   - [`set-scale`](/data/yellow/libersystem/src/idl/display.lsidl:464) names no output;
   - the [display-device wire](/data/yellow/libersystem/src/idl/display-device.lsidl:57) carries one backing, with no connector, EDID or name;
   - the service [adopts the first display provider it connects to](/data/yellow/libersystem/src/user/services/core/src/display_service.rs:1760), or [falls through to the boot framebuffer](/data/yellow/libersystem/src/user/services/core/src/display_service.rs:1698).

   Neither join is defined anywhere:
   - **ACPI.** A video adapter and its output devices are `_ADR` devices. The adapter's address is its PCI device and function; an output's is a display ID. They reach a display only through the PCI function behind the adapter. Linux's [backlight selection](https://raw.githubusercontent.com/torvalds/linux/master/drivers/acpi/video_detect.c) runs "after PCI devices are glued with ACPI devices". Its [namespace scan](https://raw.githubusercontent.com/torvalds/linux/master/drivers/acpi/scan.c) adds a Linux-specific identity to video devices, because the firmware gives them none.
   - **What P02M0196 offers instead.** [P02M0196a](/data/yellow/libersystem/docs/todo/P02M0196.md:29) gives platform devices only `_HID`, `_CID` and `_UID` identities, and [drivers match on those](/data/yellow/libersystem/docs/todo/P02M0196.md:34). So neither the video device nor its PCI companion can be named.
   - **The laptop case.** On the laptops this provider exists for, the panel is today the firmware framebuffer, which has no provider and no PCI binding at all. The GOP item was [closed with its publication path left to a native graphics driver nobody has ordered](/data/yellow/libersystem/docs/todo/P02M0099.md:2934).
   - **USB.** A monitor-control interface has no display-side key except the EDID its Monitor page can report (usage 02h, [USB Monitor Control Class 1.0](https://usb.org/sites/default/files/usbmon10.pdf)). Fetching an output's EDID is the [DDC/AUX transport P02M0099 still lists as unowned](/data/yellow/libersystem/docs/todo/P02M0099.md:271).

   The plan also leaves two cases open. When two providers claim one output - ACPI video and a native driver's registers - it does not say which wins; that conflict is why Linux carries a whole selection module. And it does not say what a provider that matches no output does.

   Levels, events, keys, restore at boot and the tool are all addressed to an output, so none of them can be written until this is decided.

   **Correct the contract and provider items** by defining the output model first:
   - how outputs are enumerated and identified, at least for today's single output;
   - a key that survives a reboot, for the restore policy;
   - where DisplayService stores the level, since it has [no configuration role](/data/yellow/libersystem/src/user/services/manifest.toml:3603).

   Then state each provider's join rule:
   - ACPI: the adapter's `_ADR`, resolved to the PCI function of the display provider, or of the device that decodes the boot framebuffer;
   - USB: EDID where both sides have one, and otherwise an explicit fallback, such as the single output or an association the operator makes.

   Say which provider wins when several claim one output, and what an unjoined provider does. Finally, require P02M0196a to identify `_ADR` devices and their PCI companions, or state that this milestone synthesizes the video identity itself.

2. **High - No process can publish the ACPI `backlight` provider as the plan assumes, and the ACPI backlight class is claimed both here and by P02M0196b.**

   Only a driver binding can publish:
   - [`publish_all`](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:2762) is reached only from [committed](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:4325) [bindings](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:4483);
   - the [catalogue interface](/data/yellow/libersystem/src/idl/device.lsidl:360) has no publish operation;
   - a [provider's identity](/data/yellow/libersystem/src/idl/device.lsidl:230) is its publisher's PCI address.

   The [plan](/data/yellow/libersystem/docs/todo/P02M0199.md:18) says "a driver publishes". But `_BCL`, `_BCM` and `_BQC` run in [P02M0196b's interpreter](/data/yellow/libersystem/docs/todo/P02M0196.md:43), which is proposed as a userspace service, and that step gives a driver no way in:
   - it answers drivers only [`_DSM` and `_DSD`](/data/yellow/libersystem/docs/todo/P02M0196.md:49);
   - it delivers `Notify` ["as events"](/data/yellow/libersystem/docs/todo/P02M0196.md:46), to no named recipient;
   - it lists [backlight among its own device classes](/data/yellow/libersystem/docs/todo/P02M0196.md:51).

   Meanwhile [0199b](/data/yellow/libersystem/docs/todo/P02M0199.md:25) specifies the same methods again. That breaks P02M0196's own rule that ["one device has one owner"](/data/yellow/libersystem/docs/todo/P02M0196.md:103).

   The ambient-light sensor has no provider kind or consumer at all, although `backlight`'s [only client is DisplayService](/data/yellow/libersystem/docs/todo/P02M0199.md:19). Kinds are a closed set in the [IDL](/data/yellow/libersystem/src/idl/device.lsidl:151), the [manifest](/data/yellow/libersystem/src/tools/system-manifest/src/lib.rs:166) and [DeviceManager](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:6066), and DisplayService's catalogue role [admits `display` only](/data/yellow/libersystem/src/user/services/manifest.toml:3661).

   As written, both plans would implement the class, or each would expect the other to. Either way, a driver has no way to set `_BCM` or to hear a 0x86.

   **Correct the ACPI provider and ambient-light items** by naming the publisher of each provider:
   - ACPI: a driver bound through DeviceManager to the video device of finding 1. It asks the ACPI service to evaluate `_BCL`, `_BCM`, `_BQC` and `_DOS` on its own node, and receives that node's notifications. 0196b must then offer that interface.
   - USB monitor control and HID sensors: `driver.xhci`.

   Take the backlight class out of 0196b's list, or make this milestone its consumer, but not both. Name the ambient-light kind and its consumer. List the IDL, manifest, DeviceManager and driver-protocol mappings, and the changes to the catalogue role.

3. **Medium - Brightness keys cannot reach any system-wide handler: consumer-page usages never leave the drivers' console path, and the raw key stream carries only keyboard-page usages to the focused client.**

   A raw [`key-event`](/data/yellow/libersystem/src/idl/input.lsidl:17) is a keyboard-page (0x07) usage, and neither keyboard driver puts a brightness key on it:
   - the USB HID driver sends [only page 0x07 to the raw sink](/data/yellow/libersystem/src/user/drivers/core/src/usb_hid.rs:314). It hands consumer usages to [`keys::feed_key`](/data/yellow/libersystem/src/user/drivers/core/src/usb_hid.rs:324), where the [brightness usages](/data/yellow/libersystem/src/user/drivers/core/src/keys.rs:673) are [reserved](/data/yellow/libersystem/src/user/drivers/core/src/keys.rs:235) and [dropped](/data/yellow/libersystem/src/user/drivers/core/src/keys.rs:442);
   - virtio-input forwards a key [only if it has a keyboard-page usage](/data/yellow/libersystem/src/user/drivers/core/src/virtio_input.rs:293), and [`keycode_hid`](/data/yellow/libersystem/src/user/drivers/core/src/keys.rs:657) has none for codes 224 and 225.

   InputService streams keys [only to the display owner](/data/yellow/libersystem/src/idl/input.lsidl:51), against a focus proof. The ["key handler"](/data/yellow/libersystem/docs/todo/P02M0199.md:20) that is to hold the set grant does not exist. InputService's one precedent for acting on a key before delivery is the [secure-attention chord](/data/yellow/libersystem/src/user/services/core/src/input_service.rs:246).

   Two policy items meet the same wall. The [idle-dimming policy](/data/yellow/libersystem/docs/todo/P02M0199.md:46) needs an activity signal that no service publishes. The [protected session](/data/yellow/libersystem/src/idl/display.lsidl:415) takes the keyboard off the ordinary path.

   **Correct the key-routing and policy items** to state:
   - which component intercepts brightness keys, and what it calls with which grant;
   - how consumer-page usages and virtio codes 224 and 225 reach it, which changes the raw key contract;
   - what the keys do during the protected session;
   - what supplies the idle and activity signal.

4. **Medium - The USB gates name a fixture that cannot play a HID device, and a gadget keyboard that does not exist.**

   [0199d](/data/yellow/libersystem/docs/todo/P02M0199.md:55) plays the monitor-control device and the light sensor over `usb-redir`, "by the fixture P02M0099's USB classes use". That fixture cannot play them:
   - [`usbredir_device.py`](/data/yellow/libersystem/src/harness/usbredir_device.py:582) models only a UAC1 microphone and two DFU targets;
   - it [refuses interrupt receiving](/data/yellow/libersystem/src/harness/usbredir_device.py:505), and a HID interface needs an interrupt IN endpoint, which is what a light sensor reports on.

   P02M0099's HID classes use the gadget instead. The UPS is [`USB_GADGET=ups`](/data/yellow/libersystem/docs/todo/P02M0099.md:6396): a `usb_f_hid` function with [an arbitrary report descriptor](/data/yellow/libersystem/src/harness/usb-gadget.sh:144), and [`ups-sim.py`](/data/yellow/libersystem/src/harness/ups-sim.py:4) answering GET_REPORT and SET_REPORT. The [key gate](/data/yellow/libersystem/docs/todo/P02M0199.md:58) uses "the gadget keyboard", but the gadget has [no keyboard kind](/data/yellow/libersystem/src/harness/usb-gadget.sh:134).

   **Correct the verification items** to use new `usb_f_hid` gadget kinds:
   - a Monitor and VESA Virtual Controls descriptor;
   - a HID-sensor ambient-light descriptor;
   - a Consumer Control keyboard.

   Each needs a far-end process like `ups-sim.py` that records the brightness it is sent and drives the illuminance. Alternatively, add interrupt endpoints and HID models to `usbredir_device.py`, and say so. This is a small change.

5. **Medium - The ACPI video notification values are wrong from 0x88 on.**

   [0199c](/data/yellow/libersystem/docs/todo/P02M0199.md:41) maps 0x88 to cycle, 0x89 to zero and 0x8A to display off. The output-device notifications of ACPI's video extension, as Linux [defines them](https://raw.githubusercontent.com/torvalds/linux/master/include/acpi/video.h), are:

   | value | meaning |
   | --- | --- |
   | 0x85 | cycle brightness |
   | 0x86 | increase brightness |
   | 0x87 | decrease brightness |
   | 0x88 | zero brightness |
   | 0x89 | display off |

   0x8A is none of them. Built as written, the cycle key is ignored and the zero key cycles. The display-off key sets the level to zero: the black panel the [floor](/data/yellow/libersystem/docs/todo/P02M0199.md:44) exists to prevent, requested by firmware rather than by a person. The [gate](/data/yellow/libersystem/docs/todo/P02M0199.md:52) raises only 0x86 or 0x87, so it would not notice.

   **Correct the notification list**, and add a host test over the whole map. This is a small change.

6. **Low - The grant item names a read grant and two holders that do not exist, and leaves the shipping tool without authority to set.**

   The [grant item](/data/yellow/libersystem/docs/todo/P02M0199.md:20) says reading comes with "the ordinary display read grant". None of its parts exist as named:
   - the [`display` capability](/data/yellow/libersystem/src/idl/security.lsidl:29) is not a read grant. It is a surface connection bound to one process and minted through [`display-admin.bind`](/data/yellow/libersystem/src/idl/display.lsidl:442) ([PermissionManager](/data/yellow/libersystem/src/user/services/core/src/permission_manager.rs:751));
   - "the session" in this tree is SessionService, the shell's [job table](/data/yellow/libersystem/src/idl/session.lsidl:1);
   - the key handler is missing (finding 3).

   The [`brightness` tool](/data/yellow/libersystem/docs/todo/P02M0199.md:47) sets levels but holds neither grant. Operator tools receive theirs from a named row, as [`btctl`](/data/yellow/libersystem/src/user/services/core/src/permission_manager.rs:331) does.

   **Correct the grant item** to name:
   - the new capabilities in the [capability enum](/data/yellow/libersystem/src/idl/security.lsidl:12): read and set, split the way [`power-state`](/data/yellow/libersystem/src/idl/security.lsidl:96) and [`power-control`](/data/yellow/libersystem/src/idl/security.lsidl:102) are;
   - their holders;
   - the PermissionManager rows, including the tool's.

Validation was by inspection of the plans, the source, the harness and primary sources: Linux's ACPI video, namespace-scan and backlight-selection code, and the USB Monitor Control Class specification. No plan, source file or git state was modified, and nothing was built or booted.

PLANNER'S RESPONSE ON P02M0199 (2026-09-26T02:47:16Z):

Verified every finding against the tree and the plans it leans on: `display_service.rs` (`output: 0` in the
snapshot, `take_published_display` adopting the first provider, `init_scanout` falling through to the boot
framebuffer), `display.lsidl`, `display-device.lsidl`, `device.lsidl` (closed `provider-kind` set,
`provider-info` named by its publisher's PCI address, no publish operation), `device_manager.rs` (`publish_all`
reached only from committed bindings, the two kind mappings), `system-manifest` (`ProviderKindName`, `wire()`,
the rule that a role's provider must be a declared dependency), `manifest.toml` (DisplayService ephemeral with
`kinds = ["display"]` and no configuration role; ConfigService after StorageService), `usb_hid.rs`, `keys.rs`,
`virtio_input.rs`, `input.lsidl`, `input_service.rs` (the secure-attention reservation before delivery, the
`protected` early return, `record_raw` accepting only three-byte frames), `security.lsidl`,
`permission_manager.rs`, `service_manager.rs` and `capability_names.rs` (how `power-state`/`power-control` and
`btctl` are resolved and granted), `classes.rs` (one class module per USB device), `usbredir_device.py`,
`usb-gadget.sh`, `ups-sim.py`, `qemu-run.sh` (q35 test profile with its default VGA), and P02M0196, P02M0197
and P02M0099 (GOP closure, DDC/AUX row, UPS gadget oracle). QEMU's own key-code table on this machine was
checked read-only: it has no brightness key, so EV_KEY 224/225 cannot be injected into a guest. The
cross-milestone decisions on the companion join, the node-scoped channel, the backlight class's owner and
driver-only publication were applied as given.

1. **ACCEPTED - no output model and no join.** Confirmed: one scanout, `output` always 0, first provider or
   boot framebuffer, no EDID or connector on the device wire. P02M0199a now opens with the OUTPUT MODEL: one
   output, id 0, with a recorded SOURCE - the `display` provider's publisher address, or the PCI function
   decoding the boot framebuffer, which the kernel computes from the framebuffer base and the BARs its own
   scan recorded and returns in `framebuffer_map`'s descriptor (the only kernel addition). The persistent key
   is put where the level lives: a BACKLIGHT's stable key (`acpi:` plus the output node's namespace path;
   `usb:` plus vendor:product:serial or port path; native function plus connector), not the output id. The
   level is stored by a new `brightness_policy` service in ConfigService, because DisplayService must not wait
   for a volume and a role's provider must be a declared dependency. Join rules with recorded reasons:
   `native` (same binding as the display provider), `firmware-adapter` (the ACPI target equals the display
   provider's or the boot framebuffer decoder's function), `edid` (dormant until an output has an EDID; the
   in-tree parser compares the base-block identity), `single-output` fallback. Precedence native > ACPI > EDID
   > single-output, ACPI ties by the internal-panel display id then namespace order; losers are SHADOWED
   (listed, refused a set, never stepped or restored); an UNJOINED backlight is listed, settable by the tool
   and restored, never acted on by keys or policy. On identity: the companion join is P02M0196's; this
   milestone defines the `video-output` match class that P02M0196b's enumeration assigns, and the output's
   identity carries the parent's resolved function whether or not anything binds it. Declined: an
   operator-made association (the tool reaches unjoined backlights by key; the fallback covers the common
   case) and multiple outputs (EXCLUDES).

2. **ACCEPTED - no publisher for the ACPI provider, class claimed twice, no ALS kind or consumer.** Confirmed
   in `device_manager.rs` and `device.lsidl`. P02M0199b names the publishers, all driver bindings:
   `acpi_backlight` bound to the method-only output node, using the node-scoped channel it receives with the
   claim for `_BCL`/`_BCM`/`_BQC` and the node's notifications; `_DOS` is stated - it is the ADAPTER's method,
   so the match row declares one parent-node method the channel admits, evaluated with 0x04 at bind and again
   in the resume step; `acpi_als` bound by `_HID` `ACPI0008`; and driver.xhci (a `hid::fields` map module) for
   USB monitor control and HID ambient-light sensors. The backlight class is this milestone's; P02M0196b keeps
   the mechanism. Kinds `backlight` (consumer DisplayService) and `ambient-light` (consumer the brightness
   policy) are listed with every mapping that must move together - `device.lsidl`,
   `driver_protocol::provider`, `ProviderKindName`/`wire()`, DeviceManager's two functions - and the catalogue
   role changes (DisplayService `["display", "backlight"]`, the policy `["ambient-light"]`, `xhci` provides).

3. **ACCEPTED - keys cannot reach a system handler.** Confirmed in `usb_hid.rs:314`, `keys.rs:235,442,657`,
   `virtio_input.rs:293` and `input_service.rs:246`. P02M0199c now specifies the path: drivers send the
   consumer-page system-key set (0x6F/0x70) as a new five-byte raw frame, virtio-input translating 225/224
   through an inverse of `consumer_keycode`; InputService reserves them before ordinary delivery like the
   secure-attention chord, never lets them into `key-event` (the client contract does not change), and sends
   presses on a new `system-keys` root handed exclusively to DisplayService, which applies them with no grant
   because it owns the brightness (DisplayService already depends on InputService, so ordering is unchanged).
   The keys ACT during the protected session (they reach no application; the person must be able to read the
   protected screen). A keyboard key and a firmware hotkey for one press are coalesced. The idle signal is a
   new InputService `input-activity` root (idle/active edges only), held by the brightness policy.

4. **ACCEPTED - wrong USB fixture, no gadget keyboard.** Confirmed: `usbredir_device.py` models a microphone
   and DFU targets and refuses interrupt receiving; the gadget has no keyboard kind. P02M0199d now uses two new
   `usb_f_hid` gadget kinds with far ends like `ups-sim.py`: `monitor` (Monitor Control with Brightness and EDID
   feature reports, plus an Ambient Light collection in the same interface, played by `monitor-sim.py`) and
   `consumer-keys` (played by `keys-sim.py`). Taken partly: the audit's three kinds became two, because xhci
   binds one class module per device and a monitor with a light sensor is one realistic interface; the key
   run is checked against the ACPI fixture's backlight for the same one-device reason. The usbredir
   alternative is declined in favour of the precedent P02M0099's HID classes already use.

5. **ACCEPTED - notification values.** The output-device map is now 0x85 cycle, 0x86 increase, 0x87 decrease,
   0x88 zero, 0x89 display off. Zero lands on the FLOOR (a firmware key press is not an explicit zero), 0x89
   changes no level and is logged, other values are ignored; a driver whose firmware steps anyway reports a
   firmware change instead of a second step. A host test covers the whole map, and the ACPI gate raises all
   five.

6. **ACCEPTED - grant and holders missing.** Two capabilities appended to `security.lsidl` after the last one
   present, split like `power-state`/`power-control`: `brightness` (read, from DisplayService's `BRIGHTNESS`
   root) and `brightness-control` (set a level and the policy's two settings, from the policy's `CONTROL`
   root), with broker names, ServiceManager resolution and PermissionManager rows: the shipping `brightness`
   tool holds both as `btctl` does; development probes `brightcheck` (both) and `brightread` (read alone).
   DisplayService's own set root has no capability name and is handed to the policy alone; the keys need no
   grant. The nonexistent "display read grant", "session" and "key handler" holders are gone from the plan.

Re-check of the whole plan: every item addressed to an output now rests on the output model and the join;
every provider has a publishing driver; the key, protected-session, activity and persistence paths name the
component, root and grant each uses; the fixtures exist in shape and are named. It is internally consistent
with the coordination decisions (companion join and node-scoped channel referred to P02M0196, the class kept
here, driver-only publication), feasible on today's tree, and ordered: the USB half first, the ACPI half after
P02M0196a/b/d. Owner questions stay recorded as asked when the part starts (the floor, idle dimming, automatic
brightness). Only `docs/todo/P02M0199.md` and this file were edited; no source, test or script was changed and
nothing was built or booted.
