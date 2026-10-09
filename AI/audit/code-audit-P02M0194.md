IMPLEMENTER'S IMPLEMENTATION ON P02M0194 (2026-10-04 to 2026-10-06):

Scope: `docs/todo/P02M0194.md`, parts a to i - the classic foundations and pairing (a), LE beyond one mouse (b),
input (c), AudioService's device, voice and call model (d), A2DP and AVRCP (e), HFP and HSP over SCO (f), serial,
file push and tethering (g), LE Audio (h), and the operator tool, verification and completion (i). What stays open is
the owner's: the independent radio and peers (RootCanal and Bumble) and the codec oracles (AOSP SBC, liblc3). The
end-of-job runs are done: the three cross-builds, the regenerated dynamic report and its gate, and the Domain limits
read after the LE Audio gate's load.

## What was implemented

**The classic foundations (a).** `service_logic::{hci_bredr, l2cap_bredr, sdp, rfcomm, bt_pairing, bt_policy}`, each
with its suite: inquiry and paging, every BR/EDR pairing model through the operator's prompt watcher with levels
that are never lowered, the inbound policy per radio and per profile (PSM at L2CAP, server channel on RFCOMM), sixteen
L2CAP channels a link with enhanced retransmission (`Ertm`), an SDP client and server, and one RFCOMM session a link
with eight DLCs and credits - bounded writes (`room`, eight frames past the credits) and a paused reader (`pause`).
BluetoothService's `bluetooth_service/classic.rs` drives them. Gated: `bluetooth-classic`.

**LE (b).** Several links, privacy both ways (resolving lists, this host's own private address), every LE pairing
model, keys derived across transports both ways, the attribute server (`service_logic::gatt_server`) and applications'
GATT grants (`bluetooth-admin.mint-gatt`, PermissionManager's `bluetooth-gatt`). Gated: `bluetooth-le`.

**Input (c).** Classic HID and HID over GATT, gamepads into the gamepad set, a Bluetooth keyboard at the console
through InputService with its Ctrl+Alt+Delete and Power key acting on nothing. Gated: `bluetooth-input`.

**AudioService (d).** The `bluetooth-audio` root (one subscriber, never a role), outputs, voice devices and the
A2DP-sink route; `open-voice` and `voice-session` from `audio-admin.open-voices`; the call relay; per-device volume;
pacing (`audio_routing::{TimerPacer, OneForOne, Jitter}`). Gated: `audio-routing`, `bluetooth-audio`,
`bluetooth-le-audio`.

**Music (e) and calls (f).** `service_logic::{avdtp, avrcp, sbc, hfp}`: A2DP source and sink with SBC, AVRCP
absolute volume and the remote control, the HFP/HSP Audio Gateway with mSBC over SCO (H2 framing, one unit back per
unit in), the relayed call's indicators. Gated: `bluetooth-audio`.

**Serial, files and tethering (g).** `bluetooth-serial` grants per launch for one aliased peer (`btserial`);
`service_logic::obex` (CONNECT, PUTs, the final PUT, DISCONNECT; `PushServer`'s bound before a byte) over GOEP L2CAP
in enhanced retransmission or RFCOMM; `btctl send`/`receive` through the shell's redirections; `service_logic::bnep`
and NetworkService's `bluetooth-network` links with the uplink rule (`service_logic::uplink`). Gated:
`bluetooth-classic`, `bluetooth-transfer`.

**LE Audio (h).**
- THE ISO TRANSPORT: `drivers::bt_usb` cuts inbound bulk data by the four-byte header both kinds share and carries
  outbound `iso` on the bulk OUT pipe; BluetoothService classifies by handle (`service_logic::le_iso::classify`),
  reassembles SDUs, counts what no table names, and paces against the controller's ISO buffers (`LE Read Buffer Size
  v2`, Number Of Completed Packets on a stream's handle).
- THE CONTROLLER'S MODE: `LE Read Local Supported Features` read at initialisation - with extended advertising every
  scan and connection runs the extended commands (`le_scan_parameters`, `le_scan_enable`, `le_create_connection` on
  `Controller`), the legacy ones never mixed in; an extended report's whole data read for an RSI, its first 31 bytes
  as a legacy report's.
- LC3 (`service_logic::lc3`, written from the specification): every configuration, Appendix B's concealment, no
  heap; the suites reproduce Appendix C's four reference frames byte for byte and every intermediate step. The test
  data (`lc3/appendix_c.txt`) is extracted mechanically from the specification's text.
- UNICAST: `service_logic::{bap, ascs, cap, le_audio, gtbs}`; `bluetooth_service/le_audio.rs` - the walk (PACS,
  ASCS, CSIS with the SIRK decrypted under the bond's LTK, VCS), the coordinated set found by RSI and bonded as the
  set's (`pair_le`, not the operator's scan-bound `pair`), offered once to AudioService, its group set up and wound
  down in order (ASEs released, CISes disconnected, only then the CIG removed), music and calls taking it in turn.
- THE HOST'S GATT SERVER: GTBS (`host_server`, `le_server_writes`, `le_call`) and PACS.
- BROADCAST: `bluetooth_service/broadcast.rs`, `btctl broadcast scan|play|stop`, the operator's four calls.

**The operator, verification and completion (i).** `btctl` with every command the milestone lists; host suites for
every pure leaf; the gates registered in `check.sh`, the verification model's catalogue and its release-required set;
`docs/THREAT_MODEL.md` section 2.4.

**The in-guest fixture, written apart.** `drivers::{bt_world, bt_le_world, bt_le_audio, bt_peer, bt_sbc, bt_lc3}` and
`bt_fixture`: the controller and every peer the gates drive. Its LE Audio half (earbuds with PACS, ASCS, CSIS and VCS
servers and a GTBS client; a broadcast source with its BASE, BIGInfo and BIG; CIG, CIS, ISO and periodic sync on the
controller side) and its LC3 frame reader and writer were written by separate authors from the specifications alone,
without reading the host's LE Audio code or codec; the reader reproduces Appendix C's frames, and was also checked
against frames from liblc3 built outside the tree. The controller starts plain; `le-features 1` makes it an LE Audio
one from its next HCI Reset, so every older gate still runs the legacy LE paths.

## Defects found by the LE Audio gate and fixed

- The SIRK was decrypted with an all-zero key: `record()` returns a bond WITHOUT its keys (the store's list omits
  them); the walk now takes the LTK from `bond()` (the keyed lookup) and zeroes every copy.
- The set's other member was never heard: a controller reports each advertiser once per scan enable, and the member
  had been reported before the set's key was known. `scan_le` now restarts a running scan, and gives it its deadline
  back - or ends one it began - when the search is over, so an operator's next scan is not refused for it.
- The fixture died after a probe exited (a quiet exit, no panic): drivers now say why they leave their loop
  (`common::leaving`), which named it.
- A square-wave tone's third harmonic came out the loudest quantized line after LC3's tilted noise shaping; the probe
  plays sines.

## Departures and decisions

- The Generic Telephone Bearer notifies a call's state to, and takes Call Control Point writes from, only an encrypted
  peer trusted for voice (the milestone's wording); reads are answered to any connected LE peer.
- A coordinated set's further members are bonded with Just Works and trusted for audio without a prompt: the
  operator's pairing of the first member is the consent. A member that does not bond within ten seconds is let go.
- An LE Audio device is walked only on a controller with the CIS central role.
- AVRCP browsing waits with the media session the milestone excludes; enhanced retransmission is exercised by OBEX.

## Owner questions (asked at the end of the run)

- Adopting RootCanal and Bumble as the independent radio and peers; AOSP SBC and liblc3 as host oracles.
- Shipping SBC, mSBC and LC3 on a product that is not SIG-qualified; the SIG's member-only conformance material.
- Committing the LC3 Appendix C data extracted from the specification's text.
- The PAN access point: the `dnsmasq` harness peer waits on Bumble; the gate's access point is the fixture's own.

## Verification

Host suites: `cargo test --manifest-path user/services/logic/Cargo.toml` for the leaves above (LC3: 35 pass, 1 ignored
table print), `cargo test --manifest-path user/drivers/core/Cargo.toml bt_` for the fixture. Builds: all three
targets, and `./check.sh --gate dynamic-report` (92 tools on each). Guest gates on x86_64:
`bluetooth-classic`, `bluetooth-le`, `bluetooth-input`, `bluetooth-audio`, `bluetooth-transfer`, `bluetooth-service`,
`audio-routing` and `bluetooth-le-audio`.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0194 (2026-10-09T18:12:52Z):

The owner's phase-2 QEMU-only continuation explicitly asks to finish this milestone, including work that can be exercised with host peers. The complete plan and existing implementation record were read. The remaining unchecked requirements are independent RootCanal/Bumble peers on the production USB path and independent SBC/mSBC/LC3 host codec oracles. Existing in-guest fixture results remain same-team evidence; they do not establish these requirements. The user's continuation authorizes the plan's named test-only dependencies. Nothing from these dependencies will be linked into or staged with guest images; SIG member-only materials and product qualification remain outside this work. Implementation and verification results will be appended below as they occur.

Implementation update, 2026-10-09 (independent host codecs and USB bridge):

- Added `src/harness/prepare-bluetooth-oracles.py`, SHA-256/revision source pins and private Python version pins;
  dependencies stay under `.build/bluetooth-oracles`. AOSP SBC 745ee92b87ed62b5a8a1bac47d5df8cad623bf59, Google liblc3
  efe84d3e0ece9e54591098148da3d10f5503b84e, Google libsbc 6e505650145c9973d08a0bdd5e5f5e1914305e40, RootCanal 1.13.0 and
  Bumble 0.0.235. All codec sources are compiled unmodified; host adapters translate only ABI/framing. Source links,
  licenses, coverage and limitations are recorded in `src/harness/bluetooth-oracles.md`. No SIG member-only material
  was accessed and no product-qualification conclusion is made.
- Added and registered `bluetooth-codecs` in check.sh, verification catalogue and release-required set. It rebuilds
  the shipping `service_logic::{sbc,lc3}` leaves through `bluetooth-codec-host.rs`, independently crosses both
  encoders through both decoders, checks actual sample errors and retains source digests. The production guest does
  not link any oracle.
- The first independent LC3 quick run FAILED at 16 kHz/10 ms during 20-byte and 80-byte bitrate transitions: maximum
  normalized difference 0.03503418, RMS -59.58 dB, outside the published 0.00148/-89.06 dB precision limits.
  `lc3::ltpf::LtpfDecoder::run` now considers the effective activation bit AND nonzero bitrate-dependent gain.
  Previously an active zero-gain filter bypassed the next fade-in, and a zero gain with the active bit set bypassed
  fade-out. The four-line fix preserves the continuous histories and existing five transition cases.
- SBC investigation found reference defects, not a shipping-code defect. AOSP full-scale/low-bitpool four-band
  synthesis signed accumulator overflow was reproduced with `-fsanitize=signed-integer-overflow` at
  `synthesis-sbc.c:439` (retained `.build/bluetooth-oracles/aosp-overflow.log`). Precision vectors use a 12 dB margin
  and do not claim overload equivalence to this reference. AOSP four-band joint-stereo ReadScalefactors calls the
  aligned 4-bit reader with bitPtr=32 immediately after initialization, violating its <16 precondition; Google
  libsbc independently agrees with Liber on those exact AOSP bitstreams (maximum two PCM samples, -94.60 dB in the
  reproducer). Added Google libsbc as a complementary decoder for that configuration. AOSP remains the encoder
  oracle everywhere and the decoder oracle for every unaffected configuration. Google's frame-size API limit is
  explicitly respected; each case must have an independent decoder. Shipping SBC code is unchanged. Initial
  adapter setup failures (AOSP bitpool is set after initialization, and its HFP decoder also consumes one transport
  padding byte) were fixed in the host adapters, without changing guest codec framing.
- `BtBridge` in `usbredir_device.py` forwards real RootCanal H4 through existing Bluetooth USB control/interrupt/
  bulk/voice-alternate endpoints, tracks real successful CIS handles to classify outbound ISO, and fails on TCP
  loss or a bounded queue exceeding 1 MiB. It synthesizes no HCI command response, pairing or peer profile.

Verification actually executed:
- PASS `./check.sh --gate bluetooth-codecs`, exit 0, 26 s; 769 SBC/mSBC configurations, 49,216 cross-decoded frames,
  maximum normalized difference 0.00021362 and worst RMS -89.56 dB; 12 LC3 configurations, 11,448 cross-decoded frames,
  maximum normalized difference 0.00045776 and worst RMS -107.88 dB. Includes both durations, all rates, every
  20–400-byte LC3 size and repeated high/low-rate LTPF transitions. Log:
  `.build/bluetooth-oracles/registered-codecs-gate.log`; receipt `result.json` beside it.
- PASS `env -u RUSTFLAGS -u CARGO_ENCODED_RUSTFLAGS cargo test --offline --manifest-path
  src/user/services/logic/Cargo.toml lc3 -- --nocapture`: 35 passed, one existing ignored table-printer, 11.77 s
  test runtime (`lc3-regression.log`).
- PASS private RootCanal process/control-event probe: Reset, version, supported commands, BR/EDR features, LE
  features each came from RootCanal through BtBridge, with actual command opcode/status checked. Process reaped.
  `.build/bluetooth-oracles/rootcanal-bridge-proof.log`, controller log beside it. This is a host adapter probe,
  NOT a QEMU USB guest acceptance run.
- PASS Python compilation, rustfmt for new Rust adapter/changed LTPF, git diff --check at this update.
- NOT YET RUN: final shared three-architecture build/regressions after the production LTPF fix, independent Bumble
  profile QEMU gates and final shared verification-model checks. Independent radio requirement remains OPEN.

Integration progress (2026-10-09 18:54:47 UTC): added `src/tools/check-bluetooth-independent.py`, a two-boot guest driver using private serial/control sockets, a paired persistent system volume, and the RootCanal USB bridge. It drives real btctl prompts (independently compares numeric values), classic HID/input trust, RFCOMM application grants, independent classic audio/voice and OPP/PAN helper scenarios, coordinated LE stereo/voice/BIS, and stored-key reuse across service restart/cold boot followed by forget. LE verdicts require newly independently decoded captures with the expected frequency, format, channel assignment and no ISO loss/gaps; bridge counters must show actual command/event/ACL/SCO/ISO transport. The host verifier rejects stale, silent, wrong-format/channel/tone and short/lost/gapped capture outcomes in its `--self-test` (PASS), and Python compilation passes. The full guest gate is registered but UNPERFORMED pending the shared final development build. None of these new guest assertions is reported as passed yet. The real RootCanal host peer check separately demonstrated that version1.13.0 ignores encryption/Broadcast Code during BIG create sync; wrong-key RF enforcement is explicitly excluded under the user's emulator-only scope, retaining the existing same-team fixture's negative software checks. Clear and encrypted-flag BIS audio transport remain required in this gate.

Implementation update (2026-10-09T18:56:38Z): independent radio harness and guest gate, awaiting final guest verification.

- Added `bluetooth-radio-peers.py`, `bluetooth_classic_peers.py`, `bluetooth_le_peers.py` and the registered
  `bluetooth-independent` production-USB guest gate. The private JSON control socket changes peer behavior only;
  guest actions use shipping btctl/audioctl/btserial/gamepad and existing audioprobe grants. RootCanal owns every
  controller/radio packet. Bumble owns pairing, SDP, L2CAP, RFCOMM credits, HID, AVDTP, AVRCP, HFP, ATT/GATT,
  PACS/ASCS/CSIP/VCS, GTBS and ISO. Track metadata adds scenario dispatch through Bumble's existing metadata
  command/response codecs, whose target dispatcher otherwise lacks a metadata delegate. Dependency code remains
  host-only and unchanged. The classic keyboard descriptor explicitly includes usage 0x66 for the Power rejection
  case; gamepad data names independent button, axis and hat values.
- `bluetooth_transfer_peers.py` supplies OBEX and BNEP, explicitly same-team as the plan permits. OPP stores actual
  received objects and hashes them; outbound OBEX uses Bumble RFCOMM and checks each real response. The PAN peer
  only translates Ethernet headers. `bluetooth-pan-network.py` creates a private network namespace/TAP and runs
  real dnsmasq for DHCP; the isolated Linux stack supplies ARP/ICMP/TCP and echo traffic. No host physical interface
  is changed. Socket/process cleanup was corrected after a bounded startup/shutdown probe exposed a live stream
  preventing server shutdown. A RootCanal test-socket greeting can abort on EPIPE if a readiness probe closes early;
  startup now waits for its actual HCI-listening log instead of creating/discarding a test or HCI connection.
- `BtBridge --connections 2` retains one real HCI controller/address across sequential USB guests for cold-boot
  bond checks. Only departed USB transfer/configuration state is cleared; the next guest sends its own HCI Reset.
  `check-bluetooth-bridge.py` exercises real usbredir negotiation/configuration/interrupt delivery and two Reset/
  Read_BD_ADDR command pairs. It is a host bridge check, not a substituted guest proof.
- Added audioprobe `radio-music` (1 kHz stereo), `radio-stereo` (1 kHz left/2 kHz right), and `radio-voice` through
  existing grants. Voice requires actual peer Answer/HangUp and incoming 1500 Hz microphone samples, checking
  count, mean-square energy and positive-crossing frequency. The independent host decodes actual guest SBC/mSBC
  through AOSP and LC3 through liblc3. `bluetooth_guest_classic.py` checks fresh music capture, phone source ->
  guest mixer -> headset capture, volume/media traffic, both HFP codecs, no-session call/audio refusals, exact OPP
  contents in both directions, unconsented/oversized OPP, and held/selected/detached PAN with a dnsmasq lease and
  Linux ICMP/TCP traffic. These guest scenarios are IMPLEMENTED BUT NOT YET RUN at this update.

Bounded verification actually executed:
- PASS private real-RootCanal/Bumble host probes: five classic controllers initialized/discoverable; Numeric
  Comparison authentication and 2048-byte RFCOMM credit-controlled echo; independent HFP SLC plus mSBC codec
  negotiation; AVRCP parsed/serialized track metadata. Retained controller logs under `/tmp/bt-interop-j_ht1ive`
  and `/tmp/bt-hfp-giayzakz`. The initial HFP probe failed because its temporary AG configuration omitted a
  required constructor argument; the corrected host probe passed. No production guest participated.
- PASS private runner/JSON/isolated-dnsmasq startup and clean-shutdown probe (`/tmp/bt-classic-54ewhbxs`). The earlier
  shutdown probe failed and was fixed as described above; all its owned processes were reaped.
- PASS `.build/bluetooth-oracles/venv/bin/python src/harness/check-bluetooth-bridge.py --work
  .build/bluetooth-oracles/bridge-reconnect`: two USB redirection connections, same real controller address
  da:4c:10:de:00:00; receipt and controller/bridge logs retained.
- PASS `.build/bluetooth-oracles/venv/bin/python src/harness/check-bluetooth-le-peers.py --work
  .build/bluetooth-oracles/le-peers-selftest` (delegated implementation/check, results inspected): actual SMP
  encryption, encrypted CSIS SIRK, PACS/ASCS discovery, VCS write/notify both ways, GTBS call observation/control,
  bidirectional CIS (30/33 independently decoded frames, ~1996/1000 Hz), periodic BASE/BIG and two synchronized
  BIS captures (30/25 frames, 1500/3000 Hz), zero loss/sequence gaps. Summary/events/PCM/LC3 artifacts retained.
- PASS `python3 src/tools/check-bluetooth-independent.py --self-test`; stale, silent, wrong-tone/rate/channel,
  short/lost/gapped LE captures are refused by the host acceptance predicate. PASS Python compilation, rustfmt
  on audioprobe, and git diff --check. No active RootCanal/dnsmasq/peer process remained after host checks.
- NOT YET RUN: final shared guest builds including audioprobe, actual `bluetooth-independent` QEMU acceptance,
  required shared regression gates and final verification-model consistency. No radio completion claim yet.

Precisely observed QEMU limitation: RootCanal 1.13.0 `rust/src/llcp/iso.rs::hci_le_big_create_sync` reads encryption
and broadcast_code into unused variables. The host capability check deliberately used a wrong key and still
received BIS audio. Independent RF broadcast-key rejection therefore cannot be demonstrated by this emulator;
record it as an excluded non-QEMU capability under the user's phase-2 scope. Ordinary encrypted-flag command
plumbing, BASE/BIG/BIS reception, codecs, routing and existing same-team negative command checks remain required.
No physical Bluetooth hardware, RF quality/latency, SIG qualification or member-only corpus was used.

Additional targeted verification: PASS `RUST_MIN_STACK=33554432 cargo check --offline --features development --bin audioprobe --target x86_64-unknown-none` from `src/user/services/core` (0.79 s command elapsed); retained log `.build/logs/end-of-job/qemu-only-20261009/audioprobe-independent-check.log`. This compiles the independent music/stereo/voice probe modes; it is not a guest audio pass.

### Progress recorded at 2026-10-09T19:02:24Z

Final independent OPP gate review strengthened the actual receipt checks in
`src/harness/bluetooth_guest_classic.py`: the redirected inbound object must have
its expected contents and exactly the expected byte count reported by guest `wc`.
The no-consent case now requires an actual peer RFCOMM channel-9 refusal or an
OBEX error response; an arbitrary exception, timeout, or malformed packet is not
a passing refusal. `bluetooth_transfer_peers.py` preserves the specific Bumble
RFCOMM DM/refused status in its control error and distinguishes valid OBEX error
response codes from invalid lengths and unexpected success/continue responses.
The declared-size bound likewise requires an OBEX refusal code.

Verification: Python compilation of both changed modules PASS;
`python3 src/tools/check-bluetooth-independent.py --self-test` PASS;
`git diff --check` PASS. These are syntax/oracle checks, not a guest transfer run.
Guest interoperability remains unperformed pending the final shared build/gate.
A process inventory found no remaining owned RootCanal, Bluetooth peer, bridge,
or private PAN network processes. Audioprobe Rust compilation remains pending
the shared build; only formatting has been checked in this implementation turn.

### Progress recorded at 2026-10-09T19:20:23Z

Pre-freeze review found two independent HID verifier defects: the peer compared
Bumble's uppercase printed address with the guest's lowercase address, and the
untrusted/forgotten negative assertions accepted any failed control request.
The production classic policy maps untrusted L2CAP admission to Security (code
3); its unrecognized inbound peer rejection is HCI unacceptable BD_ADDR (0x0F).
The harness correction now begins from those actual protocol paths. Only
protocol-specific refusal evidence may satisfy these negative cases; pending
implementation and bounded host oracle checks are recorded below when complete.

HID verifier correction completed: `Peer.connection` now parses a BR/EDR public
`hci.Address` and compares address values, reusing the existing connection across
uppercase/lowercase textual spellings. `bluetooth-radio-peers.py::control_error`
preserves the actual Bumble `BaseError` namespace, code and name alongside the
diagnostic exception text. The guest gate requires L2CAP Security Block (3) for
the bonded-but-untrusted keyboard. After forget it accepts that same refusal or
the explicit HCI authentication/key/security/pairing-policy codes
0x05/0x06/0x0E/0x0F/0x18/0x2F; 0x0F is the shipping host's unknown-peer inbound
connection policy, not a generic transport failure. No success is inferred from
timeouts, missing peers, disconnection, resource exhaustion or unsupported PSM.

Bounded checks actually executed: PASS `.build/bluetooth-oracles/venv/bin/python
src/harness/bluetooth-radio-peers.py --self-test` (0.32 s), using real Bumble
protocol exceptions and case-varied existing-address reuse without a controller;
PASS `python3 src/tools/check-bluetooth-independent.py --self-test`, including
25 rejected nonmatching HID outcomes and eight accepted exact protocol outcomes;
PASS `python3 -m py_compile src/harness/bluetooth_classic_peers.py
src/harness/bluetooth-radio-peers.py src/tools/check-bluetooth-independent.py`;
PASS `git diff --check`. The first peer self-test failed because its expected
Bumble error-name spelling incorrectly included `HCI_`; actual pinned Bumble
reports `PIN_OR_KEY_MISSING_ERROR`, and correcting that expectation passed.
These are harness/oracle checks only; the production guest's HID refusal and
stored-key reconnect remain UNPERFORMED until the final independent radio gate.

### IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0194 (2026-10-09T19:26:57Z): ordinary LE oracle completion

Final scope review found the independent radio gate covered LE Audio but omitted
part b's ordinary LE privacy, pairing models, application GATT grants and
cross-transport keys, and part c's HOGP input. Those are software work: pinned
Bumble provides their SMP/GATT/IRK/CTKD primitives, and RootCanal implements the
resolving list; no missing capability has been demonstrated. Existing
`bluetooth-le` and mouse/reconnect gates remain historical same-team evidence,
not independent proof. The new work reuses the existing btgatt policy/grant and
shipping operator path with separate Bumble peripherals and exact peer-side
observations. The review also identified missing classic/HSP/detail and GOEP
L2CAP oracle paths; those are delegated to the other implementers, with findings
and verification to be appended without relabeling the old fixtures.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0194 (2026-10-09T19:27:06Z):

Parallel continuation read the full classic foundations/pairing, audio contract, music, voice and oracle/completion sections. The independent classic harness currently exercises A2DP and HFP mSBC/CVSD, but not a real HSP channel, and lacks explicit AVDTP delay, AVRCP NOT_IMPLEMENTED, HFP gain/battery and several pairing-policy assertions. I own the classic peer and music_voice gate additions, while root owns transfer and agent0194 owns ordinary LE and top-level orchestration. No new runtime change or guest pass is claimed by this initial record.

### Independent Object Push coverage review (2026-10-09T19:29:16Z)

Reviewed the complete part-g Object Push requirement against `bluetooth_transfer_peers.py`, `bluetooth_guest_classic.py::transfer`, and production `BluetoothService::push_records`. The current independent-transport scenario offers only RFCOMM, while production selects GOEP L2CAP in enhanced retransmission mode whenever SDP attribute 0x0200 supplies a PSM. Pinned Bumble has the required ERTM processor, so this is missing test coverage, not a physical-hardware exclusion. The existing same-team OBEX parser will be reused over Bumble ERTM as the plan explicitly permits. Guest GOEP verification remains unperformed at this review.

### Independent Object Push transport implementation (2026-10-09T19:32:24Z)

`TransferPeer::offer_l2cap` now publishes/removes the real SDP GOEP PSM attribute. Its separate Bumble ERTM server uses PSM 0x1001, MTU 4096 and MPS 128. The same explicitly same-team OBEX receiver works over either transport and reports actual ERTM processor, completed final PUT, name, byte count and digest. The guest `transfer` scenario sends the same redirected file once to an RFCOMM-only record and once to a GOEP record; the result must name the actual selected transport and exact object. The classic control handler forwards `opp-offer-l2cap`.

PASS: `.build/bluetooth-oracles/venv/bin/python src/harness/check-bluetooth-opp.py --work .build/logs/end-of-job/qemu-only-20261009/opp-host-transport` (exit 0). Two real RootCanal/Bumble controllers authenticated/encrypted their connection, discovered the changing SDP record, and carried the exact 4915-byte object over RFCOMM and ERTM, with 700-byte OBEX chunks segmented across 128-byte ERTM PDUs. Both receipts have SHA-256 `4278f3e3fa6810a41f21503ba32cb2cce83eb7ab338e204e26e0a122cfe2c75b`. Summary, event trace and controller log are retained in that directory. Python compilation and diff check PASS. This validates host test equipment only; the Liber guest Object Push scenarios remain UNPERFORMED.


Classic independent-oracle progress 2026-10-09T19:33:14Z:

Implemented in bluetooth_classic_peers.py and bluetooth_guest_classic.py: Bumble AVDTP sink advertises delay reporting and sends150ms before stream opening; guest asserts the resulting160664us endpoint latency. AVRCP pass-through response codes are returned and every Play/Pause/Next/Previous press/release must be NOT_IMPLEMENTED. HFP checks operator60 to speaker gain9 and peer gain15 back to100, battery80 in btctl status, microphone command acceptance and invalid16 refusal. Added genuine HSP: independent SDP finds the HSP AG on RFCOMM2; only shared AT framing is reused, with no HFP SLC/codec negotiation, CVSD duplex, one-button answer/hangup and no unsolicited SCO when no application voice session exists. The microphone-level propagation criterion remains unclaimed pending the owner's shared-versus-separate-control decision.

The real HSP path exposed a production omission: voice_opened created a Gateway already in Connected state, while endpoint publication waited for HFP's CMER or codec event which HSP never sends. Added immediate voice_offer only after a newly admitted HSP_CHANNEL link is stored in bluetooth_service/voice.rs. The independent HSP guest fixture is its behavioral regression; compilation/guest execution remain pending the coordinated rebuild.

Added classic_pairing(g,pair): independent protocol attempts for P-192-over-P256, unauthenticated-over-authenticated and legacy-over-SC must fail; after each, restoring the independent peer's original key must reconnect encrypted without a new pairing, proving the guest retained its bond. An unpaired incoming headset is refused while no watcher exists (explicit HCI security status, not timeout), then accepted through a real incoming-consent prompt. Ordinary legacy pairing is refused before explicit pair-legacy; its successful PIN bond is then replaced by an expressly forgotten and newly paired SC bond as the original gate expected. Top-level integration is owned by agent0194. Added root-requested opp-offer-l2cap forwarding action. Python syntax, Rust formatting and diff checks PASS; no new radio/guest or broad test result is claimed.


Classic oracle review update 2026-10-09T19:36:11Z:

Parent review tightened refusal evidence: the helper now requires each exact causal BluetoothService downgrade/legacy-policy message in the command window, not merely the generic CLI failed result, and still verifies the saved original peer key reconnects afterward. Generic failure, TimedOut and bonded success are rejected by the bounded classifier probe. Corrected the delay fixture described above:150ms equals the existing fallback and could not prove propagation, so the independent sink now sends a non-default180ms during OPEN after configuration, and the guest polls the same endpoint ID for190664us after the peer receives acceptance. No specification ceiling changed; this is discriminating scenario data.

Ran .build/bluetooth-oracles/venv/bin/python .build/logs/end-of-job/qemu-only-20261009/classic194/handler-probe.py: PASS in0.33s. It exercises pinned Bumble capability/DelayReport command and accepted/refused response types plus the negative guest classifier cases. Log: classic194/handler-probe.log. Python syntax and diff checks PASS. This is a bounded harness-handler check, not an independent radio exchange or guest acceptance; those remain deferred.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0194 (2026-10-09T19:41:49Z):

Independent review of the application GATT subscription path found that descriptor discovery extended to the entire granted service, so it could select a following characteristic's CCCD; returned descriptor handles were not validated against the request/grant; only the first 16-bit descriptor page was inspected; and any non-error reply completed the CCCD write. The newly added property selection correctly chooses bit 1 for indication-only characteristics but does not close these existing selection and validation defects. Ownership of the narrow repair passed to this implementer. The required scope is exact value/descriptor association, advancing bounded discovery for both descriptor UUID formats, existing ATT response validation, and precise success/failure replies, with host tests of the production pure logic. No repair or new guest result is claimed at this initial record.


### GATT subscription repair and focused verification (2026-10-09T19:48:31Z)

Implemented the reviewed subscription defects in the production pure leaf `service_logic::att::subscription::Subscription`, registered under the existing ATT module and used by `bluetooth_service/gatt.rs`. The walk finds the exact requested characteristic value, derives notification setting 1 or indication setting 2 from its actual properties, discovers the next characteristic declaration to bound this value's descriptor range, and advances across both 16-bit and 128-bit descriptor pages. It validates expected ATT opcode, MTU, complete entry framing, ascending in-range handles, declaration/value relationships, matching error request and range, and the exact empty Write Response. The Bluetooth base UUID form of CCCD is recognized. Both property discovery and descriptor discovery are capped at the existing 32-entry ATT bound; no peer-sized table is retained. A CCCD from the next characteristic or outside the granted service is refused before any write.

The adjacent grant/failure review found that a dropped grant could still authorize a queued CCCD continuation, and a peer silent at a subscription step could hold the bearer forever. `next_gatt` now discards queued operations whose grant no longer exists. A subscription has one 30-second deadline covering queueing and every discovery/write step; no existing ATT timer constant existed to reuse. Queued expiry answers TimedOut without disturbing another operation's bearer. Outstanding expiry answers TimedOut, ends its queued application operations, quarantines that bearer and requests disconnection, preventing a late ATT reply from completing a newer operation. `bluetooth_service.rs` only adds the GATT deadline and timer hooks. Other application operation deadlines were not redesigned. A one-line adjacent fix makes the battery response's pdu[4] access lazy: an unexpected short response previously indexed it even when the length condition was false.

Verification actually executed: PASS `RUST_MIN_STACK=16777216 cargo +nightly-2026-06-16 test --manifest-path src/user/services/logic/Cargo.toml --target x86_64-unknown-linux-gnu --lib att::subscription::tests` (10 passed, 0 failed, 957 filtered; 4.43s compile and <0.01s tests). The external `att/subscription/tests.rs` covers notification and indication modes, following-characteristic/out-of-grant CCCDs, wrong/ragged/oversized replies, mixed UUID pages, precise denial, bounded/repeated pagination, handle 0xffff, the unchanged deadline across pages and a late Write Response refused after expiry. An earlier 8-test run before adding deadline and last-handle coverage also passed. PASS `RUST_MIN_STACK=16777216 cargo +nightly-2026-06-16 check --bin bluetooth_service --target /data/yellow/libersystem/src/user/x86_64-unknown-none.json` from `src/user/services/core` (3.85s). PASS targeted rustfmt and `git diff --check`.

UNPERFORMED here: the rebuilt guest ordinary-LE indication/notification grant scenario, live owner teardown and silent-peer/quarantined-bearer integration, and cross-target final builds. The old same-team tag fixture's generic Read By Type/Find Information handlers were reviewed and support the added boundary query; that source review is not an executed compatibility pass. The independent radio checkbox remains open pending the parent's final gate.


Ordinary LE independent-oracle implementation progress (2026-10-09T19:50:17Z):

Added `bluetooth_generic_le_peers.py`, `bluetooth_guest_le.py`, `check-bluetooth-generic-le.py` and the peer contract document. The real Bumble/RootCanal peers implement a private-address HOGP/GATT tag, two passkey peripherals and one public-address dual-transport CTKD peer. The production guest gate now includes all LE pairing models, three simultaneous links, explicit legacy/authentication downgrade refusals and original-key reconnect, both CTKD derivation directions, actual GATT writes/notifications/confirmed indications, denied grants before trust/after forget, long HID report map and console input, peer RPA change, production fifteen-minute host RPA rotation, service restart and cold-boot LE key reuse. No production timer is shortened. Guest verification remains UNPERFORMED.

Required production integration changes: `btctl` accepts an additive `-k public|random|bredr` selector for ambiguous dual-radio records, including pair defaults, and an optional radio kind for `pair-legacy` while preserving its old BR/EDR default. LE pairing progress now reports the committed resolved identity, and the CLI uses that identity to report the actual bond level. `btgatt indicate` chooses the indication-only FFF2 characteristic; the shell admits the probe's arguments. PermissionManager logs the actual typed GATT mint error before refusing the required-grant launch; the oracle requires exact Denied/NotFound plus absence of probe execution, rather than confusing an arbitrary launch failure with a policy decision. GATT's missing indication CCCD selection and its inherited descriptor/grant validation defects were handed to the independent reviewing implementer; that implementer's appended record describes the final production state machine and tests.

The host equipment check uses only actual independent SMP/ATT/HCI exchanges. Preliminary runs found (1) Bumble's installed advertising restart callback racing a peer RPA change, fixed with harness-owned scheduling and actual HCI address programming; (2) pinned Bumble0.0.235 selecting the wrong legacy responder key, fixed with a documented narrowly scoped key-provider adapter selecting Bumble's distributed ltk_central only with matching EDIV/Rand; (3) an invalid harness LINK_KEY distribution request on BR/EDR, corrected to request link-key derivation only on LE; and (4) `Peer.connection` reusing an LE public-address connection for a BR/EDR request, fixed by checking the actual transport. The final CTKD assertion requires BR_EDR and records the real new BR/EDR encrypted-link event. The key-provider adapter changes no SMP crypto, derived bytes or controller encryption verdict; it is explicitly recorded as same-team equipment adaptation, not hidden inside an independence claim.

PASS `.build/bluetooth-oracles/venv/bin/python src/harness/check-bluetooth-generic-le.py --work .build/bluetooth-oracles/generic-le-selftest-5`: actual NC, both SC passkey directions, legacy Passkey/JW and stored-key encryption, GATT notification plus confirmed indication, 300-byte long HID map read over two ATT responses and real key reports, independent RPA/IRK resolution after address change, and actual BR/EDR→LE plus LE→BR/EDR CTKD. Summary/controller trace/events retained. Earlier diagnostic runs1–3 failed as described; run4 exposed the inadequate mixed-transport assertion and is not used for CTKD acceptance. Scenario snapshot copying and live-HID-CCCD readiness were subsequently tightened; final rerun remains pending the reviewer's narrow held-ATT negative controls. No guest pass is inferred from this host equipment pass.

PASS x86 targeted checks: from `src/user/apps/tools`, `RUST_MIN_STACK=33554432 cargo check --offline --features bluetooth-client --bin btctl --target x86_64-unknown-none` (0.78s); from `src/user/services/core`, `RUST_MIN_STACK=33554432 cargo check --offline --features development --bin btgatt --bin permission_manager --bin shell --target x86_64-unknown-none` (3.16s). The first btctl command omitted the existing optional bluetooth-client feature and failed unresolved-import compilation; the corrected command above passed. PASS Python compilation for the new peer/helper/check and modified runner/gate, radio-runner protocol-error/address self-test, independent-gate negative/audio oracle self-test, Rust formatting and git diff --check. No full build, architecture sweep, guest run or long suite was run by this implementer at this stage.


### Subscription failure guest-oracle preparation (2026-10-09T19:56:19Z)

The independent ordinary-LE scenario had positive notify/indicate and mint denial coverage but no held-request timeout or owner teardown while operations were queued. Added development-only `btgatt timeout` and `btgatt revoke`. The former requires the typed TimedOut response after 30–45 seconds. The latter pipelines a subscribe and distinctive `revoked-write`, uses an immediately answered cached-services request on the same grant as a FIFO barrier proving the service received both requests, allows ten seconds for the host to observe the withheld request, then closes the grant and ends its owner. Normal notify/indicate behavior is unchanged.

The host `GenericPeer` has explicit `att-hold`/`att-release` controls for one actual Find Information request at custom characteristic 0's descriptor range. This is SAME-TEAM fault injection on Bumble, not independent failure behavior. The request is retained intact; a release on the identical live connection uses Bumble's normal dispatcher. A retired connection is never substituted with a replacement that reuses its numeric handle. The new `subscription_failures(g)` guest scenario runs after the normal independent notify/indicate pair. It observes the held request, waits for the development probe's actual completion, releases the live request after owner teardown, and completes a new ordinary grant as a bearer barrier. Exact peer-side ATT writes must be only that fresh grant's encrypted CCCD=1 and hello; an abandoned CCCD or revoked payload fails. The timeout case additionally requires actual old-connection retirement, rejects replay onto a replacement, waits for independently observed encrypted reconnection, and completes a fresh grant with the same exact write check.

PASS focused development probe check from `src/user/services/core`: `RUST_MIN_STACK=16777216 cargo +nightly-2026-06-16 check --features development --bin btgatt --target /data/yellow/libersystem/src/user/x86_64-unknown-none.json` (0.77s). A first invocation at repository root failed before compilation because it had no Cargo.toml; the corrected crate-directory invocation above passed. PASS `.build/bluetooth-oracles/venv/bin/python .build/logs/end-of-job/qemu-only-20261009/gatt194/check-att-hold.py` (0.33s): real pinned Bumble PDU fields, exact request hold, unchanged live release, retired/reused-handle refusal, unrelated pass-through and actual write observation. PASS Python compilation and diff check. Probe source was frozen and reported to the parent before all-target build retry2 began. Live host control exercise is owned by agent0194; guest timeout/revocation remains UNPERFORMED until the final radio gate.


Final ordinary-LE host equipment and load-observation update (2026-10-09T19:57:21Z):

PASS `.build/bluetooth-oracles/venv/bin/python src/harness/check-bluetooth-generic-le.py --work .build/bluetooth-oracles/generic-le-selftest-final` (exit0; stdout/stderr `.build/bluetooth-oracles/generic-le-selftest-final.log`). All seven normal radio scenarios passed after live HID CCCD readiness and immutable scenario snapshots were added. The same run additionally sent and withheld a real ATT descriptor request, released it on its original live encrypted bearer and received Bumble's real CCCD response, then proved an old request is discarded after a real disconnect/address rotation. These are expressly same-team fault controls on the independent server, not a guest timeout/owner-lifetime pass. The HOGP report map is301 bytes (45-byte keyboard map plus256 Push/Pop bytes); the previous paragraph's300-byte figure was an off-by-one prose error, and the real long read crossed two responses. No RootCanal or checker processes remained after the checks.

The independent guest gate now observes `graph bluetooth_service` and `usage` DURING each clear/encrypted BIG→decode/mix→CIS stream, before broadcast stop. It requires the six resource rows, finite limits and usage within those limits, preserves exact output/values, and requires fresh independently decoded frames after the snapshot. It also retains the after-load snapshot. This is a live occupancy observation; allocated heap pages include heap high-water, while no IPC-queue high-water claim is made. Added positive and missing/error/incomplete/over-limit accounting oracle checks; `python3 src/tools/check-bluetooth-independent.py --self-test`, Python compilation and `git diff --check` PASS. The actual load values, final frozen development build, guest timeout/owner-teardown scenarios, fifteen-minute privacy rotation and complete production USB interoperability remain UNPERFORMED pending the parent's final coordinated QEMU window. No radio completion rating or checkbox was assigned.


Independent-gate prerequisite and departure correction (2026-10-09T20:13:36Z):

Read-only preflight confirmed pinned Bumble0.0.235 and RootCanal1.13.0 imports, actual AOSP SBC encoder/decoder ABI loading, Google SBC/liblc3 loading, all three cached codec source archive digests matching pins, dnsmasq2.91 with DHCP, iproute2/unshare/QEMU, the TUN device, and effective Linux SYS_ADMIN/NET_ADMIN/NET_RAW capabilities. No pre-existing radio/bridge/PAN/dnsmasq processes or named namespaces were present;74GiB remained free. The newly added GATT fault controls and Domain observations retain the existing ready.json groups, JSON ok/result envelope and event cursor protocol. No guest or full test was run for that preflight.

Source review found an actual departure-fixture race: Bumble auto_restart is installed when a link connects, so merely disconnecting the independent earbuds immediately re-advertises them and races trusted reconnection. Changed only host harness code: LE peers own restart scheduling; `disconnect` retains normal restart, while new `depart` disables advertising before the actual disconnect. The guest requires advertising absent and links gone, tests the surviving set then complete endpoint removal while both bonds remain trusted, and only afterward removes absent peers' audio trust. That last step releases the outstanding LE filter-list initiation before the separate fifteen-minute own-RPA test; no production timer or runtime source was changed. This states the rotation experiment's controller precondition rather than pretending an initiating controller can change its random address.

The bounded real advertising/reconnect probe initially FAILED with missing stored keys. It exposed a second fixture configuration error: earbuds advertised static random A1/A2 while Bumble's implicit SMP configuration distributed different RootCanal public identities. Explicitly selected RANDOM identity for these documented peers and for the static-address host checker central. The updated equipment check source also includes normal encrypted reconnect and deliberate departure.

PASS `.build/bluetooth-oracles/venv/bin/python .build/bluetooth-oracles/le-departure-preflight.py` after that correction, with final artifacts `.build/bluetooth-oracles/le-departure-preflight-final/` and adjacent `.log`: actual independent controller connection, SC pairing, disconnect/re-advertisement, stored-key encrypted reconnect, then departure with zero links and advertising false. This short probe ran no audio and no guest. The initial failure log/artifacts remain under le-departure-preflight. Python compilation, the independent gate's existing self-test and git diff --check PASS. The full host CIS/BIS check was not rerun solely for this advertisement change; its prior evidence and the pending production QEMU gate remain distinct.

## IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0194 (2026-10-09T20:52:19Z):

The owner selected **“Samostatná hlasitost mikrofonu, rozšířit API”** for the previously reported HFP/HSP microphone-gain design decision. I reviewed the audio and Bluetooth IDLs, the generated client boundary, AudioService's playback/capture scaling and device inventory, the sound-card driver wire, `audioctl`, and the HFP gateway and voice endpoint integration. The existing `AT+VGM` parser emits a microphone event but BluetoothService discards it; only speaker gain has an outbound control and AudioService has only the older combined device level. This section implements the authorized additive microphone API. Existing operation numbers and serialized device/endpoint records will remain intact. No new guest verification has run; the current all-target build and radio harness evidence predate this change.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0194 (2026-10-09T20:53:20Z):

Following the owner's approved separate microphone-gain API and the AudioService implementer's request, I reviewed the existing audio driver wire, legacy combined capture scaling, Bluetooth endpoint stream and kernel AudioService fixture. The public device records and driver format remain unchanged; the regression work extends only the existing kernel test fixture to exercise actual PCM capture and playback through the new microphone control. This record precedes test edits. No build or guest has been run for this addition.


Microphone API kernel regression preparation (2026-10-09T20:55:42Z):

Added `kernel.services.audio_service_separates_microphone_gain_without_changing_legacy_capture`, registered beside the existing AudioService tests and implemented as a narrow `MicrophoneVolume` scenario in the existing `src/kernel/tests.rs` service fixture. The stand-in provider deliberately refuses CMD_FORMAT, exercising the real legacy format fallback. Actual public capture PCM proves historical combined gain remains active until the first explicit microphone set; reading the new getter alone does not change that behavior. With microphone 50, every captured sample has quarter amplitude while actual playback remains full; a later speaker level 25 changes only playback to one-sixteenth amplitude. Microphone bounds 0/100 are checked on PCM, Invalid covers level101 and an output-only device, and unknown/withdrawn IDs return NotFound. Invalid requests leave the prior gain and other device unchanged. No production source was edited by this implementer.

PASS `rustfmt +nightly-2026-06-16 --edition 2024 --config skip_children=true src/kernel/tests.rs src/kernel/test_suites/services.rs` and scoped `git diff --check`. An initial direct rustfmt invocation omitted --edition2024 and refused existing let-chain syntax before formatting; the corrected command above passed. Compilation and guest execution remain UNPERFORMED pending generated microphone bindings and the parent's coordinated final build window. Earlier fifteen-test guest passes predate this added API and do not verify this test.


Microphone kernel fixture compile verification (2026-10-09T20:56:45Z): PASS `RUST_MIN_STACK=33554432 cargo +nightly-2026-06-16 check --tests --target x86_64-unknown-none` from src/kernel (7.88s), after canonical generated bindings became available. Exact compiler log: `.build/logs/end-of-job/qemu-only-20261009/microphone-kernel-check.log`. The actual new guest test remains UNPERFORMED; no prior guest pass is reused as this API's proof.

### Progress on the authorized independent microphone API (2026-10-09T21:02:20Z)

Implemented additive `audio-control.microphone-volume` / `set-microphone-volume` operations 6/7, Bluetooth-side microphone operations 5/6, and the appended `AudioEvent::MicrophoneVolume` tag 4. Existing device/endpoint records, event tags 0–3 and old operation numbers remain unchanged. `audioctl microphone DEVICE [LEVEL]` and the shared audio-client wrapper/provider expose the API. No sound-card driver command was invented: existing providers use AudioService's software capture scaling; their legacy combined level remains until the first explicit microphone setting, and merely reading the microphone level never changes that compatibility state. An explicit microphone setting then remains independent of subsequent speaker changes. Devices without recording input, including route endpoints, reject microphone control; values above 100 and unknown devices retain their typed failures.

HFP/HSP `Gateway` now retains both gains, including remote values received before its endpoint is offered. Remote `AT+VGM` updates the microphone alone, and operator control sends `+VGM`. The applied hardware value is read back after 0–15 quantization; hardware microphone samples are not scaled again. Initial offers and re-subscribed snapshots carry the current microphone value after Arrived. An unreported hardware Bluetooth microphone returns Unsupported, rather than pretending its speaker gain is the microphone gain. Accepted operator changes also enter the same FIFO event stream, so an older queued remote VGM cannot leave AudioService's final value stale.

Review found and corrected two material issues before declaring the source ready. The first draft synchronously queried every arriving input endpoint; an older stack silently ignores the new opcode, so this could block audio. Arrival now uses only the event stream and sends no new operation to an older stack. An explicit microphone setter has a two-second deadline, and any uncertain transport/framing response retires the Bluetooth control/event connection and its endpoint channels, preventing a late reply from completing a later request. Only an actual Unsupported response permits software fallback; other failures propagate. Second, the existing convenience RFCOMM sender discards enqueue failure. The new mic path instead uses `hfp::Gateway::queue_microphone_gain`, which checks an open DLC and the existing `Session::room` bound, queues the exact line, and only then commits the gain. Closed/full/invalid refusals preserve the previous state. Existing speaker semantics were left intact.

Canonical generation initially failed its conservative ABI guard because extending a variant list is classified as breaking. `./gen.sh --accept-breaking` then PASSED for this intentional pre-release addition. The old manifest comparison passed: every previous audio/Bluetooth record and method remains identical, and the old event variants are an unchanged prefix. Runtime protocol-info still names `liber:bluetooth` major 1; the broker audio authority has no fingerprint negotiation. The host compatibility test freezes the pre-change event reader and proves that a new microphone message is skipped, then subsequent existing speaker/call/departure frames are read normally; unknown frames carry no capabilities and are consumed separately by the existing AudioService receive loop.

Verification so far (logs under `.build/logs/end-of-job/qemu-only-20261009/`):

- PASS: from repository root, `RUST_MIN_STACK=33554432 cargo test --offline --manifest-path src/user/services/logic/Cargo.toml --target x86_64-unknown-linux-gnu --lib hfp:: -- --nocapture` — all 9 HFP tests, including independent inbound gains in HFP/HSP and invalid bounds (`microphone-hfp-tests-final.log`).
- PASS: the same manifest/target with filter `microphone` — 2 tests, including actual paired RFCOMM sessions and exact VGM bytes, closed DLC refusal, exhausted credits/full queue, 100 repeated refusals without queue growth or changed gains (`microphone-logic-tests.log`).
- PASS: `RUST_MIN_STACK=33554432 cargo test --offline --manifest-path src/user/libs/protocol/bluetooth-proto/Cargo.toml --target x86_64-unknown-linux-gnu` — 42 tests including old-reader compatibility (`microphone-wire-tests.log`).
- PASS: from `src/user/services/core`, `RUST_MIN_STACK=33554432 cargo check --offline --features development --bin audio_service --bin bluetooth_service --target x86_64-unknown-none` (`microphone-services-check.log`; final event-ordering recheck follows).
- PASS: from `src/user/apps/tools`, `RUST_MIN_STACK=33554432 cargo check --offline --features audio-client --bin audioctl --target x86_64-unknown-none` (5.84 seconds; terminal output).
- PASS: `.build/bluetooth-oracles/venv/bin/python src/harness/bluetooth-radio-peers.py --self-test` — the pinned Bumble parser receives real `+VGM`/`+VGS` bytes and the fixture preserves separate microphone/speaker observations (`microphone-peer-selftest.log`); Python syntax and `git diff --check` pass.
- Earlier HFP test commands from the user crate directory failed first because its configured bare-metal target has no std/test, then because that directory's build-std config conflicts with host std. Running the host command from repository root corrected the invocation; these failed invocations are not test passes. An initial service check found a private-method access in the new getter; a narrow voice getter corrected it before the passing check.

The USB collaborator added a dedicated kernel fixture for legacy combined capture, non-mutating query, actual independent capture/playback amplitudes, endpoints/errors and failed-update preservation; its focused kernel compilation passed. Its QEMU execution is still UNPERFORMED. The independent radio gate now requires the API/device microphone changes separately from speaker changes for mSBC, CVSD and HSP, including initial gain, quantization and invalid remote gain. Those actual guest assertions, the new all-target restage and the full radio gate remain UNPERFORMED; no prior equipment or pre-change guest result is relabelled as this implementation's pass.

### Microphone source freeze and final targeted checks (2026-10-09T21:04:19Z)

Final canonical `./gen.sh` PASSED, followed by `./gen.sh --check` PASS (38 packages and aggregate, no generated/profile drift; 15 seconds, `microphone-gen-check.log`). The final affected-service command above PASSED again after the FIFO notification fix (2.11 seconds, `microphone-services-check-final.log`). The dedicated kernel fixture compiled successfully (7.88 seconds, `microphone-kernel-check.log`, collaborator-owned check). Python syntax and final `git diff --check` passed. Runtime, CLI, generated bindings and the independent gain assertions are now frozen, with no owned process left running. The parent is scheduling all-target restaging and the new guest kernel/radio verification; those results remain unperformed at this freeze and must be appended when actually obtained.


Bluetooth audio subscriber lifetime correction (2026-10-09T21:15:41Z):

The live-headset microphone restart review found a real pre-existing lifetime defect: BluetoothService did not watch the AudioRoot subscriber stream, so stopping AudioService left subscriber nonzero and every replacement endpoints() request returned Again until an unrelated profile event failed to send. Open PCM channels holding capture/playback requests were also absent from the PCM waitset. Added subscriber closure polling and a waitset entry in bluetooth_service.rs, plus closure polling before new subscription admission in audio.rs. Failed event sends defer cleanup to Stack so the profile hooks run. Cleanup clears and relays the call state, removes all PCM ownership together, and invokes the same existing LE/A2DP/voice close hooks as a normal channel closure. Removing the whole ownership set first prevents an LE voice close from restarting a dead subscriber's media channel. Offered endpoint identities and per-profile microphone values survive for the replacement snapshot.

PASS from src/user: `RUST_MIN_STACK=33554432 cargo check --offline --manifest-path services/core/Cargo.toml --bin bluetooth_service` (4.31 seconds; `.build/logs/end-of-job/qemu-only-20261009/bluetooth-subscriber-lifecycle-check.log`) and scoped `git diff --check`. The three preceding frozen microphone library builds all passed (x86_64 369s, aarch64 368s, riscv64 363s; microphone-libs-ARCH.log/status in the same directory) with matching provider/consumer inventories; none contains this subsequent lifetime fix. The six foreign provider hashes remain unchanged, so their regenerated audit executables still have the matching provider identity. Actual AudioService restart with a still-connected non-default microphone, the new microphone kernel case and independent radio interoperability remain UNPERFORMED pending the coordinated final build and QEMU gates.


Subscriber overflow follow-up (2026-10-09T21:18:01Z): the same bounded retirement path now handles a full event queue, not only a closed consumer. Previously try_send_outcome::Stalled silently lost state changes; in particular an accepted microphone echo could disappear behind older remote values. AudioService already treats stream closure as bluetooth_lost(), withdraws old device instances and retries the broker/subscription, whose snapshot contains the actual retained profile gain. No queue growth or new retry protocol was added. Final focused BluetoothService check PASS (0.40s, bluetooth-subscriber-lifecycle-check-final.log), and the Bluetooth implementer reviewed the lifetime hooks and reconnect path. The new live-headset restart fixture will test snapshot restoration without any intervening remote gain event. Local event-queue saturation itself has not been executed; this branch is source-reviewed, not claimed as a guest test pass.

Timing correction: the final subscriber check log reports 0.51s, not the preceding paragraph's 0.40s transcription. Exit0/PASS is unchanged.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0194 (2026-10-09 21:23:35 UTC):

Read-only independent review of the subscriber cleanup found a required adjacent lifecycle gap: voice_down leaves an outstanding synchronous setup pending, but voice_connected accepted a later successful completion without a live Voice PCM owner. The same completion returned without disconnect when the ACL peer had already disappeared. Implementing a narrow completion admission check while preserving the outstanding setup flag until its actual completion, allowing a replacement subscriber that has already reopened the endpoint. The event-stream waitset, failed-stream cleanup, bulk PCM removal and resnapshot logic are owned by the other implementer; no edits to those files here.


Implementation/review continuation (2026-10-09 21:25:03 UTC):

`bluetooth_service/voice.rs::voice_connected` now disconnects successful synchronous completions when the ACL peer is gone or no matching live Voice PCM owner remains. It clears setting_up at the actual completion, preserving the outstanding request across subscriber loss and allowing a replacement that already reopened the endpoint to use its completion. Orphan completions no longer set voice.sco, construct codecs or enable voice transport. No unrelated voice behavior was changed.

Independent review of the coordinated subscriber cleanup found the event handle included in the waitset, failed/full streams retired on the next loop/admission, call state reset and relayed, all PCM entries removed before profile close hooks, and endpoint/gain metadata retained for replacement snapshots. A queued Arrived event can survive producer closure because kernel channel receives drain queued messages before PeerClosed. The coordinated AudioView::open guard now polls retirement and refuses without a current subscriber, preventing such stale events from opening unowned PCM. Current AudioService drains the stream before scheduling further periods, so no permanent held-PCM regression from that race was claimed. No other concrete blocker was found in the reviewed cleanup.

PASS: `RUST_MIN_STACK=33554432 cargo +nightly-2026-06-16 check --bin bluetooth_service --target x86_64-unknown-none` from `src/user/services/core` passed after the voice fix (1.51 s), and again after the latest open admission guard (0.54 s). Rustfmt and targeted diff whitespace check passed. NOT PERFORMED: late synchronous-completion guest fault injection; existing independent peers cannot hold/release this HCI completion, and their restart/microphone assertions do not prove this race. No tautological pure-boolean test was added. The coordinated actual restart/microphone integration remains pending final artifact staging. Voice source is frozen.


Late-SCO lifecycle regression preparation (2026-10-09T21:31:31Z):

Added a development-only audioprobe radio-voice-close scenario using existing application voice grants: it opens a real session, declares Active, waits for the independent headset's actual HFP HangUp on the public commands stream, then acknowledges call/session closure without waiting on PCM that the held SCO cannot yet carry. The classic peer now has one bounded held SCO request; release calls Bumble's existing accept path, and stale connection identity/withdrawal prevents release. No HCI completion or guest verdict is fabricated. The classic guest gate performs the close, stops AudioService, releases the actual headset acceptance, requires a real successful SCO completion followed by explicit guest disconnect reason0x13, and restarts AudioService with microphone53 restored. The immediately following normal duplex call is the recovery proof. The existing microphone energy, exact ERROR, no cross-gain notifications and live-headset snapshot checks remain intact.

PASS from src/user: `RUST_MIN_STACK=33554432 cargo check --offline --manifest-path services/core/Cargo.toml --features development --bin audioprobe --bin bluetooth_service` (1.34s; bluetooth-late-sco-probe-check.log). PASS pinned `bluetooth-radio-peers.py --self-test` (bluetooth-late-sco-peer-selftest.log): no command before release, actual Bumble accept class/address, stale same-handle replacement rejected, held request discarded on ACL departure. PASS Python compilation, `check-bluetooth-independent.py --self-test`, scoped rustfmt --check and git diff --check. Logs are under `.build/logs/end-of-job/qemu-only-20261009/`. Actual delayed acceptance through RootCanal/QEMU remains UNPERFORMED until the final gate; these host checks prove fixture mechanics and compilation only. The earlier audit note saying no delayed-completion control existed is historical: this continuation adds that concrete control, not a claimed guest pass.

### Independent microphone synchronization assertions and lifecycle review (2026-10-09T21:36:02Z)

Strengthened the independent classic gate for all mSBC, CVSD and HSP runs. A same-RFCOMM unsupported-command ERROR is now an ordering barrier around gain observations; the exact observed gain-event sequence must contain only the expected VGS or VGM event, with no duplicate, stale or cross-control echo. Invalid `AT+VGM=16` must yield the actual HFP `ERROR` name (another HFP error or a timeout cannot pass). The public microphone getter is checked at the canonical 0..15 quantization, including operator50 -> peer8 -> API53.

A live-headset AudioService stop/start must restore nondefault microphone47 from the endpoint snapshot before any subsequent remote gain change. The gate rejects ACL/HFP recreation during that assertion. The independent peer also records a calibrated post-device-gain microphone PCM reference using the actual transmitted CVSD samples or independent AOSP decode of the transmitted mSBC frames. The guest's accumulated mean-square capture energy at microphone53 must remain within 0.35..1.65 of that reference; the unintended additional software capture gain would be approximately0.079 and cannot pass. This compares actual transported PCM, without inventing a physical device's gain curve.

The parent subsequently added a held real Bumble SCO-acceptance fixture and fixed subscriber closure/overflow handling, and the USB agent fixed late successful SCO completion without a current PCM owner. Read-only review confirmed the new fixture closes application and AudioService ownership before release, demands actual SCO completion followed by guest disconnect reason0x13, then checks retained53 after resubscription before the normal duplex run. Runtime teardown removes all PCM ownership before profile close hooks and retains endpoint/gain state for the new snapshot. No additional definite issue was found in this scoped review.

Verification distinction: Python syntax passed for the four microphone assertions; the parent separately records its later pinned peer/gate self-tests and focused staged-source checks. The actual independent Bluetooth QEMU gate, live microphone energy/snapshot proof, and late-SCO interoperation remain UNPERFORMED here pending the final image. Queue saturation remains source-reviewed, not a measured guest result. Previous audit content is preserved.

Verification continuation (2026-10-09T22:13:43Z):

The frozen profile/readback/microphone continuation was built on all three targets with `RUST_MIN_STACK=33554432 LIBER_DEVELOPMENT=1 ./build.sh --arch ARCH --part libs`: x86_64 PASS442s, aarch64 PASS443s, riscv64 PASS439s; each staged inventory matches121 providers and121 consumers. The six foreign/runtime provider SHA256 identities remain unchanged from the successful canonical foreign regeneration, so no new foreign regeneration was required. Serial `./build.sh --arch all` with the same development/stack environment PASS271.24s; `./build.sh --arch all --part volume --kernel-on-volume` PASS221.06s. Exact logs are under `.build/logs/end-of-job/qemu-only-20261009/post-join-*`; `post-join-stage-results.json` preserves every command, environment, exit and elapsed time.

`RUST_MIN_STACK=33554432 ./check.sh --refresh dynamic-report` PASS405.10s, `./check.sh --gate verify-model` PASS28.97s, and `./gen.sh --check` PASS16.04s. The registration check preceded compilation of the new microphone test and honestly reported it declared but not yet built. The subsequent actual x86_64 SMP4 kernel run compiled and ran all16 explicitly selected cases, PASS74.51s (61s guest):112 2D and222 3D conformance cases with zero failed/unsupported/untested, HDR/Extended/resize/worker equality, partial-allocation and emergency cleanup, both brightness responsiveness tests, audio routing/recovery, pointer/touch lifecycle, and separate microphone gain with legacy capture compatibility. Exact selection and command are in `post-join-stage-results.json`; authoritative suite log is `.build/logs/test/x86_64-20261009T220307Z-1439801-guest.log`. These are functional checks, not a live performance acceptance. ARM/RISC current microphone/joined-profile execution and the required full verification workflow remain pending; merge inventory preparation must refresh all target suites. No milestone completion is asserted by these common build results alone.

Microphone verification refinement (2026-10-09T22:13:43Z): the actual current x86_64 SMP4 guest has now passed `kernel.services.audio_service_separates_microphone_gain_without_changing_legacy_capture` in the16-case run above. This proves the real AudioService/public-API path against the governed legacy-format-refusing sound fixture. It does not replace the still-unperformed independent Bluetooth headset gain, energy, resubscription or delayed-SCO cases. Those require the forthcoming development-agent image, not the ordinary image used for graphics timing.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0194 (2026-10-09 22:48:20 UTC):

All three allocation-fix library builds PASS (x86_64 361 s, aarch64 360 s, riscv64 358 s), with matching121-provider/121-consumer inventories. Development `RUST_MIN_STACK=33554432 LIBER_DEVELOPMENT=1 ./build.sh --arch all` PASS219.36 s; bootable volumes with `--part volume --kernel-on-volume` PASS220.91 s. Source/document hashes stayed unchanged throughout these builds. `SMP=4 RUST_MIN_STACK=33554432 TEST_SELECTION="$(paste -sd, .build/logs/end-of-job/qemu-only-20261009/post-103-join-targeted-kernel-selection.txt)" ./test.sh --arch x86_64` PASS64.4 s, all16 selected tests actually passed in51 guest seconds. Actual log: `.build/logs/test/x86_64-20261009T224643Z-1649629-guest.log`. The same kernel identity312a1cc597485888481f01efd81ea10bb876357335650629ebf83a9266f4be61 loaded the newly staged graphics libraries. Exact build commands/results remain in `.build/logs/end-of-job/qemu-only-20261009/post-allocation-*`. These functional checks do not substitute for actual animated zero-allocation, memory and30FPS acceptance.

Read-only review found that the independent radio harness had accepted a shell prompt after `stop`/`start` without proving supervisor success. Corrected `Gate.service_status`, `stop_service` and `start_service` in `src/tools/check-bluetooth-independent.py` and all three restart sites in that file and `src/harness/bluetooth_guest_classic.py`. The harness now requires exact stop/start acknowledgements, actual `lssvc json-min` stopped/ready and desired states, a retained terminated process epoch, and a new positive process epoch after restart; both transitions are recorded. This covers late SCO completion, retained microphone gain after each codec restart, and Bluetooth bond reuse after BluetoothService restart. The prepared patch passed2 valid/25 negative transcript cases, the existing independent-oracle self-test, syntax and application checks. Applied source hashes match those tested bytes; the production oracle self-test passes after application. Evidence: `.build/logs/end-of-job/qemu-only-20261009/bluetooth-supervisor-patch/`. No independent guest radio restart or microphone transport pass is claimed yet.
