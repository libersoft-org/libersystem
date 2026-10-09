# Bluetooth host oracles (P02M0194)

These dependencies run only on the development host. None is linked into a LiberSystem
binary, copied into an image, or required to use Bluetooth in a shipping system.

Prepare them explicitly once:

```sh
python3 src/harness/prepare-bluetooth-oracles.py
./check.sh --gate bluetooth-codecs
```

`--only codecs` or `--only radio` prepares either group. The codec gate rebuilds the
production Rust leaves and adapters from verified cached sources, without network
access. `check-bluetooth-codecs.py --quick --no-build` is a development shortcut and
does not establish the full gate. Receipts and compiler output are under
`.build/bluetooth-oracles`; `result.json` includes the source pins, production source
digests, configuration counts and measured error. A dependency missing from the
cache is an error, never a skipped passing test.

## Sources and terms

| Dependency | Source and pin | Terms | Role |
| --- | --- | --- | --- |
| RootCanal | [google/rootcanal](https://github.com/google/rootcanal/tree/574dc62546e0adcfacb6d6df4776f9d4f24d32d8), PyPI 1.13.0 | Apache-2.0 | Independent BR/EDR and LE controller; H4 TCP |
| Bumble | [google/bumble](https://github.com/google/bumble/tree/745d607029d5c2607659894daf4139736aa6e2ff), PyPI 0.0.235 | Apache-2.0 | Independent peer protocol stacks |
| AOSP SBC | [Bluetooth/system/embdrv/sbc](https://android.googlesource.com/platform/packages/modules/Bluetooth/+/745ee92b87ed62b5a8a1bac47d5df8cad623bf59/system/embdrv/sbc/) | Apache-2.0, file copyright notices preserved | SBC/mSBC encoder and decoder |
| Google liblc3 | [google/liblc3](https://github.com/google/liblc3/tree/efe84d3e0ece9e54591098148da3d10f5503b84e) | Apache-2.0 | LC3 encoder and decoder |
| Google libsbc | [google/libsbc](https://github.com/google/libsbc/tree/6e505650145c9973d08a0bdd5e5f5e1914305e40) | Apache-2.0 | Additional independent SBC decoder where the AOSP decoder is defective |

`bluetooth-oracles-sources.json` pins archive SHA-256 values and revisions; the
preparer checks them before extracting and compiling unmodified source files.
`bluetooth-oracles-requirements.txt` pins the radio packages and their Python
dependencies in a private virtual environment. Upstream notices remain in the
downloaded trees. No Bluetooth SIG member-only conformance material is downloaded
or used, and this gate makes no SIG qualification claim.

## Codec comparisons and reference limitations

The gate compiles `service_logic::{sbc,lc3}` through a host adapter. Each encoder's
frames are independently decoded by both implementations, preserving state across
frames. The source includes silence, impulses, different tones per channel,
alternating samples and deterministic noise. It covers all SBC frequencies, block
counts, channel modes, allocation methods and subband counts at pools 2, 26 and 53,
plus mSBC. LC3 covers six rates, both frame durations, every 20–400-byte frame size,
and repeated rate changes across LTPF activation thresholds.

LC3 comparisons enforce normalized maximum difference 0.00148 and RMS at most
-89.06 dB, the 16-bit decoder precision thresholds published in liblc3's
[speech decoder conformance report](https://github.com/google/liblc3/blob/efe84d3e0ece9e54591098148da3d10f5503b84e/conformance/speech_decode_10m.html).
Synthetic differential vectors within those thresholds are evidence for this
gate, not the member-only conformance corpus. SBC's separately quantized fixed
point implementations must agree within 16 peak PCM samples and 4 samples RMS.

The AOSP decoder has two independently reproduced limitations:

- Four-subband joint stereo calls `OI_BITSTREAM_ReadUINT4Aligned` immediately after
  `OI_BITSTREAM_ReadInit`, which sets `bitPtr` to 32. The aligned reader requires
  `bitPtr < 16`; it reads the last scale factor and following samples incorrectly.
  Google libsbc agrees with Liber on the exact AOSP-generated stream. The gate
  uses Google for this decoder configuration while still testing AOSP's encoder.
- Full-scale low-bitpool overload makes AOSP's four-band synthesis accumulator
  overflow signed 32-bit arithmetic (`synthesis-sbc.c:439`, confirmed with
  `-fsanitize=signed-integer-overflow`). Precision vectors use a 12 dB input margin;
  production saturation is unchanged. Overload numerical equivalence to that
  overflowing reference is not claimed.

Google libsbc limits its input frame size to the source PCM size, so AOSP covers
otherwise legal larger frames. Every case must have an independent decoder; a
reference refusal never silently passes or removes a case. The adapters only
translate ABI and framing. AOSP's mSBC decoder consumes the HFP transport padding
octet following the 57-byte codec frame; its adapter supplies that octet explicitly.

## USB controller bridge

```sh
python3 src/harness/usbredir_device.py --emulate bt-bridge \
  --hci 127.0.0.1:6402 --socket /path/to/private/device.sock
```

Attach that socket as `USB_REDIR_SOCKET` when starting QEMU. `BtBridge` reuses
`bt-sco`'s real USB interfaces: commands on control, events on interrupt, ACL and
ISO on bulk, SCO on the voice isochronous alternate. RootCanal generates every HCI
response and peer packet. Successful CIS events determine outgoing ISO handles;
the bridge implements no radio pairing, security, profile or codec behavior.
TCP loss and bounded queue exhaustion fail the bridge, and cleanup prints H4
packet counts for evidence. The original in-guest fixtures remain distinct
same-team fault-injection oracles.

## Independent radio gate

After the final shared development build/image, run `./check.sh --gate bluetooth-independent`.
It starts private pinned RootCanal/Bumble controllers, a production USB bridge, and two
QEMU boots of one persistent system volume. Host JSON control and observations, serial
logs, original codec frames, independently decoded PCM, dnsmasq leases and H4 counters
are retained under `.build/logs/bluetooth-independent/`. The bridge keeps its HCI socket
with `--connections 2`, preserving the controller address across cold boots.

`bluetooth_classic_peers.py` supplies HID keyboard/gamepad, SPP echo, A2DP sink/source,
AVRCP and HFP peers. Pairing controls expose real Numeric Comparison, Passkey, Just
Works and explicit legacy/SC configuration through Bumble/HCI. Its audio verdicts use
AOSP, never the shipping Rust encoder/decoder. `bluetooth_guest_classic.py` drives the
shipping tools and existing-grant audioprobe scenarios. LE peer API and exact host
capability proof are in [bluetooth-le-peers.md](bluetooth-le-peers.md).
Ordinary LE/GATT/HOGP, privacy, all LE pairing models and CTKD use the additional
independent peers documented in [bluetooth-generic-le-peers.md](bluetooth-generic-le-peers.md),
including the exact pinned-Bumble equipment adaptations and host capability check.

OPP/BNEP remain explicitly same-team protocol glue in `bluetooth_transfer_peers.py`,
as required by the plan where Bumble lacks those profiles. `bluetooth-pan-network.py`
requires Linux network namespaces, `/dev/net/tun`, `ip`, and `dnsmasq`; it changes only
its private namespace. DHCP comes from dnsmasq, and ICMP/TCP from the Linux stack. A
missing dependency or privilege is a failure, never a passed/skipped radio scenario.

Short host capability checks (they do not prove guest interoperability):

```sh
.build/bluetooth-oracles/venv/bin/python src/harness/check-bluetooth-bridge.py \
  --work .build/bluetooth-oracles/bridge-reconnect
.build/bluetooth-oracles/venv/bin/python src/harness/check-bluetooth-le-peers.py \
  --work .build/bluetooth-oracles/le-peers-selftest
python3 src/tools/check-bluetooth-independent.py --self-test
```

RootCanal 1.13.0 does not authenticate a BIG synchronization broadcast key: the pinned
implementation ignores that input, and the host check demonstrates a wrong code still
receiving both BIS streams. RF wrong-key rejection is an explicit emulator limitation;
successful audio is not evidence of key authentication. BASE/BIG/BIS transport, codecs
and guest command/error handling remain testable. Existing in-guest fault fixtures
retain malformed-fragment, exhausted-credit, reset and unplug cases separately.

The Object Push transport equipment can be checked with
`.build/bluetooth-oracles/venv/bin/python src/harness/check-bluetooth-opp.py --work NEW_ARTIFACT_DIRECTORY`.
It uses real RootCanal controllers and Bumble SDP/RFCOMM/L2CAP ERTM, checking one 4915-byte object over each transport. The OBEX peer is explicitly same-team glue. This is a host equipment check, not a Liber guest pass; the guest gate separately requires its actual advertised-transport choice and exact object receipt.

The independent classic voice scenarios also exercise the approved separate microphone API:
`audioctl microphone DEVICE [LEVEL]`. Pinned Bumble observes `+VGM` separately from `+VGS`;
remote `AT+VGM` must change the microphone query while speaker inventory remains unchanged,
and operator updates must reach the peer and report the actual quantized gain. This runs for
mSBC HFP, CVSD HFP and HSP. The bounded `bluetooth-radio-peers.py --self-test` checks the
Bumble event adapter with parsed AT bytes only; it never substitutes for these guest assertions.

The classic gate also schedules one real SCO acceptance after its application and AudioService
owner have closed. `sco-hold` retains one headset request; `sco-release` lets Bumble construct the
normal HCI accept and RootCanal deliver the actual connection completion. The guest must explicitly
disconnect the orphan link with reason0x13, then restore the still-connected headset/microphone
snapshot and complete the following normal duplex call. This hold is same-team scheduling control,
not independent protocol evidence by itself. The bounded peer self-test checks no early accept,
real Bumble command construction and refusal after the ACL connection is replaced or withdrawn.
Only a completed guest gate proves the production late-completion behavior.
