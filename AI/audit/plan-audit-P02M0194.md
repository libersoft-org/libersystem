AUDITOR'S REVIEW OF PLAN P02M0194 (2026-09-25T22:52:40Z):

**Rating: 3/10.** The scope is clear and the account of P02M0180 is accurate, but the plan depends on four things the architecture does not have and the plan does not specify: a way to deliver Bluetooth audio, network, serial and multi-device input; a pairing confirmation; an independent oracle; and the transport hand-off.

Reviewed [the plan](/data/yellow/libersystem/docs/todo/P02M0194.md) at commit `07371c44af82a11c1d275b0cbd832899f712e62a` against:

- the completed [Bluetooth service milestone](/data/yellow/libersystem/docs/todo/P02M0180.md);
- P02M0099's USB Bluetooth transport, its SCO plan and its closed GOP item;
- [P02M0188](/data/yellow/libersystem/docs/todo/P02M0188.md);
- the Bluetooth, audio, network, device, input and admin IDL;
- the Bluetooth, input, audio, network, admin and permission services;
- DeviceManager's publication path, the in-guest fixture and the harness emulators.

Line numbers in P02M0099 and P02M0196 are from the working tree, which has uncommitted edits to both files. The owner has decided that the breadth (every desktop profile on both radios) is the requirement, and nothing below questions it. The findings concern the mechanisms, authorities and oracles the parts depend on, not the expected absence of the feature code.

1. **High - BluetoothService has no way to deliver audio, a network link or a serial port to the services that own them, and its only delivery path carries one mouse.**

   The plan asks the service for four deliveries:
   - an output and input that "appear to AudioService" ([d](/data/yellow/libersystem/docs/todo/P02M0194.md:64), [e](/data/yellow/libersystem/docs/todo/P02M0194.md:73), [g](/data/yellow/libersystem/docs/todo/P02M0194.md:88));
   - SPP "published as a serial byte stream like every other serial port here" ([f](/data/yellow/libersystem/docs/todo/P02M0194.md:79));
   - PAN "published to NetworkService as an interface" ([f](/data/yellow/libersystem/docs/todo/P02M0194.md:82));
   - keyboards, gamepads and arbitrary report maps "into InputService's key, pointer and gamepad streams" ([c](/data/yellow/libersystem/docs/todo/P02M0194.md:54)).

   **Why the catalogue cannot be used.** Each of those consumers finds its devices in the provider catalogue. A catalogue publication is keyed to a driver binding: [`publish_all(binding, ...)`](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:2762), where the binding is [a PCI function and a claim generation](/data/yellow/libersystem/src/idl/device.lsidl:224). P02M0099 records that ["there is no path by which a program with no device publishes a provider at all"](/data/yellow/libersystem/docs/todo/P02M0099.md:2835). That path was [moved whole to a native graphics driver](/data/yellow/libersystem/docs/todo/P02M0099.md:2941) that no milestone orders. P02M0180 also ruled the shortcut out: ["Do not publish a pretend driver provider from the Bluetooth userspace service"](/data/yellow/libersystem/docs/todo/P02M0180.md:213).

   **The one existing path.** InputService pulls from the Bluetooth service. It resolves `bluetooth-profile` [by name through the broker](/data/yellow/libersystem/src/user/services/core/src/input_service.rs:527) and holds [one profile connection and one stream](/data/yellow/libersystem/src/user/services/core/src/input_service.rs:539). That stream is opened on the [first enabled peer with `open-mouse`](/data/yellow/libersystem/src/user/services/core/src/input_service.rs:590), which is the only verb the [profile authority](/data/yellow/libersystem/src/idl/bluetooth.lsidl:199) has.

   **No equivalent exists for the other consumers:**
   - **AudioService** [subscribes to `audio` providers and nothing else](/data/yellow/libersystem/src/user/services/core/src/audio_engine.rs:704).
   - **NetworkService.** Its only link path that needs no driver is [`network-link-admin`](/data/yellow/libersystem/src/idl/network.lsidl:512), and it cannot carry PAN:
     - it is minted for [ModemService alone](/data/yellow/libersystem/src/user/services/manifest.toml:3912);
     - it carries raw IP with ["no Ethernet header, no ARP and no DHCP"](/data/yellow/libersystem/src/idl/network.lsidl:492);
     - it names each link by a [DeviceManager publication](/data/yellow/libersystem/src/idl/network.lsidl:477).

     BNEP instead carries Ethernet frames ([BNEP 1.0, 2.2](https://www.bluetooth.com/wp-content/uploads/Files/Specification/HTML/BNEP_v1.0/out/en/index-en.html)).
   - **Serial.** "Every other serial port here" is a `console-bytes` publication, and its only consumer is the [development agent](/data/yellow/libersystem/src/user/services/core/src/dev_agent.rs:122). No application can open one.
   - **Gamepads.** [P02M0192](/data/yellow/libersystem/docs/todo/P02M0192.md:34) also plans to deliver them to InputService as a published provider.

   **Why it matters.** Parts c, d, e, f and g depend on contracts, roles and consumer changes the plan never names. The implementer must either build a publication path for programs with no device (a containment change P02M0180 refused) or improvise four service-to-service contracts.

   **Correct parts c, d, e, f and g.** For each consumer, name the contract the Bluetooth service serves, who holds it and how they reach it (a role or the broker, including the reverse-dependency consequence that made InputService use the broker), its restart and loss behaviour, and the change on the consumer's side:
   - **Audio:** a Bluetooth audio authority held by AudioService alone, as `bluetooth-profile` is held by InputService.
   - **Network:** an Ethernet-framed link NetworkService can install, or DHCP inside Bluetooth with the raw-IP install. Include the rule for replacing the current uplink, since the service runs [one uplink at a time](/data/yellow/libersystem/src/user/services/logic/src/uplink.rs:4).
   - **Serial:** an application-facing serial grant for SPP, or drop "like every other serial port".
   - **Input:** `bluetooth-profile` and InputService's slot grown to several peers, to keys (never into the trusted sink) and to P02M0192's gamepad record.

   If the plan prefers a publication path for programs with no device, it must own that mechanism and reverse P02M0180's rule explicitly.

2. **High - AudioService has none of the device, format, latency, voice or control model that parts d, e and g assume.**

   **What AudioService has.** It drives exactly one device:
   - [one `snd` channel](/data/yellow/libersystem/src/user/services/core/src/audio_engine.rs:171), taken from [the first live publication when it holds none](/data/yellow/libersystem/src/user/services/core/src/audio_engine.rs:805);
   - [one recorder at a time](/data/yellow/libersystem/src/user/services/core/src/audio_engine.rs:37);
   - everything [converted to the hardware's fixed 48 kHz stereo](/data/yellow/libersystem/src/idl/audio.lsidl:11);
   - a client contract of only `beep`, `open-stream` and [`open-capture`](/data/yellow/libersystem/src/idl/audio.lsidl:16).

   The manifest says so directly: ["the service drives one device"](/data/yellow/libersystem/src/user/services/manifest.toml:3106). There is no device list, selection, hot-plug switching, latency, volume or duplex device.

   **What the plan requires without itemizing it:**
   - an output "with its latency stated", and AVRCP "absolute volume both ways" ([d](/data/yellow/libersystem/docs/todo/P02M0194.md:64));
   - a "voice device - a microphone and a speaker together" to which "a call's audio is routed" ([e](/data/yellow/libersystem/docs/todo/P02M0194.md:73));
   - earbuds that "play, and their microphones record, through AudioService like any other device" ([g](/data/yellow/libersystem/docs/todo/P02M0194.md:88)).

   **Two further gaps.**
   - **Nothing owns "a call".** No IDL in the tree has a call, telephony or media-control concept. The HFP Audio Gateway must still report call state and answer the headset's call and button commands.
   - **The A2DP sink is an input.** A phone's music would reach the speakers only if some program records it and plays it back. It would also be readable by every holder of `audio-capture`.

   **Why it matters.** Without these decisions, "a Bluetooth headset works" has no testable meaning. All of the following are left to the implementer, inside a service built for one device:
   - which output a playing stream moves to when headphones connect;
   - what an application sees when they disconnect;
   - what opens and closes the SCO link;
   - what rate a voice stream is converted to;
   - who receives the headset's play and pause.

   **Correct parts d, e and g** with an AudioService item that specifies:
   - a device inventory and a routing rule: the default output, and what happens to open streams when a device arrives or leaves;
   - per-device formats and conversion: SBC's and LC3's rates, and 8 or 16 kHz mono voice;
   - latency reporting;
   - volume carried to and from AVRCP, HFP and LE volume control;
   - a voice device whose opening brings SCO up, or a call-control contract if calls are meant, and what the Audio Gateway reports when there is no call;
   - where AVRCP and media-control commands are delivered;
   - an A2DP-sink route to an output instead of an input;
   - how a device with no hardware clock at the host is paced, citing the [owner's underrun choice](/data/yellow/libersystem/docs/todo/P02M0099.md:6193) explicitly.

3. **High - The P02M0188 protected screen cannot carry a pairing confirmation, and relying on it makes pairing impossible on many machines.**

   **Wrong shape.** AdminService confirms one requester-initiated operation that an [`admin-executor` provider prepared](/data/yellow/libersystem/src/idl/admin.lsidl:118). Executors are ["drivers DeviceManager bound"](/data/yellow/libersystem/src/user/services/core/src/admin_service.rs:4), and ["nothing a client holds can publish one"](/data/yellow/libersystem/docs/todo/P02M0188.md:304). The actions come from a [closed set: firmware download and a test probe](/data/yellow/libersystem/src/idl/admin.lsidl:33). A pairing confirmation has the opposite shape. The controller produces a value in the middle of the protocol, and the Bluetooth service must show it and get an answer. That service is neither a launched requester nor a driver. An incoming pairing or OBEX push has no requester at all.

   **Missing models.** The surface cannot handle every model the plan names ([a](/data/yellow/libersystem/docs/todo/P02M0194.md:32)). It was built without ["secret entry"](/data/yellow/libersystem/docs/todo/P02M0188.md:11) and [approves only with Enter](/data/yellow/libersystem/docs/todo/P02M0188.md:86). Passkey Entry, where this host types the number, and legacy PIN pairing therefore have no input path.

   **Timing.** The person must first press Ctrl+Alt+F12 and then decide within a prompt that [waits 60 seconds](/data/yellow/libersystem/src/user/services/logic/src/admin_broker.rs:34). LE pairing has already failed by then if the Security Manager Timer ["reaches 30 seconds"](https://www.bluetooth.com/wp-content/uploads/Files/Specification/HTML/Core-54/out/en/host/security-manager-specification.html) (Core 5.4, Vol 3 Part H, 3.4).

   **Availability.** The protected path exists only with a trusted keyboard:
   - the trusted sink goes to [`virtio_input` and the xHCI driver alone](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:4157);
   - ["no Bluetooth input"](/data/yellow/libersystem/docs/THREAT_MODEL.md:178) can feed it;
   - ["a boot without that path ... declines every request"](/data/yellow/libersystem/docs/THREAT_MODEL.md:188).

   A desktop whose only keyboard is Bluetooth could never pair anything, not even a replacement keyboard, although part c exists to support that keyboard. The same holds for any machine whose keyboard is not USB or virtio.

   **Inconsistent across radios.** BR/EDR models go to the protected screen. Part b's ["every LE pairing model"](/data/yellow/libersystem/docs/todo/P02M0194.md:46) names no confirmation surface, and P02M0180's LE Just Works is authorized through `btctl`.

   **Correct the pairing items in parts a and b and the OBEX item in part f.** Specify the confirmation flow as a design of its own:
   - a prompt that the service originates, carrying a six-digit value or a displayed passkey, with keypress notifications;
   - a deadline derived from the protocol timer instead of 60 seconds;
   - the authorization record it leaves when there is no requester;
   - where it lives: an AdminService extension (new action kinds and an originator that is not an executor) or a separate component.

   Also state:
   - what happens on machines with no trusted path and for a first Bluetooth keyboard, for example operator confirmation through `btctl` reported as weaker, as P02M0180 already does;
   - which models are excluded for lack of secret entry, or where that entry happens;
   - one rule applied to both radios.

4. **High - The peer simulator is a second implementation of every profile by the same team, and it cannot place the USB SCO and ISO path under its peers.**

   **The oracle is not independent.** The plan's oracle for every part is the in-guest fixture, grown to play a keyboard, a headset, a phone, a serial device and an LE Audio earbud, "each speaking the real protocols byte for byte" ([h](/data/yellow/libersystem/docs/todo/P02M0194.md:97)). Real hardware is never an oracle.

   For SMP, P02M0180 made a same-team peer meaningful by holding both implementations to [the specification's published sample data](/data/yellow/libersystem/src/user/drivers/core/src/bt_peer.rs:1). Even there, the key agreement is [answered with a fixed DHKey, so both halves "agree by construction"](/data/yellow/libersystem/src/user/drivers/core/src/bt_peer.rs:206).

   HFP's AT dialogue, AVDTP, AVRCP, RFCOMM credits, OBEX, BNEP and the LE Audio control services have no such sample transcripts. A peer written by the same people from the same reading confirms their interpretation, not the specification. To check audio and networking it also needs its own SBC, mSBC and LC3 codecs, a NAP that answers DHCP, and a BR/EDR controller's pairing. That is a second stack, comparable in size to the one under test.

   **Independent peers exist.** P02M0180 already set the rule this should follow: ["Use specifications and independently authored implementations"](/data/yellow/libersystem/docs/todo/P02M0180.md:71). Two Apache-2.0 projects fit:
   - [Bumble](https://github.com/google/bumble) implements [SDP, RFCOMM, HFP, A2DP/AVDTP, AVRCP, HID and SMP](https://github.com/google/bumble/tree/main/bumble) and [BAP, ASCS, PACS, VCS and BASS](https://github.com/google/bumble/tree/main/bumble/profiles). It has no OBEX or BNEP module.
   - [RootCanal](https://github.com/google/rootcanal) is a virtual BR/EDR and LE controller that serves HCI as H4 over TCP.

   **The SCO/ISO composition does not exist.** The same item says "the usbredir controller from P02M0099 carries SCO and ISO under them":
   - The in-guest fixture is a driver bound to [QEMU's `edu` device](/data/yellow/libersystem/src/user/services/manifest.toml:2672) that publishes `bluetooth-hci` [directly](/data/yellow/libersystem/src/user/drivers/core/src/bt_fixture.rs:9). Nothing it sends crosses USB.
   - P02M0099 plans only a [`bt-sco` loopback that answers "the few commands the oracle sends"](/data/yellow/libersystem/docs/todo/P02M0099.md:6356), and no ISO fixture at all.
   - [`usbredir_device.py`](/data/yellow/libersystem/src/harness/usbredir_device.py:582) has no Bluetooth device today.

   **Correct the peer-simulator and guest-gate items.** For each part, name the fixture that is the oracle and why it is independent:
   - Use an independent peer, for example Bumble peers on Bumble's or RootCanal's controller, reached through a harness controller bridged onto the guest's USB transport. That also puts SCO and ISO on the real alternates and bulk pipes.
   - Keep the in-guest fixture for fault injection (bad fragments, resets, unplug). If no independent peer is adopted for OBEX and BNEP, keep it for those too and state that it is written by the same team.
   - State the licence basis for any external peer: test-only, never linked.
   - Delete the composition that puts the usbredir controller under the in-guest fixture.

5. **Medium - The SCO and ISO hand-offs to P02M0099 are unowned, circular and unordered, and the ISO item omits what Auracast needs.**

   **ISO.** Part g says P02M0099 "carries the kind once this milestone defines its bounds" ([g](/data/yellow/libersystem/docs/todo/P02M0194.md:86)). The bounds already exist: [4100 bytes, negotiated](/data/yellow/libersystem/docs/todo/P02M0180.md:146), and in [`hci-attachment`](/data/yellow/libersystem/src/idl/device.lsidl:431). P02M0099 names a different blocker. Over USB, ISO shares the ACL bulk pair and is told apart ["only by its connection handle, which a transport does not own"](/data/yellow/libersystem/docs/todo/P02M0099.md:6322), so carrying it "is a change to the service's contract and is not made here". That item was [reopened for SCO alone](/data/yellow/libersystem/docs/todo/P02M0099.md:6327), so no P02M0099 item owns ISO now.

   **SCO.** P02M0099's plan contradicts itself about the service's side of the contract:
   - it says that side ["is P02M0180's to own"](/data/yellow/libersystem/docs/todo/P02M0099.md:6331), but P02M0180 is complete;
   - it lists the contract under [its own OWNS](/data/yellow/libersystem/docs/todo/P02M0099.md:6342);
   - it leaves ["a route between the SCO link and AudioService"](/data/yellow/libersystem/docs/todo/P02M0099.md:6367) to the service side.

   This plan mentions that side only as "the transport P02M0099 plans" ([e](/data/yellow/libersystem/docs/todo/P02M0194.md:71)).

   **Auracast.** Part g's transport item names only connected isochronous streams. Its own last item, [Auracast reception](/data/yellow/libersystem/docs/todo/P02M0194.md:91), uses broadcast isochronous streams.

   **Correct parts e and g:**
   - Claim the service's side of both contracts in this milestone.
   - Decide how ISO is told apart. Either the service reclassifies bulk-IN packets by its own connected and broadcast stream handles, or it registers those handles with the transport. Then create or name the P02M0099 item that implements it.
   - Include broadcast streams.
   - State the order: P02M0099's SCO item before part e's gate, and the ISO transport change before part g's gate.

6. **Medium - The plan leaves the server-side roles, inbound connections, and the authorities for applications and files unplanned.**

   **Server roles.** The foundations name only ["the SDP client every classic profile is found through"](/data/yellow/libersystem/docs/todo/P02M0194.md:39). Peers find the following through this system's SDP records, so an SDP server is needed:
   - the Audio Gateway "a headset connects to" ([e](/data/yellow/libersystem/docs/todo/P02M0194.md:69));
   - the A2DP sink "a phone plays to" ([d](/data/yellow/libersystem/docs/todo/P02M0194.md:61)); the A2DP test suite checks a sink's service record as an SDP-server test ([A2DP.TS p21, 4.2.1.2](https://files.bluetooth.com/wp-content/uploads/dlm_uploads/2025/02/A2DP.TS_.p21.pdf#page=29));
   - OBEX receiving.

   A Broadcast Sink also "shall instantiate one Published Audio Capabilities Service", which is a GATT server ([BAP 1.0, 3.8](https://www.bluetooth.com/wp-content/uploads/Files/Specification/HTML/BAP_v1.0/out/en/index-en.html)).

   **Inbound connections.** Nothing says when the radio is connectable or discoverable, or which inbound profiles a bonded peer may open. The `btctl` "trust" in [h](/data/yellow/libersystem/docs/todo/P02M0194.md:95) is undefined.

   **Third-party GATT grant.** The GATT client is for "a third-party program" under "a grant scoped to one device" ([b](/data/yellow/libersystem/docs/todo/P02M0194.md:48)). PermissionManager grants only compiled rows for named components: [any other component gets nothing](/data/yellow/libersystem/src/user/services/core/src/permission_manager.rs:516), and [runtime requests are always refused](/data/yellow/libersystem/src/user/services/core/src/permission_manager.rs:520). There is no route to that grant.

   **OBEX files.** OBEX sends "a file the user named" and receives "into a place the user chose" ([f](/data/yellow/libersystem/docs/todo/P02M0194.md:80)). The service's roles are [the catalogue, the bond store and its three roots](/data/yellow/libersystem/src/user/services/manifest.toml:4604). It holds no file authority of either kind.

   **Correct parts a, b, f and h:**
   - Add SDP service records, and PACS for the broadcast sink, with the roles that need them.
   - Define connectable and discoverable operation as bounded operator verbs, and "trust" as a per-peer, per-profile authorization kept in the bond record.
   - Route the GATT grant through PermissionManager the way a camera grant is routed: a named component and a device alias, as in [`camera_policy`](/data/yellow/libersystem/src/user/services/core/src/permission_manager.rs:1007). Otherwise restrict it to shipped components until an interactive permission path exists.
   - Give OBEX a read handle transferred by the operator for sending, and a manifest-scoped writable directory for receiving.

7. **Medium - P02M0180's fixed bounds contradict the new profiles' mandatory minimums, and the plan replaces them only with "bounded like the LE tables".**

   **The old bounds.** P02M0180 fixed [one link per controller, four L2CAP channels per link, 512-byte SDUs and an ATT MTU of 23](/data/yellow/libersystem/docs/todo/P02M0180.md:268). The pure leaf enforces [`MAX_SDU = 512`](/data/yellow/libersystem/src/user/services/logic/src/l2cap.rs:28) because "the boot mouse subset runs on the default ATT MTU of 23".

   **What the new profiles require:**
   - BNEP ["specifies a minimum L2CAP MTU of 1691 bytes"](https://www.bluetooth.com/wp-content/uploads/Files/Specification/HTML/BNEP_v1.0/out/en/index-en.html) (1.2).
   - A BAP Unicast Client ["shall support a minimum ATT_MTU of 64 octets"](https://www.bluetooth.com/wp-content/uploads/Files/Specification/HTML/BAP_v1.0/out/en/index-en.html) (3.6.1).
   - The plan itself wants "several ACL links at once, bounded like the LE tables" ([a](/data/yellow/libersystem/docs/todo/P02M0194.md:30)), but the LE table allows one link.

   The only other bound in the plan is a re-measurement of the Domain limits at the end ([h](/data/yellow/libersystem/docs/todo/P02M0194.md:104)).

   **Why it matters.** P02M0180 [sized the service's Domain from those numbers](/data/yellow/libersystem/docs/todo/P02M0180.md:262). Measuring after the fact reverses that method.

   **Correct part a's link item and part h's limits item.** Before implementation, state:
   - links per controller and per radio, and channels per link;
   - per-profile SDU and MTU ceilings that meet the specification minimums (BNEP 1691, ATT 64 for LE Audio);
   - ERTM windows, and queue and aggregate storage.

   Derive the Domain limits from these, and keep the re-measurement as a check.

8. **Medium - The codec oracles and the licence basis for LC3 are asserted, not sourced.**

   **Oracle sources.** SBC is to be tested "against published vectors" and LC3 "against the published conformance vectors" ([d](/data/yellow/libersystem/docs/todo/P02M0194.md:61), [g](/data/yellow/libersystem/docs/todo/P02M0194.md:88), [h](/data/yellow/libersystem/docs/todo/P02M0194.md:102)). No source is named.
   - **SBC.** The conformance bitstreams and reference implementation are distributed through the SIG's document system ([A2DP.TS p21, reference 6](https://files.bluetooth.com/wp-content/uploads/dlm_uploads/2025/02/A2DP.TS_.p21.pdf#page=8)). That document is "Bluetooth SIG Proprietary", and its use "by members ... is governed by the membership ... agreements" (page 1). mSBC inherits the same question.
   - **LC3.** Conformance uses the SIG's [LC3 test software](https://www.bluetooth.com/specifications/specs/low-complexity-communication-codec-1-0/) and ETSI's LC3plus conformance software ([liblc3's conformance notes](https://github.com/google/liblc3/blob/main/conformance/README.md)), each under its own terms.

   **Licence basis.** The plan excludes AAC because "its use is licensed" and excludes SIG qualification ([scope](/data/yellow/libersystem/docs/todo/P02M0194.md:24), [excludes](/data/yellow/libersystem/docs/todo/P02M0194.md:107)). The royalty-free patent licence in the Bluetooth PCLA covers only "Compliant Portions" that "are qualified pursuant to the Bluetooth Qualification Process" ([PCLA 1(j) and 5(a)](https://www.bluetooth.com/wp-content/uploads/2019/03/PCLA-ESign-Version-Version-11.pdf)). Vendors describe LC3's use as ["included in the Bluetooth product licence when you qualify your product"](https://www.ezurio.com/support/faqs/licence-required-use-lc3-audio-codec). So LC3 without qualification raises the same question the plan settled for AAC.

   **Correct the codec items and the licensing exclusions:**
   - Name each oracle's source and terms, as [P02M0196 does for ACPICA](/data/yellow/libersystem/docs/todo/P02M0196.md:72).
   - Where the official vectors cannot be used, use an independently written implementation as a differential oracle in the host suites. Android's SBC encoder and decoder (Apache-2.0, in the Android Open Source Project's Bluetooth stack) and Google's [liblc3](https://github.com/google/liblc3) (Apache-2.0) both qualify.
   - Put the question of shipping LC3 without qualification to the owner, as the AAC question was.

Validation was by inspecting the plan, the milestones, the IDL and the source, and by reading these primary documents:
- Bluetooth Core 5.4 Security Manager;
- BNEP 1.0 and BAP 1.0;
- the A2DP test suite;
- the Bluetooth PCLA;
- the public Bumble, RootCanal and liblc3 repositories.

No plan or source file was modified. No build, test or guest run was performed.

PLANNER'S RESPONSE ON P02M0194 (2026-09-26T02:46:11Z):

Verified every finding against the tree and the related plans. `input_service.rs` holds one profile connection and one stream opened with `open-mouse` on the first enabled peer; `bluetooth.lsidl` has no other profile verb; `audio_engine.rs` has one `snd` slot filled from the first live publication and `MAX_CAPTURES = 1`, and `audio.lsidl` converts everything to a fixed 48 kHz stereo with `beep`, `open-stream` and `open-capture` alone; `network-link-admin` is raw IP, minted to ModemService through its `LINK` role, and `uplink.rs` runs one uplink; NetworkService takes a NIC as a `MAC` greeting with the MTU followed by one Ethernet frame per message; ServiceManager's `cap_grants`/`service_of_cap` is the broker table InputService's `BLUETOOTH` name lives in; PermissionManager's `dynamic_policy` refuses every runtime request and `camera_policy` binds a component to an alias; `admin.lsidl` has two actions and the executor contract, `admin_broker.rs` waits `CONFIRM_TICKS = 6000` (60 s), and DeviceManager hands the trusted key sink to `virtio_input` and `xhci` alone; `bt_fixture.rs` binds QEMU's `edu` device and publishes `bluetooth-hci` directly, and `bt_peer.rs` answers a fixed DHKey; `usbredir_device.py` emulates a microphone and two DFU targets; `l2cap.rs` has `MAX_SDU = 512`; `hci-attachment` already carries `iso` and `max-iso`. P02M0099's SCO plan, its destination table's AudioService row, P02M0180, P02M0188 and P02M0192 say what the audit quotes. The specification figures (the 30-second Security Manager timer, BNEP's 1691, BAP's 64) were taken as cited and are consistent with the LMP response timeout and the piconet's seven active members. Bumble is not installed on the host (`import bumble` fails); `dnsmasq` is. The owner's breadth is not questioned.

1. **ACCEPTED - No delivery path for audio, network, serial or multi-device input.** Only a driver binding publishes into the catalogue, so the plan now has a header section, "HOW WHAT THE STACK PRODUCES REACHES ITS OWNERS", naming four contracts on roots of BluetoothService's own, each reached by its one consumer by name through the broker - `bluetooth-profile` (InputService), `bluetooth-audio` (AudioService), `bluetooth-network` (NetworkService) and `bluetooth-admin` (PermissionManager, minting `bluetooth-serial` and `bluetooth-gatt` per launch) - with the reason it is the broker and not a role (a role is a dependency and a stop takes the reverse closure), the manifest/broker-name/grant-row shape, and one loss-and-restart rule with each consumer's reaction. Part c grows `bluetooth-profile` to `open-input` (pointer, keys, consumer-control keys, P02M0192's gamepad record) and InputService's slot to eight peers, keys never into the trusted sink. Part g delivers PAN as a frame channel speaking the NIC's existing frame-plus-link wire, so NetworkService's own ARP, DHCP and IPv6 run on it, with the uplink rule (the operator's `connect` says whether it may replace the selected uplink, as a modem data grant does) and SPP as an application grant; "like every other serial port" is gone. Declined: an Ethernet-framed variant of `network-link-admin` (BluetoothService would then hold a role on NetworkService, the dependency the broker avoids) and DHCP inside the Bluetooth service (a second network stack); no publication path for device-less programs is built.

2. **ACCEPTED - AudioService has no device, format, latency, voice or control model.** A new part d, "AudioService: devices, routing, voice and calls", precedes the audio parts (the former parts d to h are now e to i). It specifies the inventory with per-device formats and a device-side PCM contract that drives a Bluetooth endpoint exactly as a sound card; the routing rule (arrival becomes the default, departure restores the previous one, a stream moves and never ends, silence counted per the owner's 2026-09-25 choice); pacing for A2DP, SCO, ISO and a phone's stream (a 200 ms jitter buffer); latency in the inventory and on the stream; per-device volume sent to devices that own it (AVRCP, HFP gain, VCP); `open-voice` at 8 or 16 kHz mono from a grant of its own, whose opening brings the SCO or isochronous link up; a call state on the voice session with the headset's commands relayed back, and "no call, no network service, ERROR" when there is none; the A2DP sink routed to the output and never capturable; `audio-control` with `audioctl`; and `bluetooth-audio`. Its relation to P02M0099's AudioService destination row is the tree's first-implemented-consumer rule. Declined in part: AVRCP and media commands are not delivered to AudioService but as consumer-control keys through InputService (part c), where a keyboard's media keys go.

3. **ACCEPTED - The protected screen cannot carry a pairing confirmation.** Part a now has "THE PAIRING INTERACTION, ONE RULE FOR BOTH RADIOS": every model that needs a person, and consent to an incoming pairing, is a prompt the service originates on the operator authority (`btctl` the one holder), with the value, keypress notifications and a 25-second deadline inside the 30-second SMP and LMP timers; KeyboardDisplay is declared only while a watcher is attached, NoInputNoOutput otherwise; the answer's meaning and the terminal's overdraw limit are stated for `docs/THREAT_MODEL.md` (a part i item); a first Bluetooth keyboard is paired from whatever terminal exists. Legacy PIN and LE legacy are operator-requested at their own lower level and never a downgrade of a Secure Connections bond; OBEX receiving uses the same prompt path (part g). Out Of Band is excluded with its reason. Declined: extending AdminService with a non-executor originator and entry - it would still be absent on a machine whose only keyboard is Bluetooth, needs the entry P02M0188 excluded, and would make a second surface where one rule suffices.

4. **ACCEPTED - The peer simulator is not an independent oracle.** Part i's oracle item replaces it: Bumble peers on RootCanal, reached through `usbredir_device.py --emulate bt-bridge` (P02M0099's `bt-sco` voice alternates, HCI from RootCanal), so every gate crosses the real USB transport with SCO and ISO on their real pipes; test-only, never staged. BNEP and OBEX peers are written here on Bumble's L2CAP/RFCOMM and recorded as same-team oracles, the access point bridged to the host's `dnsmasq`; a RootCanal feature a part lacks makes that part's oracle the in-guest fixture, said so; the in-guest fixture stays for fault injection. The composition of the usbredir controller under the in-guest fixture is deleted. Adopting them is a new host dependency and is recorded as an owner question asked when part a starts, with the fallback stated.

5. **ACCEPTED - SCO and ISO hand-offs unowned and unordered.** Part f owns the service half of the SCO transport (synchronous connection setup, the `voice` call, SCO packets, CVSD in the controller, mSBC framing in the host), and P02M0099's SCO item is ordered before part f's gate. Part h owns ISO, both halves, since no P02M0099 item does: THE SERVICE CLASSIFIES inbound bulk packets by its own connected and broadcast stream handles, the transport carries outbound `iso` on bulk OUT and delivers inbound bulk packets as `acl` under the larger ceiling, the per-kind ceiling applies after classification, and a packet that overtakes its stream's creation event is dropped and counted; CIS and BIS both, and Auracast reception now has extended scanning, periodic advertising sync and a BIG sync. That item is ordered before part h's gate. Declined: creating a P02M0099 item (that plan is not this milestone's to edit; the needed wording changes there are reported to the coordinator).

6. **ACCEPTED - Server roles, inbound connections, application and file authorities.** Part a gains an SDP server with a record per offered role and an inbound-policy item (connectable and discoverable bounded, pairable only with a watcher, LE always central, trust per peer and per profile in the bond record, P02M0180's `enable` becoming trust for input); part b gains a GATT server with GAP, GATT and PACS, and routes the GATT grant through PermissionManager from `bluetooth-admin` with a component and an operator-set peer alias, excluding the services the stack drives, stating that until an interactive path exists a third-party program is one a policy row names. Declined in part: a manifest-scoped receive directory and an operator-transferred read handle - OBEX uses the shell's own redirection instead (`btctl send <peer> <name> < file`, `btctl receive <peer> > file`), which gives the same "the user named it" property with an existing mechanism and leaves the service without any storage authority.

7. **ACCEPTED - P02M0180's fixed bounds contradict the new profiles.** Part a's first item states the bounds before any code - links per controller and per radio, channels per link, ERTM channels and windows, 1691-byte BR/EDR SDUs (BNEP's minimum, OBEX's packet), ATT MTU 247 (above BAP's 64), synchronous and isochronous stream counts, 1 MB aggregate storage against its computed worst cases, bonds - and derives the Domain from them (IPC queues 2 MB to 4 MB for the new standing queues, the rest unchanged with the reason); part i's re-measurement is the check.

8. **ACCEPTED - Codec oracles and the licence basis.** Part i's codec-oracle item names the sources and terms: the A2DP and HFP specifications' SBC/mSBC definitions differentially against AOSP's SBC, LC3's specification differentially against liblc3, both Apache-2.0 and host-only, the SIG's member-only material only if the owner obtains it; CVSD is the controller's and needs none. The owner question is recorded for when part e starts and covers SBC and mSBC with LC3, because the licence clause cited covers every compliant portion of an unqualified product, not LC3 alone.

Re-check of the whole plan: every part names its consumer contract, its authority, its bounds and its oracle; the delivery rule, the pairing rule and the ORDER line agree with the parts after the renumbering; every mechanism it relies on exists (the broker's grant table, PermissionManager's alias-bound grants, the NIC greeting wire, the shell's redirection, `hci-attachment`'s ISO fields) or is an item here; the owner's decisions and dates are intact, the plan references no audit, and the versions stay 1. The plan is complete, correct, feasible and internally consistent, and ready for its parts' own plans. Only `docs/todo/P02M0194.md` and this append were changed; no source was modified, nothing was built or run.


AUDITOR'S RE-AUDIT OF PLAN P02M0194 (2026-09-26T04:01:12Z):

**Rating: 6/10.** The delivery, audio, oracle and transport corrections hold, but four problems remain: a Bluetooth keyboard never reaches the text console, OBEX receiving cannot be accepted where the plan says it is, the media keys have no destination, and the pairing rules do not map onto BR/EDR.

I read the whole history: the original review of the plan at `07371c44`, the planner's response, the plan at `07371c44` and at HEAD, and commit `0dd5da07`'s changes to P02M0099. Those changes give ISO to P02M0194h and the service half of SCO to P02M0194f, and they agree with this plan.

I checked the plan against the tree:
- the Bluetooth IDL, and the HCI contract in `device.lsidl` (the 4100-byte wire bound, `iso` and `max-iso`);
- the broker's `cap_grants` and `service_of_cap`, and PermissionManager's `btctl` row and alias-bound mints;
- InputService's Bluetooth slot and key path, AudioService's engine and `audio.lsidl`;
- NetworkService's NIC greeting and `uplink.rs`, and the bond store's record format;
- `btctl`, the shell's launch shapes and redirection, `usbredir_device.py` and `usb_ffs.py`.

I also checked the plan against its neighbours: P02M0099, P02M0180, P02M0188, P02M0192 and P02M0199. I read Bumble and RootCanal at their current heads:
- Bumble has no BNEP, OBEX, HSP or Telephone Bearer client.
- RootCanal has Secure Simple Pairing, eSCO with host SCO data, CIS and BIG broadcast. It has no LE BIG Create Sync, and the plan's stated fallback to the in-guest fixture already covers that.

These corrections hold: the four broker contracts, the AudioService device model, the service-originated prompt, the independent oracle, the SCO and ISO ownership, the SDP and GATT servers, the GATT grant on the camera route, the stated bounds and the codec oracles. Four places do not hold, below.

1. **High - A bonded Bluetooth keyboard can type only into a graphical application that holds display focus. The text console, the shell, every console tool and `btctl` itself never receive its keys.**

   **What the plan routes.** Part c sends a Bluetooth keyboard's keys ["into the ordinary key stream"](/data/yellow/libersystem/docs/todo/P02M0194.md:157), and the plan names no other key path. That stream is InputService's `subscribe-keys`, ["granted only to the display owner"](/data/yellow/libersystem/src/idl/input.lsidl:52).

   **Why the console never sees it.**
   - The console does not read that stream. ConsoleService takes its keystrokes from [the kernel's console input](/data/yellow/libersystem/src/user/services/core/src/console_service.rs:9), on [a channel the kernel feeds](/data/yellow/libersystem/src/user/services/core/src/console_service.rs:612).
   - Bytes enter that input only through `console_feed`, which requires [a ConsoleInputSource capability](/data/yellow/libersystem/src/user/runtime/rt/src/lib.rs:3052). DeviceManager holds that capability ["only to delegate to the same two keyboard drivers"](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:449).
   - The console half of a keyboard lives in those drivers. [`drivers::keys`](/data/yellow/libersystem/src/user/drivers/core/src/keys.rs:1) applies the layout and the escapes. It also [suppresses Ctrl+Alt+F12](/data/yellow/libersystem/src/user/drivers/core/src/keys.rs:415), handles [the Ctrl+Alt+Delete reboot chord](/data/yellow/libersystem/src/user/drivers/core/src/keys.rs:421) and handles [the Power key](/data/yellow/libersystem/src/user/drivers/core/src/keys.rs:436). A USB keyboard [feeds both paths](/data/yellow/libersystem/src/user/drivers/core/src/usb_hid.rs:310).
   - InputService receives [no console capability at bootstrap](/data/yellow/libersystem/src/user/services/core/src/input_service.rs:663). BluetoothService's [manifest roles](/data/yellow/libersystem/src/user/services/manifest.toml:4604) carry none either.

   **Why it matters.** The plan's own premise needs this path. It rejects P02M0188's screen partly because ["a machine whose only keyboard is Bluetooth could never pair one"](/data/yellow/libersystem/docs/todo/P02M0194.md:102). It also says a bonded keyboard ["reconnects with nobody present"](/data/yellow/libersystem/docs/todo/P02M0194.md:106). On such a machine, that keyboard could not type a shell command or answer a `btctl` prompt. A gate that checks only `subscribe-keys` would pass all the same.

   The original finding 1 did not ask for this path ([its input bullet](/data/yellow/libersystem/AI/audit/plan-audit-P02M0194.md:45)), and the correction did not add it. It is newly found.

   **Correct part c's InputService item.** State how a bonded Bluetooth keyboard types into the text console:
   - which component feeds its keys to the kernel console input, and under which delegated ConsoleInputSource;
   - that the keys pass through the shared `drivers::keys` cooking;
   - whether its Ctrl+Alt+Delete and Power key act. Each needs a SystemPower connection. Record the decision in `docs/THREAT_MODEL.md`.

   Add to part c's gate: the independent keyboard peer types a command into the shell.

2. **High - OBEX receiving cannot be accepted on `btctl`'s terminal, and the prompt watchers cannot read an answer from the shape `btctl` launches with today.**

   **What the plan says.** The watchers are "a running `btctl pair` or `btctl pairable`" ([plan](/data/yellow/libersystem/docs/todo/P02M0194.md:98)). They are answered "with yes or no, a passkey or a PIN" ([plan](/data/yellow/libersystem/docs/todo/P02M0194.md:95)). An OBEX push waits on `btctl receive <peer> > file`, and the offer is ["shown on `btctl`'s terminal and accepted there"](/data/yellow/libersystem/docs/todo/P02M0194.md:246).

   **The watchers.**
   - The shell launches `btctl` with [the `Rest` shape](/data/yellow/libersystem/src/user/services/core/src/shell.rs:795), which goes through [`run_tool`](/data/yellow/libersystem/src/user/services/core/src/shell.rs:963).
   - `run_tool` [hands the tool only the write end of a relay channel](/data/yellow/libersystem/src/user/services/core/src/shell.rs:1513). The shell only reads the other end, so the tool has no keyboard.
   - Only tools with the interactive shape get the terminal, through [`run_tool_interactive`](/data/yellow/libersystem/src/user/services/core/src/shell.rs:1799). The plan does not move `btctl` to it.

   **Receiving.**
   - A redirection is a pipeline. [`cmd > b` becomes `cmd | redirect_out b`](/data/yellow/libersystem/src/user/services/core/src/shell.rs:272).
   - Every pipeline stage gets a send-only terminal, because ["a stage that could take from that queue would eat the next line the user typed"](/data/yellow/libersystem/src/user/services/core/src/shell.rs:1628).
   - The broker gives the first stage [no stdin](/data/yellow/libersystem/src/user/services/core/src/permission_manager.rs:1359). It gives every stage a diagnostics endpoint that is ["Send-only deliberately"](/data/yellow/libersystem/src/user/services/core/src/permission_manager.rs:1368).
   - So `btctl receive <peer> > file` can print the offer but can never read an answer, whatever shape `btctl` has. The item cannot close as written.

   **How it continues finding 6.** The planner declined the auditor's receive directory ([response](/data/yellow/libersystem/AI/audit/plan-audit-P02M0194.md:232)), saying the shell's redirection gives the same property "with an existing mechanism". That holds for sending with `< file`. It does not hold for receiving.

   **Correct part a's pairing item and part g's OBEX item.**
   - State that `btctl` takes the interactive launch shape, so `pair` and `pairable` can read answers.
   - Make the receive invocation its own consent. For example, `btctl receive <peer> <max-bytes> > file` accepts the first object that peer pushes within the deadline and the stated size. It prints the escaped name, size and type on the terminal's diagnostics endpoint and asks nothing.
   - If the plan keeps an interactive accept, it must give receiving a destination other than a shell redirection.

3. **Medium - Consumer-control keys have no vocabulary, no stream and no consumer, and the ORDER line cites a vocabulary no plan defines. They cover a keyboard's media keys, a headset's AVRCP buttons and LE Audio's media-control commands.**

   **Where the plan sends them.**
   - `open-input` carries ["consumer-control keys"](/data/yellow/libersystem/docs/todo/P02M0194.md:149).
   - A headset's AVRCP play, pause, next and previous are ["delivered as the same consumer-control keys"](/data/yellow/libersystem/docs/todo/P02M0194.md:154). The earbuds' Media Control commands [arrive the same way](/data/yellow/libersystem/docs/todo/P02M0194.md:275).
   - The InputService item routes [pointers, keys and gamepads](/data/yellow/libersystem/docs/todo/P02M0194.md:157) and gives consumer-control keys no destination.
   - The ORDER line says they ["use the vocabulary P02M0099's HID expansion plans for them"](/data/yellow/libersystem/docs/todo/P02M0194.md:164). That item names consumer controls only in [its opening sentence](/data/yellow/libersystem/docs/todo/P02M0099.md:4764). Neither [what it owns](/data/yellow/libersystem/docs/todo/P02M0099.md:4793) nor [the vocabulary it added](/data/yellow/libersystem/docs/todo/P02M0099.md:4855) has a consumer-control record.

   **What the tree and P02M0199 do.**
   - [`key-event` is keyboard-page only](/data/yellow/libersystem/src/idl/input.lsidl:17).
   - A keyboard's media keys are dropped, ["reserved until a media session exists"](/data/yellow/libersystem/src/user/drivers/core/src/keys.rs:176).
   - The one plan that routes consumer-page usages, P02M0199c, rules that ["a consumer-page usage never enters `key-event`"](/data/yellow/libersystem/docs/todo/P02M0199.md:191). Its `system-keys` stream [carries only brightness](/data/yellow/libersystem/docs/todo/P02M0199.md:193).

   **Why it matters.** The planner answered original finding 2's question - who receives the headset's play and pause - with ["where a keyboard's media keys go"](/data/yellow/libersystem/AI/audit/plan-audit-P02M0194.md:224). They go nowhere. Three implementers would build three different things:
   - one adds media usages to `key-event`, against P02M0199c;
   - one adds a new stream with no consumer;
   - one drops them, and a headset's play button does nothing.

   **Correct part c and its ORDER line.** Do one of the following:
   - Define the record and the stream consumer-control keys travel on, who may subscribe and under what focus rule. Carry them in P02M0199c's consumer-page frame and keep them out of `key-event`.
   - Or state that they are recognised and dropped until a media session exists, as a keyboard's are, and remove the claims that headset and earbud buttons act.

   Either way, stop citing a P02M0099 vocabulary that is not planned.

4. **Medium - The "one rule for both radios" is written in LE's terms and does not map onto BR/EDR in three places.**

   **IO capability.**
   - The host declares [KeyboardDisplay while a watcher is attached](/data/yellow/libersystem/docs/todo/P02M0194.md:98). The BR/EDR IO capability reply takes only DisplayOnly, DisplayYesNo, KeyboardOnly and NoInputNoOutput ([Core 5.4, Vol 4 Part E, 7.1.29](https://www.bluetooth.com/wp-content/uploads/Files/Specification/HTML/Core-54/out/en/host-controller-interface/host-controller-interface-functional-specification.html#UUID-063323a1-51b0-a373-8e29-84f9d0e0263e)).
   - GAP maps a keyboard with a numeric display to DisplayYesNo, and with that capability the host shows passkeys and never types one ([Core 5.4, Vol 3 Part C, 5.2.2.5 and 5.2.2.6](https://www.bluetooth.com/wp-content/uploads/Files/Specification/HTML/Core-54/out/en/host/generic-access-profile.html#UUID-d3c98a61-d1f0-6c8a-c88f-760073e0b784)). Linux sends DisplayYesNo in its place, "as it is not supported by BT spec" ([hci_event.c](https://github.com/torvalds/linux/blob/master/net/bluetooth/hci_event.c)).
   - So ["Passkey Entry in either direction"](/data/yellow/libersystem/docs/todo/P02M0194.md:93) holds on LE only. An implementer who picks KeyboardOnly instead changes the model phones and keyboards get.

   **Key strength.**
   - BR/EDR Secure Simple Pairing yields P-192 or P-256 keys ([Core 5.4, Vol 4 Part E, 7.7.24](https://www.bluetooth.com/wp-content/uploads/Files/Specification/HTML/Core-54/out/en/host-controller-interface/host-controller-interface-functional-specification.html#UUID-ef6b7301-ab4b-cce6-1fa4-c053c3cd1585)).
   - The plan knows only Secure Connections and legacy PIN. It refuses only ["a peer bonded with Secure Connections that later offers only legacy"](/data/yellow/libersystem/docs/todo/P02M0194.md:112).
   - A P-192 bond has no stated level. A P-256 bond re-paired at P-192 is not covered by "never downgraded".

   **Trust enforcement.** Inbound trust is ["refused at L2CAP"](/data/yellow/libersystem/docs/todo/P02M0194.md:127). HFP, HSP and OPP over RFCOMM share RFCOMM's one PSM, 0x0003 ([Assigned Numbers](https://www.bluetooth.com/specifications/assigned-numbers/)). A per-profile refusal therefore has to happen per RFCOMM server channel.

   **Correct part a's pairing and inbound-policy items.**
   - State DisplayYesNo as the watcher-attached BR/EDR capability.
   - Give P-192 Secure Simple Pairing a level of its own, and make "never downgraded" cover a P-256 bond offered P-192.
   - Enforce inbound trust per PSM, and per RFCOMM server channel for the RFCOMM profiles.

Validation: this was a read-only inspection. I read the plan at HEAD and at `07371c44`, the audit file, commit `0dd5da07`'s diff of P02M0099, the neighbouring plans and the source tree. For reference I read, in the scratchpad, shallow clones of Bumble and RootCanal, the Core 5.4 HCI, GAP and USB transport pages, Linux's `hci_event.c` and liblc3's conformance notes. No plan, source or audit file was modified, and nothing was built, tested or booted.
