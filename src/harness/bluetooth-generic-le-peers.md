# Independent ordinary LE peers

`bluetooth_generic_le_peers.py` joins the radio runner as JSON group `le-generic`.
Separate pinned RootCanal controllers and Bumble 0.0.235 implement actual SMP,
ATT/GATT, resolving addresses and encrypted connections. The scenario's battery,
custom characteristic values and HID report map are harness data; no shipping
Bluetooth packet codec or simulated guest response is used by these peers.

Peers `tag`, `display`, `remote` have static identities F0:F1:F2:F3:F4:C1/C2/C3.
The tag advertises a genuine independently generated RPA. `dual` uses its actual
RootCanal public address on both radios, reported by `ready.json`; do not hardcode
that allocated address. Both cross-transport derivation directions negotiate real
SMP keys, then establish encryption on the other transport without pairing again.

Every peer exposes encrypted Battery, custom FFF0 and HOGP services. Custom FFF1
notifies; FFF2 indicates and records completion only after Bumble receives the
ATT confirmation. Both retain observed writes. The keyboard report map exceeds
247 bytes using balanced HID global Push/Pop items, so reading it requires a long
read. `type` sends actual encrypted report notifications to the subscribed guest.

JSON commands use `action` and `peer`:

- `status`: every peer's real connections, encryption history, live HID CCCD,
  pairing counts, key fingerprints, writes and confirmed indications.
- `advertise`, `stop-advertising`, `disconnect`: control actual controller activity.
- `pairing-config`: `io` is Bumble's named I/O capability; `sc`, `mitm`, `accept`
  select the scenario. `pairing-number` supplies the number the guest displayed.
- `type` with `text`, or `report` with hexadecimal `data`: HOGP input.
- `rotate`: disconnect the tag, program a new real RPA and advertise it.
- `privacy`: independently resolve the connected guest RPA using the distributed
  IRK, returning the observed address, identity and encryption state.
- `server-values`: discover/read the guest's actual GAP/GATT attributes.
- `save-keys`, `forget-keys`, `restore-keys`: retain or restore an independent
  peer's original stored bond for downgrade adversaries; only digests are returned.
- `connect-classic` with `address`: require actual BR/EDR authentication/encryption,
  never reuse an LE link with the same public address.
- `att-hold`, `att-release`: explicitly same-team fault injection retaining one
  actual custom-characteristic descriptor request. A live bearer is answered by
  Bumble; a retired bearer is discarded and never confused with a reused handle.
  The guest timeout/revoke probes require the typed thirty-second timeout,
  encrypted replacement connection, owner teardown and absence of abandoned
  CCCD/application writes. These failures are not independent normal-peer proofs.

`bluetooth_guest_le.py` drives the shipping `btctl` and grant-only `btgatt`.
It covers NC, both SC passkey directions, legacy passkey/JW, three concurrent LE
links, downgrade refusal and old-key reuse, both CTKD directions, GATT permission
denials/read/write/notify/indicate, long-map HOGP typing, peer and host RPA changes,
service restart, cold boot and forget. The host RPA proof uses the unchanged
production fifteen-minute timer; other radio scenarios run during that interval.
Timeouts are not accepted as pairing or permission refusals. Input delivery is
proved by a complete console line produced by reports, not a peer send counter.

Two pinned-Bumble equipment adaptations are explicit:

- `Device.update_rpa` sends the previous address before updating its field.
  The harness instead generates an RPA with Bumble, programs both actual HCI
  address commands and updates the advertising set's address. The harness owns
  restart scheduling so a previously installed disconnect callback cannot restart
  advertising during address replacement.
- Legacy SMP stores the responder-distributed key as `ltk_central` on both peers
  (`smp.Session.on_pairing_complete`). `Device.get_long_term_key` incorrectly
  selects `ltk_peripheral` for the responder. The peer's key-provider adapter uses
  the actual distributed `ltk_central`, requiring matching EDIV/Rand, only when
  there is no active SMP session or SC LTK. It changes no pairing, cryptography,
  generated key or controller verdict. Without it, real RootCanal correctly
  reports authentication failure on legacy reconnect.

A bounded equipment check runs without QEMU:

```sh
.build/bluetooth-oracles/venv/bin/python src/harness/check-bluetooth-generic-le.py \
  --work NEW_ARTIFACT_DIRECTORY
```

It retains controller logs, protocol events and scenario snapshots for actual
GATT notify/indicate, long HOGP map/input, IRK/RPA reuse, all listed pairing models,
legacy reconnect and CTKD in both directions. This proves host equipment
capability only. Guest acceptance requires the final `bluetooth-independent`
gate and the retained malformed/credit/reset/unplug fixture gates.

The full gate also records BluetoothService graph/domain accounting while each
clear/encrypted BIG stream is decoded, mixed and transmitted over CIS. Independent
decoder counters must grow across each observation. The retained after-load
observation includes allocated heap high-water; IPC readings are occupancy
snapshots, not a claim to have measured the queue peak.
