IMPLEMENTER'S IMPLEMENTATION ON P02M0194 (2026-10-04 to 2026-10-06):

Scope: `docs/todo/P02M0194.md`, parts a to i - the classic foundations and pairing (a), LE beyond one mouse (b),
input (c), AudioService's device, voice and call model (d), A2DP and AVRCP (e), HFP and HSP over SCO (f), serial,
file push and tethering (g), LE Audio (h), and the operator tool, verification and completion (i). What stays open is
the owner's: the independent radio and peers (RootCanal and Bumble) and the codec oracles (AOSP SBC, liblc3), and the
end-of-job runs - the three cross-builds, the dynamic report and the Domain limits re-measured under load.

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
table print), `cargo test --manifest-path user/drivers/core/Cargo.toml bt_` for the fixture. Guest gates on x86_64:
`bluetooth-classic`, `bluetooth-le`, `bluetooth-input`, `bluetooth-audio`, `bluetooth-transfer`, `bluetooth-service`,
`audio-routing` and `bluetooth-le-audio`.
