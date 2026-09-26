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


AUDITOR'S RE-AUDIT OF PLAN P02M0199 (2026-09-26T04:01:12Z):

**Rating: 7/10.** The six original findings are answered and most answers hold, but three decisions still conflict with a dependency or with the tree: the backlight's match class, the policy's restart, and the boot-framebuffer decoder.

The complete history was read: the original review of six findings and the planner's response accepting all six. The audited version was never committed, so the planner's described changes were checked against the current plan at commit `0dd5da07`. In the tree I verified DisplayService (one scanout, `output: 0`, the provider and boot-framebuffer paths), the `display`, `display-device`, `device`, `input`, `config` and `security` IDL, the four spellings of the provider kinds, `keys.rs`, `usb_hid.rs`, `virtio_input.rs`, InputService's key path, PermissionManager's grants, ServiceManager's broker and restart code, the manifest and its validator, ConfigService, the kernel's PCI scan and `framebuffer_map`, the xHCI class dispatch, `usb-gadget.sh`, `ups-sim.py`, `test-kernel.sh` and the x86_64 test profile in `qemu-run.sh`. I cross-checked P02M0196, P02M0195, P02M0197 and P02M0099, and primary sources: QEMU v10.0.0 (`acpi-vga.c`, `acpi-build.c`, `qapi/ui.json`), Linux v6.12 (ACPI scan and video, `f_hid`, `g_hid.h`). The corrections for the output model, the publishers and kinds, the key path, the gadget fixtures, the notification map and the grants hold as far as they go; the three items below are what remains.

1. **High - The `video-output` class is "`_DOD` or `_DOS`" here and "`_DOD` and `_DOS`" in P02M0196b, and the ACPI fixture's adapter declares only `_DOS`.**

   This plan defines the class for a method-only device "whose parent is a PCI companion that has `_DOD` or `_DOS`" ([IDENTITY AND MATCH](/data/yellow/libersystem/docs/todo/P02M0199.md:135)). P02M0196b, which builds the rule table the class is assigned from, writes the same row as "a child of a companion that has `_DOD` and `_DOS`" ([P02M0196](/data/yellow/libersystem/docs/todo/P02M0196.md:215)). The ACPI half waits for "P02M0196b's enumeration with the `video-output` class" ([ORDER](/data/yellow/libersystem/docs/todo/P02M0199.md:280)).

   The fixture gives the adapter "an adapter `_DOS` that records its argument there" and no `_DOD` ([fixture](/data/yellow/libersystem/docs/todo/P02M0199.md:270)). QEMU's own node for the VGA function carries `_ADR`, `_S1D`, `_S2D` and `_S3D` and nothing else (https://gitlab.com/qemu-project/qemu/-/blob/v10.0.0/hw/i386/acpi-build.c#L561, https://gitlab.com/qemu-project/qemu/-/blob/v10.0.0/hw/display/acpi-vga.c). Built from P02M0196b's text, the fixture's output never receives the class and `acpi_backlight` never binds. The first ACPI check, "the backlight joined as `firmware-adapter`" ([checks](/data/yellow/libersystem/docs/todo/P02M0199.md:271)), then fails, and so does the key gate, which is checked against that backlight ([key gate](/data/yellow/libersystem/docs/todo/P02M0199.md:273)).

   Built from this plan's text, an adapter with `_DOD` and no `_DOS` matches, yet the bind step only says `_DOS` is "Evaluated at bind with 0x04" ([_DOS item](/data/yellow/libersystem/docs/todo/P02M0199.md:145)); what the driver does when the adapter has none is not said. Linux accepts either method (https://github.com/torvalds/linux/blob/v6.12/drivers/acpi/scan.c#L1302), binds `_DOD` without `_DOS` with a firmware-bug message (https://github.com/torvalds/linux/blob/v6.12/drivers/acpi/acpi_video.c#L1049) and skips `_DOS` when it is absent (https://github.com/torvalds/linux/blob/v6.12/drivers/acpi/acpi_video.c#L683). This continues original findings 1 and 2: identity and publication were referred to P02M0196, and the two plans now spell the referral differently.

   **Correct the IDENTITY AND MATCH item and the ACPI fixture.** State the rule once, as "`_DOD` or `_DOS`", and have P02M0196b's row use the same words. Say that the bind skips the `_DOS` step, and logs it, when the adapter has no `_DOS`. Give the fixture's adapter a `_DOD` naming the output's `_ADR` beside its `_DOS`, so the fixture matches under either wording.

2. **Medium - The brightness policy is declared restart-transparent, but it takes DisplayService's set root as an exclusive client role, which the supervisor can deliver only once.**

   The policy's roles include "`BRIGHTNESS` and `BRIGHTNESSCTL` (the second exclusive)", and the same item says "Restart transparent, state reconstructible" ([roles](/data/yellow/libersystem/docs/todo/P02M0199.md:211), [restart](/data/yellow/libersystem/docs/todo/P02M0199.md:214)).

   In the manifest model an exclusive client role "is not re-creatable at all, because the supervisor gave away the only end it had" ([RoleKind::Client](/data/yellow/libersystem/src/tools/system-manifest/src/lib.rs:892)). The first delivery takes the kept end and closes the supervisor's copy ([bootstrap.rs](/data/yellow/libersystem/src/user/services/core/src/service_manager/bootstrap.rs:180)). A transparent restart re-runs the same delivery ([relaunch_planned](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:1594)) and finds no end; a required role then fails the relaunch ([bootstrap.rs](/data/yellow/libersystem/src/user/services/core/src/service_manager/bootstrap.rs:162)) and the service is left Failed ([service_manager.rs](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:1380)). An optional role would restart the policy with no set channel at all. Today only escalating services hold exclusive roles ([display_service](/data/yellow/libersystem/src/user/services/manifest.toml:3617), [shell](/data/yellow/libersystem/src/user/services/manifest.toml:4369)), and no gate restarts the policy, so this would ship unseen.

   "Handed to one service alone" does not need `exclusive` in this tree. DisplayService's `TRUSTED` root is "handed to AdminService and to nothing else" ([manifest](/data/yellow/libersystem/src/user/services/manifest.toml:3679)), and AdminService, a transparent service ([manifest](/data/yellow/libersystem/src/user/services/manifest.toml:4917)), holds it through a plain client role ([manifest](/data/yellow/libersystem/src/user/services/manifest.toml:4953)). The plan's own "no capability name resolves to it" ([vocabulary](/data/yellow/libersystem/docs/todo/P02M0199.md:93)) already gives the rest of the guarantee. New defect, introduced with the role list written for original findings 3 and 6.

   **Correct the policy's role list.** Take `exclusive` off `BRIGHTNESSCTL`, as AdminService's `TRUSTED` role has it, and keep "alone" as one declared client and no broker name. If exclusivity is wanted, make the policy `escalate` instead - but not both as written.

3. **Medium - The boot-framebuffer decoder rests on "the BARs its own PCI scan recorded", which the kernel does not record for a display function, and ORDER lands it before P02M0196 adds that record.**

   The plan calls the decoder "one kernel addition: the kernel holds both the framebuffer's physical base and the BARs its own PCI scan recorded" ([output model](/data/yellow/libersystem/docs/todo/P02M0199.md:44)). ORDER puts "the contract" in the USB half, which "needs nothing from P02M0196 and lands first" ([ORDER](/data/yellow/libersystem/docs/todo/P02M0199.md:277)).

   In the tree a device row holds one BAR ([device.rs](/data/yellow/libersystem/src/kernel/device.rs:20)), filled only for virtio functions and five resolved families - xHCI, NVMe, AHCI, SDHCI and HDA ([pci/mod.rs](/data/yellow/libersystem/src/kernel/arch/common/pci/mod.rs:43)). Every other function gets a row with `bar_phys: 0` ([device.rs](/data/yellow/libersystem/src/kernel/device.rs:145)), and the scan's own record has no BAR field ([pci/mod.rs](/data/yellow/libersystem/src/kernel/arch/common/pci/mod.rs:401)). That includes the q35 VGA function the ACPI gate relies on ([fixture](/data/yellow/libersystem/docs/todo/P02M0199.md:267)) and a laptop's GPU. Recording every function's BARs is the change P02M0196b already plans: "the boot scan records EVERY function's BARs; today a function outside the resolved families has a row with none" ([P02M0196](/data/yellow/libersystem/docs/todo/P02M0196.md:165)). So the "one" addition is two, and one of them belongs to a milestone this half says it does not need. Two implementers would build different things: one records BARs inside 0199a, one reads them on demand in `framebuffer_map`, one waits for P02M0196b.

   The same sentence calls ConsoleService the descriptor's "other reader" ([output model](/data/yellow/libersystem/docs/todo/P02M0199.md:46)). It no longer reads `abi::Framebuffer` ([console_service.rs](/data/yellow/libersystem/src/user/services/core/src/console_service.rs:185)); DisplayService's call is the only one ([display_service.rs](/data/yellow/libersystem/src/user/services/core/src/display_service.rs:1856)). This continues original finding 1, whose correction asked for "the device that decodes the boot framebuffer".

   **Correct the output-model item and ORDER.** Say that the decoder needs every function's memory BARs recorded at the boot scan. Either make that recording part of 0199a as the same change P02M0196b names, landed by whichever comes first, or move the decoder into the ACPI half, since only the `firmware-adapter` join uses it. Drop the ConsoleService parenthetical.

Validation: this re-audit was done by inspection - the plan, its complete audit history, the neighbouring plans P02M0196, P02M0195, P02M0197 and P02M0099, the source, manifest and harness files cited above at commit `0dd5da07`, and primary sources fetched read-only (QEMU v10.0.0, Linux v6.12). Every cited line was re-read. QEMU was used only for `--version` and `-device help`. No plan, source or audit file was modified, no git state was changed, and nothing was built or booted.


PLANNER'S RESPONSE ON P02M0199 (2026-09-26T05:34:17Z):

I checked every finding against the plan, its complete audit history and the tree. On the service side I read
`RoleKind::Client` and the exclusivity rules in `system-manifest`, `deliver_roles` in ServiceManager's
`bootstrap.rs`, and `restart_service`, `start_stopped_service`, `relaunch_service` and `relaunch_planned` in
`service_manager.rs`. I also read the DisplayService, InputService, AdminService and PowerService rows in
`manifest.toml`, and the power-service gate's `stop`/`start` restart in `check-power-service.sh`. On the
display side I read `DeviceEntry` in the kernel's `device.rs`, the `RESOURCED` table, the inventory loop and
`PciDevice` in `pci/mod.rs`, `sys_framebuffer_map`, `abi::Framebuffer` and its layout pin in the ABI tests,
`console_service.rs` and DisplayService's `framebuffer_map` call. Primary sources were Linux's
`drivers/acpi/scan.c` (`acpi_is_video_device`) and `acpi_video.c` (`acpi_video_bus_DOS`, and the firmware-bug
path for `_DOD` without `_DOS`), and QEMU's `hw/display/acpi-vga.c`. The neighbouring plans were P02M0196 (its
`video-output` row, its BAR recording and the node-scoped channel's parent-method allowance), P02M0197 (the
driver contract's RESUME and the shared `input-activity` root), P02M0195 (no reference to this milestone) and
P02M0099 (the GOP closure and the DDC/AUX row). Summary: all three findings are accepted and none is rejected.

1. **ACCEPTED - `video-output` is spelled two ways, and the bind and the fixture assume `_DOS`.** Confirmed.
   This plan's class rule reads "`_DOD` or `_DOS`", while P02M0196b's row reads "`_DOD` and `_DOS`". The
   fixture's adapter declared only `_DOS`, and QEMU's own node for the VGA function carries `_ADR` and
   `_S1D`/`_S2D`/`_S3D` and nothing else. Linux accepts either method, skips `_DOS` when it is absent, and
   binds an adapter that has `_DOD` without `_DOS`, logging a firmware bug. The bind step never said what
   happens without `_DOS`. Plan changes:
   - IDENTITY AND MATCH keeps the rule stated once, as "`_DOD` or `_DOS`". P02M0196b's row is changed to the
     same words by its own plan.
   - The `_DOS` bullet now ends with this case. AN ADAPTER WITH `_DOD` AND NO `_DOS` still matches the class,
     as some firmware ships. The channel answers that the object does not exist. The driver then skips the
     `_DOS` step - at bind, with one log line, and at every resume - and binds and publishes as usual.
   - The ACPI fixture's adapter now declares a `_DOD` naming the output's `_ADR` beside its `_DOS`, as real
     firmware declares both.
   - HOST SUITES gain the bind against an adapter with no `_DOS`, the step skipped and logged once.
   - EXCLUDES now says that `_DOD`'s presence only counts toward the `video-output` class and that it is never
     evaluated. So the fixture's `_DOD` does not contradict the exclusion of output switching.

2. **ACCEPTED - the brightness policy is transparent but held an exclusive client role.** Confirmed in the
   tree:
   - An exclusive client role is documented as "not re-creatable at all".
   - `deliver_roles` duplicates the end, then takes it from the supervisor and closes the supervisor's copy.
   - A later delivery of a required role finds no end and fails.
   - Both the crash restart and a deliberate `start` reach `relaunch_planned`, which re-runs that delivery. A
     failure there leaves the service Failed. A new transparent service takes that path: `check-bootstrap-plan`
     requires every transparent service to be relaunchable, and apart from three older hand-written bootstraps
     that means being named in `plan_relaunchable`.
   - AdminService is transparent and holds DisplayService's `TRUSTED` root through a plain client role.
   - DisplayService's own exclusive `FOCUS` and `KILL` are sound only because DisplayService escalates and is
     never relaunched.
   Plan changes:
   - The policy's roles now read "plain clients of DisplayService's `BRIGHTNESS` and `BRIGHTNESSCTL`", and
     "(the second exclusive)" is gone.
   - A new sentence says why: RESTART TRANSPARENT, STATE RECONSTRUCTIBLE, so NO ROLE IS `exclusive`, because
     an exclusive client end is handed over at the first delivery and a relaunch would leave the service
     failed.
   - `BRIGHTNESSCTL` stays the policy's alone the way `TRUSTED` stays AdminService's: the manifest declares no
     other client, and no capability name resolves to it. The vocabulary item in 0199a now says the same: "The
     manifest declares one client of this root, the brightness policy (0199c), and no capability name resolves
     to it".
   - The policy item also says what a restart loses. Settings and stored levels are ConfigService's and
     everything else is re-read, so only what was in progress is lost: a dim, whose level stays until a key or
     a set changes it, and a pause of automatic brightness, which resumes.
   - The `SYSKEYS` item now gives the reason its exclusive role is sound: DisplayService escalates and is
     never relaunched, as its `FOCUS` and `KILL` already assume.
   - Since no gate restarted the policy, the USB `monitor` run gains one check. `brightness_policy` is stopped
     and started, as the power-service gate restarts PowerService. It must come back with every role delivered
     and its stored settings, and forward a set again. That path re-runs role delivery, so an exclusive role
     would fail the check.
   The `escalate` alternative is declined: a policy that escalates stays down after its first crash until a
   reboot, and then nothing is restored or dimmed.

3. **ACCEPTED - the boot-framebuffer decoder rests on BARs the kernel does not record, and the ConsoleService
   parenthetical is wrong.** Confirmed in the tree:
   - A kernel device row holds one BAR. It is filled only for virtio functions and the five `RESOURCED`
     families: xHCI, NVMe, AHCI, SDHCI and HDA.
   - The inventory loop appends every other function with `bar_phys: 0`, and `PciDevice` has no BAR field. So
     q35's VGA function and a laptop's GPU have none.
   - P02M0196b already plans for the boot scan to record every function's BARs.
   - `sys_framebuffer_map` is once-only, and DisplayService is its only caller. ConsoleService now draws on a
     surface from the display protocol and no longer reads `abi::Framebuffer`.
   Plan changes, taking the second of the two offered resolutions:
   - The OUTPUTS item no longer carries the decoder. It says the `boot-framebuffer` source's function is NONE
     until the ACPI half lands the kernel's decoder, because the `firmware-adapter` join is its only reader.
     It stays none for ramfb and a device-tree simple framebuffer. The ConsoleService parenthetical is
     dropped.
   - A new first item of 0199b, THE BOOT FRAMEBUFFER'S DECODER, holds the decoder. The kernel finds the PCI
     function whose memory BAR contains the boot framebuffer's base and returns it in `framebuffer_map`'s
     descriptor, as a decoder address and a present flag appended to `abi::Framebuffer`. DisplayService, the
     call's one caller, puts it in the source.
   - The same item says the decoder NEEDS EVERY FUNCTION'S MEMORY BARS RECORDED AT THE BOOT SCAN, which the
     kernel does not do today. That record is the change P02M0196b makes for its BAR and window checks: ONE
     change, landed by whichever of the two comes first and used by the other.
   - WHAT IT RESTS ON names that record.
   - ORDER moves the decoder into the ACPI half. Until then the USB half's boot-framebuffer source names no
     function. The BAR record is the one piece of P02M0196b the ACPI half may land itself, if it gets there
     first.
   - The `firmware-adapter` join now points at 0199b.
   - The ACPI run now checks the decoder directly, before the join: output 0's source is the boot framebuffer
     decoded by the VGA function.

Re-check of the whole plan: I re-read it end to end against the tree and the neighbouring plans, confirming
again the claims my edits leaned on. `keys.rs` reserves KEY_BRIGHTNESSDOWN/UP (224/225), and
`consumer_keycode` maps 0x6F/0x70. The surface configuration carries `output: u32`. ConfigService keys are
free-form, with only `device.policy.` reserved, so `display.brightness.level.<stable key>` is writable.
`power-state` and `power-control` resolve as the grant item says. Two consistency additions came out of the
re-check:
- (a) The TWO PROVIDER KINDS item now says that the policy's catalogue role takes two of the 32
  catalogue-client slots the manifest checks, because it is a transparent service's minting role. Today 27 are
  demanded: 17 minting roles, 9 of them transparent, plus DeviceManager's own. Whichever milestone first
  pushes the demand past 32 raises the bound to 64 in the same change, both in `system-manifest` and in
  DeviceManager's table, which keeps its own constant. This is the decision P02M0201 carries, stated here
  because this plan adds such a role.
- (b) The ACTIVITY item now names P02M0197's sleep policy, in the power-state service, as the root's second
  holder, and says whichever milestone lands first builds the root, as P02M0197 already states.
Two paragraphs were re-wrapped, every line is ASCII within the file's existing width, and nothing unrelated
was removed. The result:
- The plan is consistent with P02M0196 once its row reads "`_DOD` or `_DOS`", its own plan's change. It is
  also consistent with P02M0196's node-scoped channel and parent `_DOS` allowance and with its BAR record.
- It is consistent with P02M0197's RESUME step and shared activity root, and it does not overlap P02M0195.
- The owner's decisions stand: created under P02M0196's approval; the floor and the policy defaults are asked
  when the part starts; no native graphics path.
- The plan is complete and internally consistent at the level of decisions, and each part is ready for its own
  written plan.

Only `docs/todo/P02M0199.md` was edited and this response appended; no source, test, script or other plan was
changed, and nothing was built or booted.


AUDITOR'S RE-AUDIT OF PLAN P02M0199 (2026-09-26T15:50:59Z):

**Rating: 8/10.** The three findings of the last re-audit are corrected, both in this plan and in P02M0196, and the facts about the tree that the corrections rest on hold. One gate is left whose result depends on choices the plan does not make: the ACPI run checks a restore in its second boot while automatic brightness is still on. And the ambient-light driver has no step in P02M0197's suspend exchange.

The complete history was read: the review of six findings, the planner's response, the re-audit of 2026-09-26T04:01:12Z (three findings, 7/10), and the planner's response of 05:34:17Z, which accepts all three. I also read `git diff` of the plan against HEAD, whose copy is the one the last re-audit cited, and the whole plan in the working tree. In the tree I checked:

- ServiceManager's restart path: `restart_service`, `start_stopped_service`, `relaunch_service`, `plan_relaunchable` and `relaunch_planned` in `service_manager.rs`, `deliver_roles` in `service_manager/bootstrap.rs`, and `check-bootstrap-plan.py`;
- in `system-manifest`: `RoleKind::Client`, the catalogue-slot sum and the exclusive-client rule;
- the manifest rows of DisplayService, InputService, PowerService, AdminService, ConfigService and `xhci`, and the stop/start restart in `check-power-service.sh`;
- the kernel's `DeviceEntry`, `RESOURCED` and `PciDevice`, `sys_framebuffer_map`, and `abi::Framebuffer` beside the loader's `bootproto::Framebuffer`;
- on the input side: `keys.rs`, `usb_hid.rs`, `virtio_input.rs`, `hid.rs`, the class probe in `classes.rs` and InputService's `record_key`;
- on the service side: ConfigService's key rules, PermissionManager's rows and its re-resolution of grants, and `device.lsidl`'s kinds and `subscribe`;
- in the harness: the x86_64 test profile in `qemu-run.sh`, `usb-gadget.sh`, `ups-sim.py` and `test-kernel.sh`.

Sibling plans were read in the working tree:
- P02M0196, whole;
- P02M0197's driver contract and its sleep policy;
- the catalogue-bound items of P02M0200 and P02M0201;
- P02M0195 and P02M0099, checked for overlap, and there is none.

Correctly resolved, with the evidence re-checked:
- **Previous finding 1.** The class now reads "`_DOD` or `_DOS`" both [here](/data/yellow/libersystem/docs/todo/P02M0199.md:147) and in [P02M0196b's row](/data/yellow/libersystem/docs/todo/P02M0196.md:266). An adapter without `_DOS` [still binds, with the step skipped and logged](/data/yellow/libersystem/docs/todo/P02M0199.md:163). The fixture's adapter [declares a `_DOD`](/data/yellow/libersystem/docs/todo/P02M0199.md:295). A [host case](/data/yellow/libersystem/docs/todo/P02M0199.md:264) covers the bind, and [EXCLUDES](/data/yellow/libersystem/docs/todo/P02M0199.md:313) says `_DOD` is never evaluated.
- **Previous finding 2.** The policy's roles are now [plain clients](/data/yellow/libersystem/docs/todo/P02M0199.md:227). `deliver_roles` [duplicates](/data/yellow/libersystem/src/user/services/core/src/service_manager/bootstrap.rs:175) such a role again at every relaunch, and gives up the kept end [only for an exclusive one](/data/yellow/libersystem/src/user/services/core/src/service_manager/bootstrap.rs:179). The new [restart check](/data/yellow/libersystem/docs/todo/P02M0199.md:285) can fail:
  - [`start_stopped_service`](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:1450) reaches [`relaunch_planned`](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:1571), and that fails on an exclusive role;
  - `start` is also refused for a transparent service missing from [`plan_relaunchable`](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:1561), which [`check-bootstrap-plan`](/data/yellow/libersystem/src/tools/check-bootstrap-plan.py:139) enforces anyway.

  One imprecision in the response changes nothing: a failed deliberate `start` leaves the service Stopped, not Failed. The `escalate` alternative was offered as optional, so declining it is justified.
- **Previous finding 3.** The decoder is now [0199b's](/data/yellow/libersystem/docs/todo/P02M0199.md:135), the output's function [stays none until it lands](/data/yellow/libersystem/docs/todo/P02M0199.md:46), [ORDER](/data/yellow/libersystem/docs/todo/P02M0199.md:305) moves it, and the ConsoleService parenthetical is gone. In the tree:
  - [`sys_framebuffer_map`](/data/yellow/libersystem/src/kernel/syscall/mod.rs:878) is once-only, and [DisplayService](/data/yellow/libersystem/src/user/services/core/src/display_service.rs:1856) is its only caller;
  - the loader hands over [`bootproto::Framebuffer`](/data/yellow/libersystem/src/boot/protocol/src/lib.rs:127), not `abi::Framebuffer`, so appending to the latter changes no boot handoff.
- **The planner's own additions.**
  - The catalogue demand is [27 today](/data/yellow/libersystem/src/tools/system-manifest/src/lib.rs:1499): 17 minting roles, 9 of them transparent, and DeviceManager's own. That is against a [bound of 32](/data/yellow/libersystem/src/tools/system-manifest/src/lib.rs:217). The policy's role makes it 29, and [P02M0201](/data/yellow/libersystem/docs/todo/P02M0201.md:184) carries the same rule for raising the bound.
  - The `input-activity` root and its two holders match [P02M0197](/data/yellow/libersystem/docs/todo/P02M0197.md:297).

1. **Medium - The ACPI run checks the restore in a second boot that inherits automatic brightness from the first, and the plan does not decide whether a restore or automatic brightness sets the level, so the check passes or fails by implementation choice.**

   The sequence:
   - The first boot [turns automatic brightness on](/data/yellow/libersystem/docs/todo/P02M0199.md:300) as its last check.
   - The [second boot, on the same volume](/data/yellow/libersystem/docs/todo/P02M0199.md:301), then checks that "the level stored in the first is restored".
   - The setting [is stored in ConfigService](/data/yellow/libersystem/docs/todo/P02M0199.md:254), and nothing in the run turns it off.
   - The fixture's [`ACPI0008`](/data/yellow/libersystem/docs/todo/P02M0199.md:296) is in the SSDT at the second boot too.

   The two rules then compete:
   - A restore applies only to a backlight ["whose level nothing has set since it appeared"](/data/yellow/libersystem/docs/todo/P02M0199.md:244).
   - [Automatic brightness](/data/yellow/libersystem/docs/todo/P02M0199.md:251) makes the active joined backlight follow the illuminance, and its sets are [never stored](/data/yellow/libersystem/docs/todo/P02M0199.md:242).
   - If the policy's first automatic set comes first, the restore rule excludes the restore.
   - If the restore comes first, the next reading replaces it. That holds unless the policy's own restore counts as the ["set" that pauses automatic brightness](/data/yellow/libersystem/docs/todo/P02M0199.md:253), which the plan does not say.
   - Whether a reading arrives at all depends on whether the sensor's [`events` stream](/data/yellow/libersystem/docs/todo/P02M0199.md:107) starts with the current illuminance, which is also unsaid. `ambient-light` has no `get`.

   So the second boot ends at the stored level only if the curve maps the region's illuminance to that level by chance, or under choices the plan leaves open.

   This is a new finding. The sequence was already in the version the last re-audit reviewed.

   **Correct the ACPI verification item**: turn automatic brightness off before the second boot, or move the automatic-brightness check into the second boot, after the restore check. Also set `idle off` at the start of both runs. Neither run sets the [idle timeout](/data/yellow/libersystem/docs/todo/P02M0199.md:249), and a dim inside a run would move the levels its checks compare, so their results would otherwise depend on the default the owner chooses.

2. **Low - The `acpi_als` driver this milestone adds has no step in P02M0197's suspend exchange, and P02M0197 refuses every sleep while a binding without one is Online.**

   - P02M0197 refuses a sleep ["while a binding with no `suspend-deadline` is Online"](/data/yellow/libersystem/docs/todo/P02M0197.md:140). Its part adds the exchange only to [the drivers the image ships when it lands](/data/yellow/libersystem/docs/todo/P02M0197.md:143).
   - `acpi_backlight` has [a RESUME step of P02M0197's driver contract](/data/yellow/libersystem/docs/todo/P02M0199.md:162). [`acpi_als`](/data/yellow/libersystem/docs/todo/P02M0199.md:182) has none.
   - Neither driver says who implements the exchange if this milestone lands after P02M0197. [P02M0190](/data/yellow/libersystem/docs/todo/P02M0190.md:90) and [P02M0201](/data/yellow/libersystem/docs/todo/P02M0201.md:157) say it for their drivers: whichever of the two milestones lands second.
   - On a laptop whose firmware describes an `ACPI0008`, the sensor's binding would then refuse every sleep.

   This is a new finding. The same gap is reported for P02M0198's fan driver and for P02M0202's two drivers in their re-audits of this date.

   **Correct the ACPI half** : both drivers declare a `suspend-deadline` and implement P02M0197's exchange, carried by whichever of the two milestones lands second.

Validation: this re-audit was done by read-only inspection. It covered the plan, its diff against HEAD, its complete audit history, and the sibling plans P02M0196, P02M0197, P02M0200, P02M0201, P02M0195 and P02M0099 in the working tree. It also covered the source, manifest and harness files listed above, and Linux v6.12's `f_hid.c`, fetched read-only for the gadget's GET_REPORT handling. The catalogue demand was computed by reading the manifest with Python's `tomllib`. Every cited line was re-read. No plan, source or audit file was modified, no git state was changed, and nothing was built, tested, benchmarked or booted.

PLANNER'S RESPONSE ON P02M0199 (2026-09-26T17:12:45Z):

Verified read-only: the ACPI verification item's sequence (automatic brightness turned on last in the first boot, the restore checked in a second boot on the same volume), the policy's restore rule (only a backlight nothing has set since it appeared), automatic brightness's sets never stored, its pause on a key or a set, the settings stored in ConfigService, `ambient-light`'s interface (`describe` and `events`, no `get`), and P02M0197's refusal of an Online binding without `suspend-deadline`. Summary: two findings, both accepted.

1. **ACCEPTED - the second boot's restore competed with automatic brightness inherited from the first.** Of the two fixes offered I took the first, turning automatic brightness off before the second boot, because it keeps the automatic-brightness check in the boot that sets up its illuminance. Plan changes:
   - The ACPI run now begins with `idle off`, stored, so no dim in either boot moves a level a check compares; after the automatic-brightness check, automatic brightness is turned off and a level set and held past the two-second settle, so it is the one stored; the second boot - the region at the firmware default, automatic brightness and the idle timeout still off from the stored settings, so nothing competes with the restore - checks that that level is restored.
   - The USB `monitor` run's checks also start after `brightcheck` has set `idle off`, for the same reason (the finding's "both runs").
   - The device-side contract now states what the finding noted was unsaid: `ambient-light`'s `events` stream opens with the current reading, so a consumer never waits for a change to learn the light.

2. **ACCEPTED - `acpi_als` had no step in P02M0197's exchange.** Plan changes: a new item in the ACPI half, "ACROSS A SLEEP", covering all three pieces this milestone adds to drivers, carried by whichever of P02M0197 and this milestone lands second: `acpi_backlight` and `acpi_als` each declare a `suspend-deadline`; `acpi_backlight` finishes the method in hand and evaluates nothing until `RESUME`, whose `_DOS` and `_BCM` step was already written; `acpi_als` stops its `_ALP` polling at `SUSPEND` and at `RESUME` evaluates `_ALI` again, publishes it and polls again. The xhci class module is included because the same gap exists there: its devices take part in the xHCI driver's suspend of the USB devices bound inside it, and on their resume a sensor's reporting-state and power-state properties are set again by SET_REPORT - a device reset clears them - and a monitor's brightness is read again. The verification adds "A SLEEP", carried the same way: a suspend to idle and an S3 cycle answered by both drivers, and after the harness puts the firmware default back in the region's level and clears its `_DOS` record during S3, `_DOS` saw 0x04 again and the level set before the sleep is back.

Coordinated change: P02M0197's "NO DRIVER IS SKIPPED" names `acpi_backlight`, `acpi_als` and this xhci class module among the drivers that carry the exchange themselves.

Re-check of the whole plan: the two verification runs now depend on no default the owner has yet to choose, and the restore check has a single possible winner; every driver piece this milestone adds meets P02M0197's contract; the ORDER still places the ACPI half, its sleep item among it, after P02M0196's prerequisites. The file is ASCII, no line exceeds 112 columns, it cites no audit, and the `Status:` line is kept.

Edited `docs/todo/P02M0199.md` (and P02M0197's list of drivers); no source was changed, and nothing was built or booted.
