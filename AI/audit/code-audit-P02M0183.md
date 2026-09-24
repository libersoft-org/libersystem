IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0183 (2026-09-23T05:58:00Z):

This record was opened after the implementation had started earlier in the same session (the IDL, the
pure leaves and the MBIM validators were written first); everything below is what is in the tree now.

## What was implemented

### P02M0183a - contracts, deployment, authority

- `src/idl/modem.lsidl` (`liber:modem@1`): `modem-id` (publication slot/generation/binding generation
  plus the service incarnation), `context-id` (modem, SIM generation, context generation), SIM,
  registration, signal-with-validity, `attempt-count` (known flag, remaining, SIM generation, observed
  time), `context-status` (state, address, prefix, MTU, and the two drop counters `transmit-refused` /
  `receive-dropped`), `modem-status` (no subscriber identifier), `modem-limits`, `modem-watch`
  (snapshot/resync). Interfaces `modem` (observation), `modem-data`, `modem-identity`, `modem-manage`
  (`sim-pin-outcome` with `accepted/rejected/blocked/outcome-unknown/unknown-count`) and `modem-admin.mint(kind,
  alias, data-policy, owner: handle<task>) -> modem-grant`.
- `src/idl/modem-device.lsidl` (`liber:modem-device@1`), the provider contract: negotiated
  `device-limits`, `device-open` (connection generation), `device-state`, `ip-config` (with `ip-family`, so an
  IPv6-only context is expressible), `command`/`command-reply` (connection generation, transaction, kind,
  SIM and context generations, secrets bounded to 8 bytes, APN), `indication` (revision + whole state),
  `datagram` (context generation + at most 4096 bytes); interface `modem-device` (open, command,
  indications stream, receive stream, transmit channel). Development-only `modem-fixture` control
  interface (set-sim, delay, withdraw, republish, stats, replay, drop-context, ipv6-only).
- `gen.sh` packages and dependencies, crates `modem-proto` and `modem-device-proto`, the `proto` facade,
  manifest `[[sources]]`/`[[libraries]]` rows. `./gen.sh --accept-breaking` regenerated 23 packages.
- Provider kind `modem = 15`: device IDL, `driver_protocol::provider::MODEM`, system-manifest's closed
  mapping, DeviceManager's two conversions.
- ModemService, `src/user/services/core/src/modem_service.rs`, manifest program and service rows with
  roles `CATALOGUE` (factory, `kinds = ["modem"]`), `SERVE` (observation root), `ADMIN` (minting root) and
  `LINK` (factory minted by ServiceManager from NetworkService's new `LINKADMIN` root), depending on
  `network_service`; `restart = "transparent"`, relaunched by re-running the plan (`plan_relaunchable`).
  rt `CAP_MODEM_STATE`/`CAP_MODEM_ADMIN`, broker `cap_grants`, `service_of_cap` and `serve_resolve` arms
  so PermissionManager resolves both roots by name.
- Limits: 4 providers, 32 client connections (observation connections and grants together), 1 context,
  4 admin connections; `modem.limits` states them with what is in use.
- PermissionManager (`permission_manager.rs`): capabilities `modem-state`, `modem-data`,
  `modem-identity`, `modem-manage` appended to `VOCABULARY` (now 35); `modem-state` is a fresh
  sub-connection of the observation root; the other three are minted in `grant_for_task` through
  `modem-admin.mint` with a `WAIT | TRANSFER` duplicate of the prepared task, the minted modem and SIM
  generation written into the audit entry's detail. `modem_policy` has no shipping row: identity,
  management and data are denied by default. Development rows for the gate's probes.

### P02M0183b - provider boundary and transaction lifetime

- `src/user/drivers/core/src/mbim.rs` (host-tested, 8 tests): fragment parsing, negotiated limits
  (256 fragments, 64 kB/message, 256 kB aggregate, 4 assemblies), an `Assembler` that refuses order and
  conflicting duplicates and expires assemblies after two seconds, fragment construction, NTB16/NDP16
  datagram extraction with session membership, every offset checked and NDP cycles refused, and block
  construction.
- `service_logic::modem_commands` (host-tested, 12 tests): per-connection transaction numbers, eight
  pending and one state change at a time, replies dropped unless they name the outstanding
  connection/transaction/kind, stale SIM or context generations answered `stale` without acting,
  five/sixty-second deadlines, expiry of a state change as OUTCOME-UNKNOWN with the modem uncertain until
  a read-only query answers (`Refusal::Reconcile`), retry counters known/unknown with SIM generation and
  time, unknown counts needing acknowledgement, SIM replacement ending the context and counters, context
  loss from indications, a `Watch` that coalesces to the newest snapshot and marks RESYNC (`requeue` keeps
  an undelivered snapshot's kind), and `PacketQueue` (64 packets / 256 kB, MTU-capped, drops counted).
  Added in this pass: `Reply::context_active`, so the reconciling query settles an UNKNOWN context to
  what the device reports, and an expired activation records the context generation that may exist.
- ModemService uses them: commands are sent with the capture trick and never waited for; replies,
  indications and datagrams are handled as they arrive; PIN and PUK are preceded by a fresh query and sent
  once, their secrets in a zeroing `Secret` and the command, its encoding and the request buffer scrubbed
  after use; an OUTCOME-UNKNOWN PIN or state change schedules a reconciling query, never a replay; a
  context the modem reports up that no flow owns is deactivated (bounded).

### P02M0183c - NetworkService attachment and lifecycle

- `src/idl/network.lsidl`: `link-provider`, `link-family`, `link-attachment` (provider, SIM and context
  generations, packet channel, family, address/prefix, optional gateway, at most two DNS servers, MTU),
  `link-installed` (interface identity and effective MTU) and the private `network-link-admin`
  (reserve / install / release).
- `src/idl/base.lsidl`: the shared `error` enum gained `link-changed` (value 15) - the typed error the
  plan names for sockets and listeners of a switched link. `world_errors.rs` and `lico.rs` gained arms.
- `net.rs`: the raw-IP medium is a flag in the existing `Stack` (`Stack::new_raw`): no link-layer header
  (`l2()`/`write_l2`), `on_frame` validates the message with `service_logic::raw_ip::datagram` and hands
  it to the same `on_ipv4`, `lookup` never waits for a MAC, no ARP is ever built; ICMP/UDP/TCP/DNS are the
  shared code. Also: the interface identity (index, generation) the stack was built under, a second DNS
  server, and a channel-closed mark that stops every wait at once.
- `network_service.rs`: one serve loop for linked and unlinked service (`Runtime`, `serve`). The network
  subscription is held for the life of the service; the boot snapshot is taken whole and fed in
  publication-identity order, so the lowest NIC is selected; `service_logic::uplink` decides selection,
  late providers, withdrawal, failed channels (`failed`), reservations, installation and release. A NIC
  is opened through the catalogue with a bounded greeting (2 s); one that cannot be opened is failed and
  the choice made again. A switch tears the link down (`tear_down`): sockets and listeners are marked
  dead and answer `link-changed`, receive streams end, pending `accept`s are answered `link-changed`, and
  the stack (addresses, routes, DNS, traffic) is dropped; the interface generation advances. The
  `LINKADMIN` root (appended last to the role list; the kernel DHCP harness sends the tag with no
  capability) mints link-admin connections; a reservation belongs to the connection that made it, and the
  holder's channel closing releases it. `install` validates the attachment (`uplink::validate`, the
  family profile, a packet channel) before anything is torn down and answers the interface identity and
  effective MTU; `release`'s fallback is applied after the reply. `info` reports `wwan0`, index 1, no MAC,
  the attachment's DNS servers and a direct default route when there is no gateway.
- ModemService's flow: `activate` reserves admission (`again` = busy, nothing else touched), then asks
  the modem, then installs with a fresh packet channel and answers only when NetworkService committed;
  an installation refusal (IPv6-only is `unsupported`) is a bounded deactivation plus the reservation
  given back. Context loss, SIM replacement, provider loss, the owner task ending, the grant closing or
  NetworkService dropping the packet channel end the context and release the link. Link-admin calls are
  asynchronous with a ten-second deadline; a late reservation nobody waits for is released.

### P02M0183d - verification

- Fixture `src/user/drivers/core/src/modem_fixture.rs` (development-only, `edu` at 0:28.0, `dma = "none"`):
  a SIM with PIN 1234 / PUK 12345678 and counters, one context configured 10.64.0.2/30, gateway and DNS
  10.64.0.1, MTU 1400 (or IPv6-only on request); every command and reply is fragmented and reassembled by
  `mbim::Assembler` and every datagram packed and extracted by `mbim::block`/`mbim::datagrams`; the gateway
  answers ICMP echo, the resolver answers `modem.test` and NXDOMAIN for anything else; control operations
  as listed above; the stats count attempts and only the LENGTH of the last secret.
- Probes (development-only, services crate): `modemcheck`, `modemhold`, `modemswap`, `modemdata`,
  `modemfail` - see the gate script for what each proves.
- Gate `src/tools/check-modem-service.sh`, registered as `qemu-modem-service` in `check.sh`, verify-model
  `GATES` (now 132) and `GATES_THAT_BOOT_A_GUEST` (now 38), and `release-required.toml`
  (`gate.qemu-modem-service`, `host.modem-proto`, `host.modem-device-proto`). Two machines: `NET_NONE=1`
  (new harness switch in `qemu_attach_virtio_net`) for the no-NIC half, the ordinary machine for busy
  refusal and authorized replacement/fallback.

## Material decisions

1. `link-changed` was added to the shared base error enum rather than reusing `stale` or
   `address-unavailable`; the plan names a typed link-changed error, and the two earlier additions
   (`address-unavailable`, `stale`) set the precedent. Pre-release, so `--accept-breaking`.
2. A granted endpoint carries no `duplicate` right (`GRANT_RIGHTS`), so the "duplicated or transferred"
   proof moves the endpoint to another process (`modemhold | modemcheck inherit`) - the only way a grant
   can outlive its owner's process in another holder.
3. Prepared-launch failure after minting is exercised by `modemfail`, whose identity grant names an alias
   nobody configured: PermissionManager mints its data grant, fails the identity one and abandons the
   prepared task, and ModemService retires the data grant when the task observer fires. The gate proves
   reclamation by the handle/client baseline and the service's retirement line.
4. The observation root counts against the 32 clients; admin connections are bounded separately (4).
5. While choosing the modem fixture's PCI slot, 0:31.0 turned out to be ICH9-LPC's on q35: QEMU refuses
   `-device edu,addr=0x1f`. P02M0182's smart-card fixture was pinned there, so its registry entry and
   gate were moved to 0:27.0 (`addr=0x1b`); probed with QEMU directly (0x18-0x1e accepted, 0x1f refused).
   The correction is recorded in P02M0182's record as well.
6. A development-image producer is still not registered for this gate, for the reason recorded for the
   earlier fixture gates: `LIBER_DEVELOPMENT=1 ./build.sh` rewrites the same staging the shipping image
   producers read, so declaring it a producer creates an ordering hazard. Owner decision.

## Verification performed (2026-09-23)

- `cargo check` of the services crate (all bins, with and without `development`), the drivers crate (all
  bins, `development`) and the tools crate (`shared-image`): clean.
- `TEST=1 TEST_TAGS="" cargo build --tests` in `src/kernel`: builds (the harness change and the new error
  variant).
- Host suites from `src/`: service-logic 594 passed (modem_commands 12, uplink 10, raw_ip 3 among them);
  drivers lib 270 passed (mbim 8); modem-proto 18, modem-device-proto 15, network-proto 57, base-proto 4,
  proto 53; system-manifest 24; verify-model 157 (the release-required exact-set test included).
- `./gen.sh --accept-breaking`: OK. `./check.sh --gate source-hygiene`: clean.
- `python3 src/tools/check-bootstrap-plan.py`: the modem service is consistent; the check still fails on
  `font_catalogue` ("the manifest says transparent but relaunch_service cannot re-run its bootstrap"). That
  failure is PRE-EXISTING: the same checker run against a `git archive HEAD` copy reports the same line.
- QEMU (no boot): which `edu` slots q35 accepts, as above.

## Not performed yet

- The `qemu-modem-service` gate (both machines): it needs a development image, built at the end of the job.
- The existing network guest coverage the NetworkService refactor touches: the DHCP lease kernel test, the
  `test.sh` network tests, `ipv6-peer`, `dma-mode-x86_64` (its "configured via DHCP" / "no network
  provider" lines) and `qemu-virtio-iommu-x86_64`; the aarch64 and riscv64 cross-builds; the dynamic-report
  regeneration check. All at the end of the job.
- Until the gate has run, the guest-behaviour items of the plan stay unticked.

## Verification pass (2026-09-24T00:34:07Z): the gates booted, what they found, and what changed

### Common to the service gates of P02M0180-P02M0188 (recorded in each of the nine files)

Each item below was found by booting a gate, fixed, and booted again. None of the gates had run before
this pass.

- **The first development image did not build.** Dynamic programs import every non-inlined function
  they call from a staged library and must declare each one; services calling the decisions crate now
  declare `service-util`, and a service calling a crate no staged library publishes is STATIC, as
  `font_catalogue` is (`power_service`, `smartcard_service`, `midi_service`, `admin_service`). The generated
  packages that use `liber:process@1.{task}` only as a handle no longer declare `process-proto`.
  `bootproto::manifest::MAX_ROWS` went from 256 to 512: the development volume's signed manifest carried
  291 rows (the row count is a `u16` on the wire; 512 rows of the longest staged path fit the 64 kB bound
  - a boot-protocol bound, flagged for the owner). The capability names the broker resolves for the new
  services live in `services::capability_names`, so `rt` stays byte-for-byte its committed state.
- **No fixture ever bound.** QEMU's `edu` function is class 0x00, subclass 0xff (`lspci`: `class 00ff`);
  all seven fixture rows in `manifest.toml` asked for 0xff/0x00. Corrected in all seven.
- **PermissionManager could launch none of the new probes.** Their policy rows were
  `#[cfg(feature = "development")]`, and PermissionManager is a dynamic program built once, into the shared
  image both configurations stage, without that feature - so the rows were absent from the development
  image too, and the shell reports a refused launch by printing nothing. The rows, the fixture-control
  grant path and the per-probe policy tables are unconditional, as the older probe rows (`btcheck`,
  `sandbox_probe`) already were; what keeps them inert in the shipping configuration is that none of their
  programs is staged there and that its supervisor mints no catalogue connection admitting a fixture's
  kind. PermissionManager declares the `smartcard-proto`, `modem-proto`, `camera-proto` and `midi-proto`
  it now calls.
- **No grant could be minted from a resolved root.** `rt::connect_or_resolve` keeps the connection the
  broker minted from a service's root and mints its own from it; SmartcardService, ModemService,
  CameraService, MidiService and AdminService accepted CONNECT only on their roots, so every mint was
  refused. A connection now mints another of its kind under the same bound, as PowerService's and
  BluetoothService's already did.
- **Handle baselines are read from the system graph** (`graph`), which holds each service's process and
  reads the kernel's own count: ProcessService's accounting lists only launches in a Domain of their own,
  and these services run in the supervisor's. SystemGraphService's 4 kB reply buffer had been outgrown by
  the graph (every `graph` answered `query error`); it is 64 kB, on the heap. Two measured properties of
  the graph decide WHEN a gate reads it: PermissionManager resolves each service root the first time it
  mints from it and keeps that connection (+1 handle per root, once), so the baseline is read after the
  first probes; and the graph keeps the process handle it was given at its own start, so after a service
  restart it reads the instance that ended (state `failed`, no handles) - no baseline is read across a
  restart.
- **A pipeline stage's diagnostics never reached the terminal** (older than this job; the gates exposed
  it). The shell handed the pipeline broker its terminal without DUPLICATE, so each per-stage stderr
  `duplicate` failed silently and `eprint` fell back to stdout - the pipe, for every stage but the last.
  The shell now hands the broker a duplicable endpoint, the broker gives the last stage a narrowed copy
  (send, wait, transfer) and closes the duplicable one once every stage's error endpoint exists. The
  holders that are first stages (`cardhold`, `modemhold`, `camhold`, `midihold`, `admincheck`) report on
  stderr.
- **Three fixtures spun a processor.** The camera, smart-card and modem fixtures armed their timer only
  when something was due; a kernel timer stays expired until armed again, so after the last deadline
  every wait returned at once and the fixture starved the probes. The timer is armed on every pass, to
  `u64::MAX` when nothing is due.
- **Gate scripts**: three were not executable (modem, camera, MIDI) and four copied their log onto itself
  (`cp "$GUEST_LINES" "$lines"` naming one file) and exited under `set -e`; fixed.
- **Two runs in parallel collide.** Every x86_64 guest forwards host port 5555; a second guest with a NIC
  is refused before it boots. Gates are run one at a time.

### Found by the modem gate's boots, and changed

- **Every grant was refused** (the common CONNECT defect): ModemService accepted CONNECT only on its roots.
  An admin connection now mints another admin connection (`CONNECT_OP && !is_serve`, bounded by the admin
  table), and an observer connection mints another observer (`Observer { chan, watch: None }`, bounded by
  `MAX_CLIENTS`).
- **The modem fixture's alias table was `cfg`-gated** in the service like the policy rows, and absent from
  the image for the same reason; it is unconditional (the fixture row is what exists only in development).
- **The fixture spun** (the common timer defect).
- **The handle baseline**: the probe's `handles` phase is gone; the gate reads the service's handles from
  the system graph after the first probe and at the end (13 both times - measured: the two roots
  PermissionManager resolves on first use are the difference between the boot figure, 11, and every
  figure after), and `modemcheck clients` prints the service's own client count at the same two points.
- **`modemhold` reported into the pipe**; it reports on stderr.
- A first run's second boot (the machine with a NIC) never started: another guest of mine was holding host
  port 5555 at the time. Not a defect of the milestone; the rerun below boots both machines.

### Verification

| What | Command | Result |
| --- | --- | --- |
| development image | `LIBER_DEVELOPMENT=1 ./build.sh --arch x86_64` then `LIBER_DEVELOPMENT=1 ./image.sh` | PASS, 2026-09-23T22:24Z |
| the gate | `./check.sh --gate qemu-modem-service` | **PASS**, 800 s, 2026-09-23T22:46Z |

First machine (no NIC): `modemdata: PASS`; `modemcheck: PASS` pin, identity, activate, drop, rollback,
inherit, sim, uncertain, limits, republish; "network: modem link installed - 10.64.0.2/30"; "ModemService: a
grant's owner ended - its grant is retired"; "the service's handles (13) and clients (5) returned to their
baseline". Second machine (a NIC): "network: configured via DHCP", `modemcheck: PASS busy`, `modemswap:
PASS`.

### Host-side checks run in this pass (2026-09-23/24, x86_64 host)

| What | Command (from `src/` unless it says `./`) | Result |
| --- | --- | --- |
| decisions crate | `cargo test --manifest-path user/services/logic/Cargo.toml` | PASS, 670 |
| drivers library (fixtures' pure halves, decoders) | `cargo test --manifest-path user/drivers/core/Cargo.toml --lib` | PASS, 289 |
| manifest model | `cargo test --manifest-path tools/system-manifest/Cargo.toml` | PASS, 24 |
| terminal | `cargo test --manifest-path term/Cargo.toml` | PASS, 68 |
| bind ledger | `cargo test --manifest-path user/libs/driver/binding/Cargo.toml` | PASS, 85 |
| changed programs | `cargo check --target x86_64-unknown-none [--features development] --bin <name>` for `btcheck`, `cardcheck`, `permission_manager`, `admin_service`, `import_probe` | PASS |
| verification model | `./check.sh --gate verify-model` | PASS |
| declared interfaces | `./check.sh --gate declared-interfaces` | PASS |
| grant vocabulary | `./check.sh --gate grant-vocabulary` | PASS |
| milestone index | `./check.sh --gate milestone-index` | PASS |
| source hygiene | `./check.sh --gate source-hygiene` | PASS |
| bootstrap plan | `./check.sh --gate bootstrap-plan` | FAIL, the pre-existing `font_catalogue` mismatch only |
| dynamic report | `./check.sh --gate dynamic-report` | FAIL: "missing aarch64-unknown-none provider admin-proto" - the tracked report covers all three targets and cannot be checked or refreshed without their builds |

### Not performed in this pass, and why

- **The AArch64 and RISC-V cross-builds** that every registration item names. The owner's standing rule is
  that nothing slow runs while a task of the job is open, and that big runs are theirs to start. Commands:
  `./build.sh --arch aarch64`, `./build.sh --arch riscv64` (and with `LIBER_DEVELOPMENT=1` for the development
  programs), then `./check.sh --refresh dynamic-report` and `./check.sh --gate dynamic-report`.
- **`./verify.sh --plan` and `./verify.sh`**, for the same reason.
- No gate was run on AArch64 or RISC-V; every gate here is x86_64-only by its own statement.

### Where it stands, and what is the owner's

Ticked in `docs/todo/P02M0183.md` (2026-09-24): twenty-two items. Open: the limits item - refusal at the
four-provider, 32-client and one-context caps is tested nowhere (the gate reads the stated limits; the host
tests bound pending commands and packets), so a test at each cap is still owed; and the run item - the
cross-builds, the dynamic report that needs them, and the "affected existing network guest checks"
(`./test.sh --arch x86_64 --tags network`, `./check.sh --gate ipv6-peer`) have not run. What this pass saw of
the Ethernet path: the modem gate's second machine was configured by DHCP and got its NIC back after the
authorized replacement, and every guest in the pass printed its IPv6 link-local status.

## Final state of this pass (2026-09-24T01:32:00Z)

All nine service gates pass on x86_64, each run by itself (every x86_64 guest forwards host port 5555, so two
at once collide):

| Gate | Result | Finished (UTC) | Tree it ran on |
| --- | --- | --- | --- |
| `bluetooth-service` | PASS, 841 s | 2026-09-24T00:10:18Z | development image built 2026-09-23T23:17Z |
| `power-service` | PASS, 360 s | 2026-09-24T00:16:18Z | the same image |
| `smartcard-service` | PASS, 481 s | 2026-09-23T22:32:56Z | development image built 2026-09-23T22:22Z |
| `qemu-modem-service` | PASS, 800 s | 2026-09-23T22:46:16Z | the same 22:22Z image |
| `qemu-camera-service` | PASS, 400 s | 2026-09-23T23:49:37Z | the 23:17Z image |
| `qemu-midi-service` | PASS, 400 s | 2026-09-23T23:56:17Z | the 23:17Z image |
| `spool-service` | PASS, 40 s | 2026-09-24T01:00:05Z | the test kernel over the tree as it then stood |
| `media-import-service` | PASS, 40 s | 2026-09-24T01:00:45Z | the same |
| `qemu-admin-path` | PASS, 406 s | 2026-09-24T01:27:30Z | its own cold development build of the final tree |

What changed after a gate's pass, and why it does not reach that gate's path: after the smart-card and modem
passes, the bind ledger's bound (keyboard drivers only), the Bluetooth probe's pointer check, and the
AdminService, admin probe, import probe, kernel scenario and harness changes; after the Bluetooth, power, camera
and MIDI passes, the latter group only. None of these is on the path of a gate that passed before it. After the
last gate, `permission_manager.rs` was run through `rustfmt` with the tree's `rustfmt.toml` - three hunks this
job's edits had left unformatted, formatting only - and `cargo check --bin permission_manager` passed after it.

Host checks at the end: `./check.sh --gate grant-vocabulary`, `milestone-index`, `source-hygiene`,
`verify-model` and `declared-interfaces` - all PASS. `bootstrap-plan` fails only on the pre-existing
`font_catalogue` mismatch; `dynamic-report` cannot be checked without the AArch64 and RISC-V builds.

Not performed (the owner runs the long and cross-architecture work): `./build.sh --arch aarch64` and
`./build.sh --arch riscv64` (also with `LIBER_DEVELOPMENT=1`), `./check.sh --refresh dynamic-report` then
`./check.sh --gate dynamic-report`, `./verify.sh --plan` and `./verify.sh`.

## Continuation (2026-09-24, from 01:33Z): the verification items the first pass left open

### Common to P02M0180-P02M0188 (recorded in each of the nine files)

The first verification pass (above) ended with all nine gates passing on x86_64 and these items still open
across the nine milestones: the AArch64/RISC-V cross-builds every registration item names, and the dynamic
report that cannot be checked without them; the in-guest key/store-capability denial and stale-handle
rejection the Bluetooth gate item names (P02M0180); tests of refusal and reclamation at the modem service's
provider, client and context bounds, and the existing network guest checks that milestone names (P02M0183);
and for the administrative path (P02M0188) the demonstration that bypassing protected presentation or the
executor boundary fails its gate, `./verify.sh --plan` and `./verify.sh`. This continuation added the missing
assertions, re-ran the three gates they live in, ran the host checks and the existing x86_64 checks the job's
changes reach, and left the slow-architecture work - the cross-builds and the dynamic report - for last, once
nothing else was open.

What changed in the tree in this continuation. Development-only probes, the gate scripts, the kernel test suite
and its Cargo manifest, and `gen.sh`; no production service, driver, library, IDL or manifest row changed:

- `src/user/services/core/src/btcheck.rs` (01:36Z): the new phase `deny`, and stale-handle assertions added to
  `refund` and `loss`. `src/tools/check-bluetooth-service.sh`: runs `btcheck deny` after `btcheck pair` and
  requires its PASS line.
- `src/user/services/core/src/modemcheck.rs` (01:42Z): the new phase `context`. `src/tools/check-modem-service.sh`:
  runs `modemcheck context` after `modemcheck activate`, and a third part runs the new kernel scenario below
  through `TEST_SELECTION=... ./test.sh --arch x86_64`, reading its verdict from that run's own `RESULT-LOGS`.
- `src/kernel/test_suites/services.rs` (01:41Z): `mod modem_rig` and the scenario
  `kernel.services.modem_service_refuses_at_its_provider_and_client_bounds_and_gives_them_back`.
  `src/kernel/Cargo.toml`: `modem-device-proto` and `modem-proto` as path dependencies, as the other harness
  wire crates (`network-proto`, `display-device-proto`, `printer-device-proto`) already are, so the rig answers
  and reads both wires through the generated code.
- After the modem gate's first run of its new part (see P02M0183's section): `src/tools/check-modem-service.sh`
  (02:29Z) runs `test.sh` without the gate's `QEMU_EXTRA`, and the scenario's wait for a new publication pumps
  the scheduler (`services.rs`, 02:37Z).
- `gen.sh` (02:46Z): the five hyphenated package keys this job added to its `EXTERNAL` table are quoted - see
  "Found in this continuation" below.
- For the P02M0188 demonstration only, two planted defects, one at a time, each in one file; each file was
  copied aside first and restored from the copy afterwards, and both restorations were checked with
  `sha256sum -c` against hashes taken before the first plant (both `OK`). Neither defect is in the tree.

### Found in this continuation, and changed (common)

- `./gen.sh --check` failed at once: "line 180: EXTERNAL[$package]: unbound variable". The five package keys
  this job added to `gen.sh`'s `EXTERNAL` table - `[modem-device]`, `[camera-device]`, `[midi-device]`,
  `[printer-device]` and `[ptp-transport]`, one each for P02M0183-P02M0187 - were written unquoted, and a shell
  formatter reads an unquoted hyphen in a subscript as arithmetic: `shfmt` (which `format.sh --changed` runs,
  and `commit.sh` runs that before a commit) had rewritten them as `[modem - device]` and so on, keys nothing
  looks up. The comment above `["display-device"]` in the same table describes exactly this. All five are now
  quoted as that one is; `shfmt -d gen.sh` reports no change, so the next format pass leaves them alone, and
  `./gen.sh --check` passes: "no drift: 33 packages, the aggregate and every profile regenerates to what is on
  disk" (20 s). The bindings on disk were not stale - only the script that checks and regenerates them was
  broken, which is why the check is the thing that found it.
- Formatting of everything the job changed, checked without writing: `rustfmt +nightly --edition 2024
  --config-path rustfmt.toml --check` over the 266 Rust files, `shfmt -d` over the 13 shell scripts and `taplo
  fmt --check` over the 25 TOML files changed since `13309309` - all three clean (the same tools and flags
  `format.sh --changed` applies).

### Verification in this continuation, before the cross-builds (x86_64 host and guests, one guest at a time)

| What | Command | Result | Finished (UTC) |
| --- | --- | --- | --- |
| Bluetooth gate | `./check.sh --gate bluetooth-service` | **PASS**, 841 s | 02:15:21Z |
| modem gate, first run | `./check.sh --gate qemu-modem-service` | **FAIL**, 821 s: both boots passed; its new third part could not start QEMU (see P02M0183) | 02:29:02Z |
| administrative path, clean tree | `./check.sh --gate qemu-admin-path` | **PASS**, 414 s | 02:35:56Z |
| modem bounds scenario alone | `env -u QEMU_EXTRA TEST_SELECTION=kernel.services.modem_service_refuses_at_its_provider_and_client_bounds_and_gives_them_back ./test.sh --arch x86_64` | **FAIL** (a scenario defect, see P02M0183), then after the fix **PASS**, 1 passed, 25 s | 02:36Z, 02:38Z |
| the same, with a bound mutated | as above, `MAX_PROVIDERS` 5, then `MAX_CLIENTS` 33 | **FAIL** each, as it must; file restored, `sha256sum -c` OK | about 02:45Z |
| generated bindings | `./gen.sh --check` | **FAIL** (the script), then after quoting **PASS**: no drift, 33 packages, 20 s | 02:46Z, 02:47Z |
| every crate host suite | `./check.sh --gate host-tests` | **FAIL** (`driver-protocol`, see P02M0188), then after the fix **PASS**, 124 suites, 104 s | 02:48Z, 03:27Z |
| verification model | `./check.sh --gate verify-model`, `--gate verify-model-tests` | **PASS**, 19 s and 53 s | 02:50Z |
| manifest-derived checks | `./check.sh --gate declared-interfaces`, `--gate grant-vocabulary`, `--gate milestone-index` | **PASS** each | 02:50Z |
| source hygiene | `./check.sh --gate source-hygiene` | **PASS**, 72 s | 02:51Z |
| bootstrap plan | `./check.sh --gate bootstrap-plan` | **FAIL**, only the pre-existing `font_catalogue` mismatch: the manifest says `restart = "transparent"` for it and `relaunch_service` has no arm for it, which is so at `a07f535b` too; every service of this job agrees with its plan | 02:50Z |
| formatting, check mode | `rustfmt +nightly --edition 2024 --config-path rustfmt.toml --check`, `shfmt -d`, `taplo fmt --check` over every file changed since `13309309` | **PASS** (266 Rust, 13 shell, 25 TOML files) | 02:20Z |
| kernel tests of the changed components | `./test.sh --arch x86_64 --tags permission-service,process-service,network,input,mouse,display,usb --timeout 1800` | **PASS**, 48 passed, 167 s | 02:59:04Z |
| the IPv6 peer gate | `./check.sh --gate ipv6-peer` | **FAIL**, 106 s, the quiet row (see P02M0183) | 03:02:56Z |
| modem gate, end to end | `./check.sh --gate qemu-modem-service` | **PASS**, 835 s | 03:18:33Z |
| the DHCP test | `TEST_SELECTION=kernel.services.dhcp_lease_renews_at_t1_and_restarts_its_clock ./test.sh --arch x86_64` | **PASS**, 1 passed, 26 s | 03:19Z |

What changed after each gate passed, and why it is not on that gate's path: after the Bluetooth pass, the modem
gate script and its kernel scenario, `gen.sh` and a host-only test file; after the administrative-path pass, the
same; after the modem pass, the host-only test file (`driver/protocol/src/tests.rs`, compiled only under
`cfg(test)`). None of them is in an image a gate boots.

Tree changes this continuation did NOT make, for the record: the user committed the tree three times while it
ran (`ea33ffd0` and `47f9544c` at 02:15-02:16Z, `0fbe2ad0` at 02:38Z); `commit.sh`'s format pass changed no source
file (no file under `src/` was newer than 02:14Z afterwards). Cargo rewrote two lock files while the host
checks built: `src/proto/Cargo.lock` gained `admin-proto` (the aggregate crate's new dependency, from P02M0188)
and `src/tools/font-gen/Cargo.lock` dropped a `smartcard-model` entry (this job made it a dev-dependency of
`service-logic`, which `font-gen` reaches as an ordinary dependency).

### P02M0183: refusal and reclamation at the three bounds, tested

The limits item ends "Test capacity refusal and reclamation rather than removing bounds"; the first pass read
the stated limits (`modemcheck limits`) and tested none of the three refusals. Now each bound is driven to its
refusal and back, where it can be reached:

- ONE ACTIVE CONTEXT, in the guest (`modemcheck context`, `one_context` in `modemcheck.rs`; the first boot, no
  NIC). With one context up, a second `activate` must answer `again` (busy) and the fixture's activation count
  must not move - the refusal happens before the modem sees anything. The first context is then deactivated
  and NetworkService must drop its link; a new activation must then succeed with a DIFFERENT context id (the
  slot came back, and it is not the old context revived), and is deactivated in turn. PASS line: "PASS context:
  a second activation while one context was up was refused before the modem saw it, and the slot came back when
  it ended".
- FOUR PROVIDERS AND 32 CLIENT CONNECTIONS, in the test kernel
  (`kernel.services.modem_service_refuses_at_its_provider_and_client_bounds_and_gives_them_back`,
  `src/kernel/test_suites/services.rs`). Reaching them in a guest needs five modems and thirty-three clients;
  the kernel harness plays both. `modem_rig` starts the production `modem_service` from the package with its
  four roles in manifest order (CATALOGUE, SERVE, ADMIN, LINK), plays the provider catalogue (a `modem`
  subscription answered with a stream whose first frames are the publications, and `open` answered with a
  fresh provider connection) and plays each modem's provider wire through the generated
  `modem_device::dispatch` - a session at the contract's version, the two streams (the indication stream
  opening with the current state, which admission waits for) and the transmit channel. The scenario publishes
  five modems: four are admitted (`limits` reads providers 4 of 4) and the fifth is refused before it is even
  opened (the rig asserts no second `open` of any modem and counts the ones opened). One admitted modem is then
  withdrawn: the service gives its slot back (providers 3), and a sixth publication takes it (opened, providers
  4). Then thirty-two observation connections are minted from the root and the thirty-third is refused (the
  service answers the connect with no capability); closing one gives its slot back and the next connect is
  admitted again. A `limits` read is made through a connection of its own, dropped after the read; the provider
  counts are asserted from it, and the client bound is counted by minting connections, not read from `limits`.
- The gate script runs that scenario as its third part:
  `TEST_SELECTION=kernel.services.modem_service_refuses_at_its_provider_and_client_bounds_and_gives_them_back
  ./test.sh --arch x86_64`, and passes only if `test.sh` exits 0, its `RESULT-LOGS` line names readable logs,
  the scenario's id is in them and no `[failed]` line is.

Found by the first run of the new part (the gate at 02:15-02:29Z: both boots passed, `modemcheck context`
included; the third part failed), and changed:

- The gate exports `QEMU_EXTRA="-device edu,addr=0x1c"` for the fixture's device, and `QEMU_EXTRA` reaches
  `test.sh`'s machine too, whose PCI bridge sits at slot 0x1c: QEMU refused to start ("slot 28 function 0 not
  available for edu, in use by pcie-pci-bridge,id=liberbr"). The third part now runs `env -u QEMU_EXTRA
  TEST_SELECTION=... ./test.sh --arch x86_64`.
- Run by itself, the scenario then failed at "the slot a withdrawn modem gave back was taken by a new
  publication": its wait for the sixth modem only pumped the scheduler through `limits()`, which the `&&`
  skipped while the modem was not yet opened - so nothing ran and the publication was never seen. The loop now
  pumps on every pass until the new publication is opened (bounded by `PASSES`), then asserts providers 4. A
  defect in the scenario, not the service: the guest log shows the service admitting the sixth modem as soon as
  it is allowed to run.

WATCHED FAILING, as the tree asks of a new test: `MAX_PROVIDERS` set to 5 in `modem_service.rs`, rebuilt and
run - `[failed]`, "four of five published modems were admitted"; `MAX_CLIENTS` set to 33 - `[failed]`, "a
thirty-third client connection was admitted". The file was restored from a copy taken before the first change
and `sha256sum -c` answered `OK`. On the clean tree the scenario passed by itself: `env -u QEMU_EXTRA
TEST_SELECTION=kernel.services.modem_service_refuses_at_its_provider_and_client_bounds_and_gives_them_back
./test.sh --arch x86_64` - 1 passed (25 s), the guest log showing four modems admitted, one refused ("this
service holds four (resource exhausted)"), one withdrawn, and the new one admitted.

Run: `./check.sh --gate qemu-modem-service` - **PASS**, 835 s, finished 2026-09-24T03:18:33Z, on the development
image built at 02:56Z from this tree. Its lines include modemdata PASS; modemcheck PASS pin, identity, activate,
context, drop, rollback, inherit, sim, uncertain, limits, republish; "the service's handles (13) and clients (5)
returned to their baseline"; modemcheck PASS busy; modemswap PASS; "four modems and thirty-two clients were
admitted, the next of each refused, and both slots given back"; and the gate's PASS line naming the provider,
client and context bounds.

Ticked in `docs/todo/P02M0183.md`: the limits item.

### P02M0183: the existing network guest checks - one passes, one fails, and why

The run item names "affected existing network guest checks". There are two: the kernel test tagged `network`
(`kernel.services.dhcp_lease_renews_at_t1_and_restarts_its_clock`, which drives the real NetworkService) and the
`ipv6-peer` gate (three guests against a scripted peer). Beside them, the x86_64 kernel tests of the components
this job changed were run by tag.

| Check | Command | Result |
| --- | --- | --- |
| kernel tests of the changed components | `./test.sh --arch x86_64 --tags permission-service,process-service,network,input,mouse,display,usb --timeout 1800` | **PASS**, 48 passed (167 s), 02:59Z. Includes the two PermissionManager summary tests this job changed (`..._enforces_static_and_dynamic_probe_policy`, `..._runs_tools_with_minimal_grants`), which had not run since the vocabulary grew; not the DHCP test, which is `Slow` and is skipped by a tag run |
| the DHCP test | `TEST_SELECTION=kernel.services.dhcp_lease_renews_at_t1_and_restarts_its_clock ./test.sh --arch x86_64` | **PASS**, 1 passed (26 s), 03:19Z |
| the IPv6 peer gate | `./check.sh --gate ipv6-peer` (after `LIBER_DEVELOPMENT=1 ./image.sh`) | **FAIL**, 106 s, 03:02:56Z, at its first row |

THE FAILURE. The quiet row captures for 45 s and requires three router solicitations in the RFC 7559 schedule
(IRT, then about twice the previous interval). The capture held two: from `::` at 37.769 s and from the
link-local address at 41.318 s; the third would have been due near 48.4 s. Everything else the row asserts was
there (the report before detection, the detection probe, the re-report, `routers=0`). The row fails on WHEN the
guest reaches the wire, not on what it sends.

WHY, MEASURED. A reproduction of the row by hand (`ipv6-peer.py --scenario quiet`, the guest as the gate boots
it: `NET_PEER_PORT=... ./run.sh --arch x86_64 --smp 2`, every serial line timestamped as it arrived) failed the
same way (solicitations at 36.763 s and 40.345 s), and its log shows the cause. ServiceManager's bring-up is
sequential - each service is started and its report awaited before the next - and each pass walks
`manifest.toml`'s `[[services]]` in order. This job inserted its ten services at positions 1-10, right after
`audio_service`; seven of them (`power_service`, `smartcard_service`, `camera_service`, `midi_service`,
`spool_service`, `media_import_service`, `bluetooth_bond_store`) have all their dependencies up in the same pass
as `config_service` (position 11), which NetworkService depends on, and so start ahead of it. On the timestamped
boot, SessionService reported at 34.21 s and ConfigService at 37.48 s; before this job only AudioService stood
between them. The serial lines arrive in batches, so the figure is an estimate: about 2.5 s of NetworkService's
start is these services. That is about what the row misses by, so giving it back would put the third
solicitation at the very end of the capture - the row was not far from its edge before them either.

WHAT WAS TRIED, AND STOPPED. Moving the ten service blocks after the existing twenty-four in `manifest.toml` - which
restores every existing service's start order and costs the display and shell the same seconds instead - was
refused by this session's permission classifier as a change to a shared resource, and was not attempted another
way. Nothing was changed. The owner's choice: move the new services after the existing ones (and re-run
`./check.sh --gate ipv6-peer`), or give the row's capture more room (its assertions about the schedule's shape
would be unchanged), or both. The run item stays open on this, and on the cross-builds.

The cross-builds, the dynamic report and the verification planner run last, once, and are recorded in the next section.

## Final state of the job (2026-09-24T04:30Z): the cross-builds, the dynamic report and the verification plan

### Common to P02M0180-P02M0188 (recorded in each of the nine files)

The slow-architecture work ran last, once, after every other item of the nine milestones was done or had been
reduced to a decision that is the owner's. It found one more defect of this job's, which was fixed before the run
was repeated.

### Found by the AArch64 and RISC-V builds, and changed

The first attempt at the final run failed on both ports at the same place, the build's check that every import
of a dynamic program is defined by exactly one of its declared providers:

    build-shared: media_import_service import _RNvMs3_..._5alloc7raw_vecINtB5_6RawVecmE8grow_oneCs3SQuU4xAvRw_4wasm
    has 0 declared providers (expected 1)

(`./build.sh --arch aarch64`, 433 s, and the same on the retry; `./build.sh --arch riscv64` with `wasm`'s RISC-V
hash.) `RawVec<u32>::grow_one` - `m` is `u32` in the v0 mangling - is what `Vec<u32>::push` calls when it has to
grow. With generic sharing, rustc does not instantiate it in a program if a crate the program loads already
exports it; of several exporters it takes the one with the lowest StableCrateId, and StableCrateIds differ
between targets. P02M0183's `modem-device.lsidl` has `@bound(2) dns: list<u32>`, so the generated decoder makes
`modem-device-proto` an exporter, and every program using the `proto` aggregate loads it. On x86_64 it won for
the four programs that push a `Vec<u32>` - `kill`, `traceroute`, `spool_service`, `media_import_service` - which
is why the first pass declared `modem-device-proto` as a provider of all four. But the two services also load
`wasm` (through `service-logic`), which exports the same instance, and reading the crate hashes out of the symbol
names (base 62, most significant first) shows `wasm`'s is the lower one on AArch64 (`3SQuU4...` against
`4Vf2JM...`) and on RISC-V (`4qHskh...` against `bj6yQv...`), while `modem-device-proto`'s is lower on x86_64
(`3X374D...` against `ju6TMu...`). So on the ports those two services import it from `wasm`, which they do not
declare, and no single providers list - the manifest has one per program, not one per target - is right on all
three. `kill` and `traceroute` load no other exporter; on AArch64 `kill` imports it from `modem-device-proto` as on
x86_64, and the RISC-V build below passed them too.

Changed (P02M0186's and P02M0187's services, and their manifest rows):

- `spool_service.rs`, `pump`: the printers lost in one pass are kept in a fixed `[u32; jobs::MAX_PRINTERS]` with a
  count - at most one per printer, and adoption refuses a printer beyond `MAX_PRINTERS` - instead of a `Vec<u32>`.
- `media_import_service.rs`, `tick`: the idle transfers' devices are collected (`filter`, `map`, `collect`) before
  the transfers are failed in a second loop, instead of being pushed one by one. `Transfer::timed_out` only reads
  the transfer, so both loops select the same transfers. `collect` into a `Vec<u32>` grows through the
  allocator's type-independent reserve path, not through `grow_one` - the service's existing `collect`s into
  `Vec<u32>` import nothing.
- `manifest.toml`: `modem-device-proto` and its explanatory comment are gone from both programs' providers.

Verified: `LIBER_DEVELOPMENT=1 ./build.sh --arch x86_64` passed (256 s) and neither program imports the routine
any more (`llvm-readelf --dyn-syms`, 0 each); `./check.sh --gate spool-service` **PASS** (40 s, 03:53:46Z) and
`./check.sh --gate media-import-service` **PASS** (39 s, 03:54:25Z); the format check of the three files is clean;
and the ports then built (below).

### The final run

| What | Command | Result | Finished (UTC) |
| --- | --- | --- | --- |
| AArch64 build | `./build.sh --arch aarch64` | **PASS**, 326 s. The first attempt died on this machine's intermittent rustc SIGILL while compiling the kernel ("rustc interrupted by SIGILL"); the one retry built (47 s) | 03:59:51Z |
| RISC-V build | `./build.sh --arch riscv64` | **PASS**, 318 s, first attempt | 04:05:09Z |
| AArch64 development build | `LIBER_DEVELOPMENT=1 ./build.sh --arch aarch64` | **PASS**, 110 s | 04:06:59Z |
| RISC-V development build | `LIBER_DEVELOPMENT=1 ./build.sh --arch riscv64` | **PASS**, 103 s | 04:08:42Z |
| the growth routine's importers, all three targets | `llvm-readelf --dyn-syms` on the staged programs | `kill` and `traceroute` import it from `modem-device-proto` on x86_64, AArch64 and RISC-V alike; `spool_service` and `media_import_service` import it nowhere | 04:06Z |
| the dynamic reports | `./check.sh --refresh dynamic-report` | **PASS**, 335 s. Rewrote `docs/DYNAMIC_EXECUTABLES.tsv` (228 rows), `docs/DYNAMIC_WAVES.tsv` (18 rows) and `docs/DYNAMIC_IMAGE.tsv` (3 rows). Staged bytes per target: x86_64 7652888 -> 8342560, AArch64 8232032 -> 8985328, RISC-V 8402816 -> 9177000 - measured against the report as it was last refreshed (`c822f5f2`, 2026-09-19), so the difference is everything since then, not this job alone | 04:14:17Z |
| the dynamic report check | `./check.sh --gate dynamic-report` | **PASS**, 339 s | 04:19:56Z |
| the verification planner | `./verify.sh --plan` | "FULL verification", because `gen.sh` belongs to `harness.scripts` and `src/user/services/manifest.toml` to `manifest`; build and boot on all three targets; provisional estimate 8952 s against 9005 s for everything | 04:20:31Z |
| host checks on the final tree, after the last source edit | `./gen.sh --check`; `./check.sh --gate` `host-tests`, `verify-model`, `declared-interfaces`, `grant-vocabulary`, `milestone-index`, `source-hygiene`, `bootstrap-plan` | **PASS** each (no drift in 33 packages; 124 crate host suites in 106 s; hygiene 74 s), except `bootstrap-plan`: **FAIL** on the pre-existing `font_catalogue` mismatch alone | 04:24:52Z |

NOT RUN, and why: `./verify.sh` itself. Its plan is the whole suite on three architectures (about two and a half
hours, most of it emulated guests), and by the owner's standing rule the big suites are theirs to start; the job
hands them the command. No guest was booted on AArch64 or RISC-V: every gate of these milestones is x86_64-only by
its own statement, and the ports are cross-built.

### Where the nine milestones end

| Milestone | State | What keeps it open |
| --- | --- | --- |
| P02M0180 | OPEN | the operator item and the InputService item - two departures from the plan's wording, stated in the plan with their reasons, the owner's to accept |
| P02M0181 | OPEN | the three items that belong to or wait on P02M0099's real HID and ACPI producers; the service-first stage is done |
| P02M0182 | COMPLETE | - |
| P02M0183 | OPEN | the run item, on the existing `ipv6-peer` gate alone: its quiet row fails because the new services start ahead of NetworkService; the fix is the owner's decision (move the new services after the existing ones in `manifest.toml` - which this session's permission classifier refused as a change to a shared resource - or give the row's capture more room) |
| P02M0184 | COMPLETE | - |
| P02M0185 | COMPLETE | - |
| P02M0186 | COMPLETE | - |
| P02M0187 | COMPLETE | - |
| P02M0188 | OPEN | the registration item, on `./verify.sh` alone - the owner's run |

Decisions taken in this job that close nothing and block nothing, listed so the owner can overrule any of them:
the kernel's `LIVEVOL` rule - the medium's image is handed over as the live system volume only when the loader
chose no block volume (a boot-chain change, found by the Bluetooth gate's cold reboot); `bootproto::manifest::
MAX_ROWS` 256 -> 512; `MAX_BIND_RESOURCES` 5 -> 6 (the trusted key sink is a sixth keyboard resource);
PermissionManager's probe rows and fixture-control path no longer `cfg`-gated; `btcheck` and `cardcheck` holding
the supervisor's admin channel to stop and restart their service themselves (development-only programs); the
scenario runner's `unordered` expect option; AdminService's `MAX_TESTERS` of four and its epoch rule (the highest
recorded plus one); `kill` and `traceroute` declaring `modem-device-proto` for a shared generic instance.

### P02M0183 at the end

Nothing newly ticked: the run item stays open on the `ipv6-peer` failure alone; its cross-builds, dynamic report,
host and generation checks are done. The `dns: list<u32>` field of `modem-device.lsidl` is what makes
`modem-device-proto` an exporter of `Vec<u32>`'s growth routine. It was left as it is: `kill` and `traceroute`
resolve that routine to the same crate on all three targets, and the two services that could not no longer need it.

## Addendum (2026-09-24T05:45Z): `ipv6-peer` on the shipping image

The final section above records `ipv6-peer` failing its quiet row. That run was on the DEVELOPMENT image
(`LIBER_DEVELOPMENT=1 ./image.sh`), whose boot carries development-only work (the development agent came online
about 5.5 s after the drivers on a timestamped boot, and DeviceManager's budget fixture takes three attempts).
`./verify.sh --plan` builds the shipping image ("shared-image") for its gates, so the gate was run there too.

| What | Command | Result | Finished (UTC) |
| --- | --- | --- | --- |
| shipping x86_64 build and image | `./build.sh --arch x86_64`, `./image.sh` | **PASS**, 97 s | about 04:32Z |
| the quiet row by hand, timestamped | `ipv6-peer.py --scenario quiet` and `NET_PEER_PORT=... ./run.sh --arch x86_64 --smp 2` | three solicitations at 32.353, 35.946 and 43.149 s (intervals 3.59 and 7.20 s) - the row's schedule assertions hold; on the development image the same reproduction saw two (36.763, 40.345) | about 04:34Z |
| the gate, first run | `./check.sh --gate ipv6-peer` | **FAIL**, 1314 s: quiet, router, hostile, flood, echo, narrow, transport, fallback and budgets passed; `hostile-quote` failed "the guest stopped answering IPv4" - the peer's single IPv4 ping, sent 20 s after it starts, was never answered | 04:56:34Z |
| the gate, second run | the same | **FAIL**, 1313 s: the same nine rows passed; `hostile-quote` failed "the listener never started" (`httpd &` was typed, and no "listening on port 80" line followed) | 05:21:32Z |
| `hostile-quote` by itself, six times | the gate's `row` by hand: `ipv6-peer.py --scenario hostile-quote`, `guest-console.py` typing the gate's five lines, the same `run.sh` invocation; four runs in the default `public` run mode and two with `LIBER_RUN_MODE=gate`, as `check.sh` exports | **all eight of the row's assertions held in all six**; the early ping was answered at 31.4-32.6 s, the moment the guest's stack started (its first router solicitation in the same millisecond) | 05:25-05:40Z |
| the dynamic report against the shipping build | `./check.sh --gate dynamic-report` | **PASS**, 328 s (tools and providers carry no development feature, so the report is the same for both builds) | 05:46Z |

What this does and does not establish. The row asserts on the peer's one IPv4 ping, sent at a fixed 20 s, being
answered - on this machine about 12 s before the guest's network starts, so the frame waits somewhere between QEMU
and NetworkService and is answered when the stack comes up. The NetworkService code it passes through is the same
as before this job: `do_dhcp` is byte-identical, and the bring-up (open the published NIC through the catalogue,
read its greeting, attach the IPv6 host, run DHCP) reads frames in the same order as the pre-job
`take_published_nic` path (compared against `048e0abe`). The virtio-net driver was not changed. The two failures
came only as the tenth boot of a full run, with a different symptom each time, and the row never failed by itself.
Whether the full gate passed on this machine before this job is not established - there is no pre-job build to
run it on - so this is recorded as an open failure, not attributed and not dismissed.

P02M0183's run item stays open on `ipv6-peer` alone. The owner's options, none taken here: move the ten new
services after the existing ones in `manifest.toml` (measured to give the network back about 2.5 s; refused to
this session by the permission classifier), give the quiet row's capture more room, and run the gate on a tree
from before this job to see whether `hostile-quote` failed there too.

## Addendum (2026-09-24T06:15Z): a third `ipv6-peer` run, with every row's console kept

The gate prints only the guest's `ipv6:` lines on failure and deletes its work directory, so a third run on the
shipping image was made with each row's files copied out while it ran (`guest`, `capture`, `peer`, `driver`, `run`,
`script`; the gate and the tree unchanged).

| What | Command | Result | Finished (UTC) |
| --- | --- | --- | --- |
| the gate, third run | `./check.sh --gate ipv6-peer`, with a copier outside it | **FAIL**, 1145 s: quiet, router, hostile, flood, echo, narrow, transport and fallback passed; `budgets` failed "the listener never started" | 06:09:19Z |

The `budgets` transcript shows the cause. The console driver (`guest-console.py`) sends a whole line the moment
it sees `vol://system> ` after "shell attached"; here the line `httpd &` reached the shell split after its first
character:

    vol://system>
    # guest-console sent: httpd &
    h
    ttpd &
    unknown command: h (Tab Tab lists the commands)
    vol://system>
    unknown command: ttpd (Tab Tab lists the commands)

- so no listener was ever started, and the row failed on its first assertion. The same symptom ended the second
run's `hostile-quote`. In the kept logs of this run the other scripted rows' first lines arrived whole, and in one
of the six standalone `hostile-quote` runs an empty line arrived before `httpd &` - harmless there, the same seam.

Where that happens, and what this job changed there: nothing. `console_service.rs` (whose line discipline hands
the shell its input), the serial input path, `src/harness/guest-console.py`, `src/harness/ipv6-peer.py` and
`src/tools/check-ipv6-peer.sh` are all as they were at `048e0abe` (`git diff --stat 048e0abe HEAD` over them is
empty). The shell changed in this job only in its program table and in running `tool &` in the background
through the broker - after a line has been read, not while it is being read.

So across three full runs on the shipping image every row has passed in at least one run, the failing row
differed each time (`hostile-quote`, `hostile-quote`, `budgets`), and the two causes found - the first typed line
arriving split, and the peer's single early IPv4 ping going unanswered - lie in code this job did not change.
That is evidence, not proof, that the failures predate the job: no pre-job build was run. P02M0183's run item
stays open on it, because the gate it names has not passed; the owner's options are the three recorded above,
and a fix to the first-prompt race (the driver waiting for the prompt to settle, or the line discipline taking a
line typed at once) would belong to the console or the harness, not to these milestones.
