# Independent LE peer controls

`bluetooth_le_peers.py` is loaded by `bluetooth-radio-peers.py` under the JSON control
group `le`. All ATT, SMP, ASCS, CSIP, VCS and ISO traffic uses pinned Bumble 0.0.235
and separate RootCanal 1.13.0 HCI controllers. Audio is independently encoded and
decoded by the pinned upstream liblc3 library. Control replies and event logs are
host observations; none are production Bluetooth replies or simulated successes.

The three peers are `le-left` (static random `F0:F1:F2:F3:F4:A1`), `le-right`
(`F0:F1:F2:F3:F4:A2`), and `le-broadcast` (`F0:F1:F2:F3:F4:B0`). Use `ready.json`
or `status` for addresses, since these are distinct from controller public addresses.
Each earbud exposes PACS, ASCS sink ASE 1, CSIS/CAS, VCS and TMAP. CSIS uses a shared
encrypted SIRK, set size 2, ranks 1/2 and independently generated RSI advertising.
Sink PACs support mono LC3 at 16/48 kHz, 7.5/10 ms, 20–120 octets. Left also exposes
source ASE 2 (16 kHz, 20–40 octets). Left/right Audio Locations are 1/2. Pairing is
Secure Connections Just Works with persisted bonds in the artifact directory.
Their distributed SMP identities explicitly use these same static random
addresses; Bumble's default public identity would be a different controller
address and could not name this static-address advertisement on reconnect.

Commands are JSON lines sent to the runner's private Unix control socket:

```json
{"group":"le","action":"start-advertisements"}
{"group":"le","action":"status"}
{"group":"le","action":"set-volume","peer":"le-left","level":129}
{"group":"le","action":"source-voice","peer":"le-left","frequency":1000,"enabled":true}
{"group":"le","action":"source-voice","peer":"le-left","enabled":false}
{"group":"le","action":"gtbs-watch","peer":"le-left"}
{"group":"le","action":"gtbs-control","peer":"le-left","control":"accept","index":1}
{"group":"le","action":"gtbs-control","peer":"le-left","control":"terminate","index":1}
{"group":"le","action":"disconnect","peer":"le-left"}
{"group":"le","action":"depart","peer":"le-left"}
{"group":"le","action":"broadcast-start","broadcast_id":1193046}
{"group":"le","action":"broadcast-stop"}
```

`disconnect` allows the peer to advertise and reconnect normally. `depart`
disables advertising before disconnecting; restart scheduling belongs to this
harness because Bumble installs its automatic callback when the connection is
created. This prevents a deliberate departure from racing an immediate return.
The guest checks set survival/removal while the absent peers remain trusted,
then removes their audio trust before the separate own-RPA timer scenario. This
releases the pending filter-list initiation; the controller cannot change its
initiating address while that attempt is in progress.

`gtbs-watch` requires an established connection to the production call gateway. It
discovers the remote GTBS, subscribes to Call State and Call Control Point, and reads
the current call list. It never creates calls in the guest. The gateway must first
create the desired incoming/outgoing call through its normal service interface.
`le-gtbs-calls` events contain `{index,state,flags}` triples; `le-gtbs-result` contains
the actual remote control notification as hex. Source voice waits for a negotiated,
streaming ASCS source; it does not create a CIS or bypass negotiation.

`status` returns `peers`, `captures`, `errors`, and the current `broadcast` or null.
Each capture includes peer, ASE, sample rate, duration, octets, Audio Location,
received frame/loss/sequence-gap counts, measured PCM RMS/frequency, and `.lc3`/`.pcm`
paths. LC3 artifacts have a little-endian 16-bit length before each frame; PCM is
native little-endian signed 16-bit mono. Artifacts stop growing at 30 seconds per
stream; counters continue. Captures exist only after a real streaming ASE and count
only actual received ISO SDUs. Guest stereo output can therefore be checked against
separate left/right tone frequencies. `le-ase-state`, `le-codec`, `le-iso-capture`,
`le-paired`, `le-encrypted` and `le-volume` provide additional observations. Any
`errors` entry fails an interoperability gate.

Broadcast sends two 48 kHz, 10 ms, 120-octet BIS streams with independently encoded
1500 Hz left and 3000 Hz right tones. The periodic BASE contains the real codec and
channel configuration; extended advertising contains broadcast ID and name.
`le-broadcast-started` records actual assigned BIS handles and BASE. An optional
`"code":"00112233445566778899aabbccddeeff"` supplies a 16-byte broadcast code.

**Emulator limitation:** RootCanal 1.13.0 accepts an incorrect broadcast code and
delivers the BIS packets. Its `hci_le_big_create_sync` ignores the encryption/key
arguments. The host capability check deliberately demonstrates this with two
different keys. Neither a successful BIG sync nor received PCM proves broadcast-key
authentication in this emulator. Wrong-key RF rejection is outside this fixture's
capability; ordinary BIG/BIS reception and host error handling remain testable.

Run the short host capability check separately from the guest gate:

```sh
.build/bluetooth-oracles/venv/bin/python src/harness/check-bluetooth-le-peers.py \
  --work .build/bluetooth-oracles/le-peers-selftest
```

It verifies SMP encryption, encrypted CSIS SIRK, PACS/ASCS, VCS in both directions,
the GTBS client, real bidirectional CIS audio (2 kHz/1 kHz), actual periodic BASE and
BIG synchronization, and both received BIS tones. It retains `summary.json`, event
and RootCanal logs, and independent decoded audio. A passed host check establishes
test-equipment capability only; production guest interoperability requires the
separate guest run and its own evidence.
