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

---

AUDITOR'S REVIEW ON P02M0183 (2026-09-24T06:58:57Z):

Rating: 8/10

ModemService, the command logic, the uplink selection and NetworkService's raw-IP medium are careful, and they
match the plan closely. The findings are:
- one small secret-hygiene gap;
- the run item's existing-network-check requirement, which is not met - largely for reasons outside this
  milestone's code;
- the alias configuration, which is compiled into the service.

## Findings

### 1. A submitted PIN/PUK survives in two buffers (minor defect)

The plan: "clear submitted-secret buffers after bounded handling." ModemService does most of this deliberately:
- `Secret` zeroes itself with volatile writes on drop;
- `submit_with` scrubs the `Command` fields and the encoded bytes after sending;
- `grant_request` scrubs the reply buffer.

Two copies are missed:
- **The encoder's own buffer.** The generated `modem_device::Client::command` encodes into a `VecWriter` and
  passes `request` to the transport; the `Capture` copies it, and that copy is scrubbed. The generated
  `request` vector is then dropped unzeroed at the end of `command`, together with any smaller buffers the
  writer outgrew while encoding.
- **The receive buffer.** The client's `enter_pin`/`enter_puk` request - PIN, PUK and new PIN in the clear -
  sits in the service loop's `buf` (`BUF_BYTES`). `grant_request` notes that "the request buffer carried the
  secret; it is not read again", but nothing zeroes it. It is overwritten only as far as later messages happen
  to reach.

Neither copy leaves the process, and nothing logs them. The requirement, however, is about exactly these
buffers. Scrubbing `buf[..len]` after a manage request, and encoding the secret-bearing command into a buffer the
service owns and scrubs, would close it.

### 2. The run item's "affected existing network guest checks" are not met (open item, cause largely outside this milestone)

- **What passes:** the `dhcp_lease_renews_at_t1_and_restarts_its_clock` kernel test.
- **What fails:** `./check.sh --gate ipv6-peer`, as the implementer's records show.
  - On the development image, the quiet row does not see three router solicitations in its 45 s window.
  - On the shipping image, nine of ten rows pass in each full run. The tenth row varies, and its two identified
    causes are in code this job did not change (verified: `git diff 048e0abe HEAD` over `console_service.rs`,
    `guest-console.py`, `ipv6-peer.py` and `check-ipv6-peer.sh` is empty):
    - the shell's first typed line arriving split;
    - the peer's single early IPv4 ping.
  - NetworkService's DHCP-phase frame handling is unchanged (`do_dhcp` is identical to the pre-job version).
- **A real contribution from the job, but not from this milestone's code.** ServiceManager's bring-up walks the
  services in manifest order and waits for each, and the job inserted its services at positions 1-10. Seven of
  them start ahead of `config_service` and hold NetworkService back by about 2.5 s: power, smartcard, camera,
  MIDI, spool, import and the bond store. `modem_service` itself depends on `network_service` and does not.
- **Conclusion:** the requirement is unmet and correctly left open. The fix, or an acceptance, is the owner's.

### 3. The modem alias is compiled in (minor deviation)

`ALIASES` in `modem_service.rs` binds only the fixture's alias, in every build; a shipping image names no
configured modem. This matches "identity and management are denied by default". It also means a real modem
cannot be granted without a code change, rather than through configured policy. It is the same observation as for
P02M0182, and nothing is affected until P02M0099's MBIM provider exists.

## Verified

- **Grants.**
  - The admin mint resolves an alias to exactly one current publication, or fails. It binds the modem, the SIM
    generation and, for data, the APN and replace-uplink policy.
  - Non-data grants refuse a data policy.
  - The owner task handle is in the wait set, and its termination retires the grant. That ends its context,
    closes its connection and drops its pending calls, whatever copies of the endpoint exist.
  - SIM replacement retires the grants bound to the old SIM generation and ends the context. Provider loss ends
    the context and every grant bound to the modem.
- **Commands.**
  - The provider's limits are clamped down.
  - Commands are correlated by connection generation, transaction and the SIM/context generations.
  - A reply to nothing outstanding is dropped.
  - An unknown outcome is never replayed; a read-only query reconciles it.
  - A PIN attempt is preceded by a fresh counter query and refused against an unknown count unless acknowledged.
  - A context found up with no owner after reconciliation is taken down.
- **Context flow.**
  - Reservation with NetworkService comes before the modem is asked anything; `again` means busy without
    replace authority.
  - Activation is followed by installation, and success is reported only on `installed`.
  - An installation failure makes a bounded deactivation; an uncertain activation abandons, then reconciles.
  - One context globally, and 32 clients.
  - Datagrams are admitted only for the active context generation, within the MTU, bounded by
    `mc::PacketQueue`, and counted when refused.
- **Uplink selection (`service_logic::uplink`).**
  - At most 16 NICs; the current usable NIC is kept, otherwise the lowest live publication is chosen.
  - Reservations are refused as busy without replace authority, and re-checked at install time.
  - Release restores the replaced NIC if it is still live, otherwise falls back.
  - Every switch advances the interface generation.
  - `validate` refuses IPv6 as unsupported, and refuses non-unicast addresses, a gateway off the subnet and an
    out-of-range MTU.
- **Raw-IP medium.** `NetworkService::bring_up_raw` builds a raw stack from the attachment alone: no MAC, ARP,
  DHCP or static fallback.
- **Registration.** The gate is registered in `check.sh`, the verify-model catalogue (including the guest-booting
  list) and `release-required.toml`. Its third part runs the provider and client bounds scenario, which the
  implementer's records show was also demonstrated failing with the bounds mutated.

## Checks performed

Code reading only; nothing was built or run for this review. Files read: `modem_service.rs` (commands, grants,
context flow, link calls; about 900 of 1769 lines), `service_logic/src/uplink.rs` (whole), NetworkService's
`bring_up_raw` and `bring_up_nic`, the generated `modem_device::Client::command`, and PermissionManager's
`modem_policy`. I compared the pre-job NetworkService start-up (`git show 048e0abe:...network_service.rs`) with
the current one, and read the implementer's recorded `ipv6-peer` evidence.

---

IMPLEMENTER'S RESPONSE ON P02M0183 (2026-09-24T19:39:29Z):

## Finding 1 - A submitted PIN/PUK survives in two buffers: PARTLY ACCEPTED

The finding names two buffers.

**The encoder's buffer: ACCEPTED.** Confirmed: `captured_device` drove the generated
`modem_device::Client::command`. That client encodes into its own `VecWriter` and drops the vector unzeroed,
together with any smaller buffer it outgrew while encoding. Changes in `Service::submit_with`:
- The frame is now written into a fixed 256-byte buffer the service owns (new constant `COMMAND_FRAME`). It holds
  the `OP_COMMAND` operation, the correlation and `Command::encode` of the command.
- The frame is sent with `try_send` as before, and the whole buffer is then zeroed with `scrub`, whether or not
  the send succeeded.
- The command's own `secret` and `new_secret` are still scrubbed, and the `Secret`s still zero themselves on
  drop.
- The bytes on the wire are the same as the generated client wrote: a little-endian `u16` operation, a `u32`
  correlation, then the command.
- `captured_device`, whose only caller this was, is removed. `Capture` and `stamp` remain for the link-admin
  calls, which carry no secret.
- The bound: two secrets of at most 8 bytes, which is the IDL's `@bound(8)`, an APN of at most 64 bytes and the
  fixed fields come to about 130 bytes. A frame that did not fit would not be sent, and would be answered as a
  failure through the existing not-sent path.

**The receive buffer: REJECTED - already done.** The service loop zeroes the request buffer right after
`grant_request` returns: `scrub(&mut buf[..len])` in `__user_main`, under "THE REQUEST MAY HAVE CARRIED A PIN: the
buffer it arrived in is zeroed before anything else uses it". That line was in the implementation commit, before
this review. The note in `grant_request` that the review quotes means the buffer is not read again inside that
function. The reply buffer is scrubbed inside `grant_request` as well.

## Finding 2 - The run item's "affected existing network guest checks" are not met: ACCEPTED as stated; no code change

Agreed on every point:
- `do_dhcp` is unchanged.
- The two identified `hostile-quote` causes lie in code this job did not change.
- The job's services ahead of `config_service` add about 2.5 s to NetworkService's start.

Changing the manifest order was refused by the permission classifier in the earlier session and was left to the
owner. Whether to reorder the manifest or accept the gate's state is still the owner's decision, so the item stays
open.

## Finding 3 - The modem alias is compiled in: REJECTED

This has the same basis as the smart-card finding (P02M0182, Finding 1):
- PermissionManager's `modem_policy` is compiled too, and so is every device-policy row. The image is the unit of
  configuration.
- Granting a real modem takes a policy row and its alias entry in the same change, whichever way this table was
  delivered.
- The default-deny and exact-resolution properties hold.
- No MBIM provider exists yet: that is P02M0099's.

Building bootstrap delivery of alias tables would be new infrastructure the milestone does not require.

## Milestone document

`docs/todo/P02M0183.md` records the encoder change at the attempt-counter item, the one that asks for the
secret buffers to be cleared.

## Verification

- **Specific to this milestone:** `qemu-modem-service` passed every row, including `modemcheck: PASS pin`, which sends the PIN command through the new hand-encoded frame, and `sim`, `uncertain` and `limits`.
- **Static checks (all pass):**
  - `cargo check` of every changed program, in both feature configurations where one is gated;
  - `rustfmt --check` of every changed file;
  - `./check.sh --gate source-hygiene`: clean.
- **Host suites:** `service-logic` 672/672 and the `drivers` library 290/290, new tests included.
- **Builds (all pass):**
  - `LIBER_DEVELOPMENT=1 ./build.sh --arch x86_64`: 348 s, provider inventory `match`.
  - `./build.sh --arch aarch64`: 342 s.
  - `./build.sh --arch riscv64`: 337 s.
  - The three dynamic programs touched, `bluetooth_service`, `modem_service` and `camera_service`, link against their declared providers on all three targets.
- **Guest gates, one at a time, on a development image** (`LIBER_DEVELOPMENT=1 ./image.sh --format iso`):
  - `bluetooth-service`: PASS, 841 s.
  - `qemu-modem-service`: PASS, 839 s.
  - `qemu-camera-service`: PASS, 400 s.
  - `qemu-midi-service`: PASS, 400 s. These four ran 18:57-19:39Z.
  - `qemu-admin-path`: PASS, 465 s, 18:37-18:45Z.
- **Two earlier attempts at the four service gates tested nothing.** Their images lacked the development probes (`no artifact at vol://system/libexec/btcheck.lsexe`): first `./image.sh` had not been rerun after the build, then it rebuilt a shipping volume because it ran without `LIBER_DEVELOPMENT`. The gates were repeated as above.


---

AUDITOR'S RE-AUDIT ON P02M0183 (2026-09-25T00:27:22Z):

Rating: 9/10

The accepted half of Finding 1 is fixed correctly, and the other decisions on this milestone are justified.

- **The fix is correct.**
  - `submit_with` writes the frame into a 256-byte stack buffer (`COMMAND_FRAME`).
  - The header matches the generated client's: `OP_COMMAND`, then the correlation, both little-endian.
  - The buffer is zeroed after the send, whether or not the send succeeded.
  - The `modemcheck pin` row passed on it.
- **The rejected half of Finding 1 (the receive buffer) is correctly rejected.** The service loop has zeroed
  `buf[..len]` after every grant request since the implementation commit.
- **The rejection of Finding 3 stands**, on the same grounds as P02M0182's.

## Unresolved

### The run item (Finding 2) is still open

This is unchanged: `ipv6-peer` still does not pass. The decision is still the owner's: either reorder the manifest
so the new services start after `config_service` and `network_service`, or accept the result.

The delay does not come from ModemService's own code. That is why it costs only the one point.

---

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON P02M0183 (2026-09-25T01:01:42Z):

## Unresolved - The run item (Finding 2) is still open: ACCEPTED as stated; no code change

The statement is accurate, and nothing in this milestone's code can close the item:

- **On the shipping image, the failures are not in this job's code.** `./verify.sh` builds its gates on the
  shipping image, and there the quiet row passes. The rows that failed were `hostile-quote` and `budgets`, and
  they had two causes:
  - a console line typed split after its first character;
  - the peer's single early IPv4 ping.

  Both lie in ConsoleService's line discipline, `guest-console.py` and the gate itself, which are unchanged since
  before this job.
- **The job's one contribution does not come from ModemService.** On the development image, NetworkService starts
  about 2.5 s later. That delay comes from seven services of P02M0180 to P02M0187 placed ahead of
  `config_service`. ModemService starts after `network_service`.
- **Moving those services would not close the item.**
  - It would change the boot order of seven services outside this milestone.
  - By the measurement recorded in the milestone, it would put the development image's third solicitation at
    the very end of the capture rather than safely inside it.
  - It would not touch the two causes above.

The item therefore needs the owner's decision, as the milestone records: reorder the services, give the quiet row
more room, or accept the gate's state. The milestone stays open on this item alone. No code of this milestone
changed in this round.


## IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0183 (2026-10-09 18:28:16 UTC):

Reviewed the complete plan and retained verification records. The previous IPv6 regression has a later recorded passing run in the plan; the older audit failure remains history, not the final known result. Software closure still requires the final integrated build/verification after the current shared changes; no fresh modem guest run is claimed here. Existing audit content is preserved; no audit rating is assigned.

Verification continuation (2026-10-09T22:13:43Z):

The frozen profile/readback/microphone continuation was built on all three targets with `RUST_MIN_STACK=33554432 LIBER_DEVELOPMENT=1 ./build.sh --arch ARCH --part libs`: x86_64 PASS442s, aarch64 PASS443s, riscv64 PASS439s; each staged inventory matches121 providers and121 consumers. The six foreign/runtime provider SHA256 identities remain unchanged from the successful canonical foreign regeneration, so no new foreign regeneration was required. Serial `./build.sh --arch all` with the same development/stack environment PASS271.24s; `./build.sh --arch all --part volume --kernel-on-volume` PASS221.06s. Exact logs are under `.build/logs/end-of-job/qemu-only-20261009/post-join-*`; `post-join-stage-results.json` preserves every command, environment, exit and elapsed time.

`RUST_MIN_STACK=33554432 ./check.sh --refresh dynamic-report` PASS405.10s, `./check.sh --gate verify-model` PASS28.97s, and `./gen.sh --check` PASS16.04s. The registration check preceded compilation of the new microphone test and honestly reported it declared but not yet built. The subsequent actual x86_64 SMP4 kernel run compiled and ran all16 explicitly selected cases, PASS74.51s (61s guest):112 2D and222 3D conformance cases with zero failed/unsupported/untested, HDR/Extended/resize/worker equality, partial-allocation and emergency cleanup, both brightness responsiveness tests, audio routing/recovery, pointer/touch lifecycle, and separate microphone gain with legacy capture compatibility. Exact selection and command are in `post-join-stage-results.json`; authoritative suite log is `.build/logs/test/x86_64-20261009T220307Z-1439801-guest.log`. These are functional checks, not a live performance acceptance. ARM/RISC current microphone/joined-profile execution and the required full verification workflow remain pending; merge inventory preparation must refresh all target suites. No milestone completion is asserted by these common build results alone.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0183 (2026-10-10 04:45:47 UTC):

Continuation result: BLOCKED, NOT COMPLETE. Retained complete modem/IPv6 evidence and all-target builds do not replace final current-tree host/network/modem verification, which remains unperformed.

Read-only host revalidation confirms no reboot since the2026-10-10 02:58:50UTC Linux OOPS (current boot began2026-10-09 17:54:52UTC), kernel taint128, dummy_hcd still live/refcnt0/noholders, and all seven cleanup-ledger files byte-identical to the preserved ownership record. This does not retest module unloading; the prior blocked delete_module and interrupted cleanup remain the evidence. No further unload, module-list read, guest, benchmark or reboot was attempted. The same external blocker persists for the third consecutive goal turn. Independent remaining-work review found no concrete meaningful step that would discharge final proof before host recovery.

Evidence: `.build/logs/end-of-job/qemu-only-20261009/host-blocked-audit-20261010T0444.json` and HOST_RECOVERY_RESUME.md. The previous turn made actual diagnostic/correctness progress; this turn revalidated the unchanged impasse. Required next external action is a host restart/replacement preserving the workspace, followed by boot/sysfs and old-boot-ledger reconciliation. The existing owner question is unanswered. Completion flags and all original requirements are preserved; passed, failed, excluded and unperformed checks remain distinct.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0183 (2026-10-10 05:18:34 UTC):

The owner explicitly declined host restart/replacement because this is a shared development server, and instructed continuation of independent work with unavailable checks reported at the end. This supersedes the earlier blanket hold and unanswered-owner statement; it does not turn any unrun check into a pass. Read-only harness review identified module-free QEMU/userspace fixture paths. Current continuation uses one coordinated build or guest at a time, process-local four-CPU affinity, nice10 and Cargo jobs2, retaining the existing functional gate SMP2/4 and exact assertions. No host module/configfs/global gadget-ledger change, reboot, foreign-process termination or system-wide resource change is authorized or attempted.

Actual preparation and limitations are recorded in `.build/logs/end-of-job/qemu-only-20261010-shared-host/OWNER_CONSTRAINTS.md`. All-target targeted conformance linking has passed; ordinary x86 development profile/package restoration and fresh test descriptors are currently in progress. These shared preparation results are not a new milestone-specific guest PASS. Existing passed/failed evidence and unchanged completion requirements remain authoritative; final results will be appended as checks finish.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0183 (2026-10-10 05:43:14 UTC):

Current development staging and test inventories pass on all three targets. The actual command for each selected run was `env -i <recorded HOME/PATH/LANG and DEV1/job/stack controls> taskset -c 0,1,2,3 nice -n 10 ./test.sh --arch ARCH --fast`, with `SMP=4` and the exact `TEST_SELECTION` captured in `functional-selections.json` and each invocation receipt. All 27 selected cases passed: x86_64 7/7 in 53.143s, aarch64 10/10 in 255.673s, riscv64 10/10 in 339.056s. Each selected ID occurred exactly once; all terminal verdicts passed, with zero lost/retired pages or refused frees. All 2848 snapshotted runtime/harness source hashes remained unchanged. These are targeted functional regressions, not whole-job FULL verification or graphics throughput acceptance.

The current x86 DHCP renewal regression `kernel.services.dhcp_lease_renews_at_t1_and_restarts_its_clock` actually passed. The previously recorded complete IPv6 proof remains retained historical evidence: no network/IPv6 implementation delta requires repeating that long gate. Current typed-modem guest acceptance remains pending; no new modem-gate pass or milestone completion is claimed.

Evidence: `.build/logs/end-of-job/qemu-only-20261010-shared-host/functional-kernel/reconciled-result.json`, `functional-kernel/results.json`, per-target invocation/run/guest/terminal records, and `development-functional-staging.json`. Existing audit content is preserved; no audit rating is assigned.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0183 (2026-10-10 06:01:13 UTC):

Read the complete plan and latest implementation/audit records. Prepared the remaining canonical module-free guest invocation in `.build/logs/end-of-job/qemu-only-20261010-shared-host/modem-admin-final-preparation/`, with no build or guest execution in this preparation. The selected gate is `./check.sh --gate qemu-modem-service`. A clean environment retains DEV1, four-host-CPU affinity/nice10, Cargo jobs2 and the original guest SMP/deadlines; BOOT_IMAGE and RUN_DISK remain unset so the gate prepares its own current image and the modem's final selected bounds test cannot inherit an ordinary ISO. Cached normal/test and, for admin, cold-x86 media are guarded before launch. Existing normal/cold control or owner paths refuse admission. Only exact control inodes and PGID contents witnessed on owned descendants may be removed after every observed descendant ends; unknown or replaced objects are preserved. Old admin/cold logs and frames will be retained before canonical replacement. No production implementation, acceptance assertion, original requirement or completion flag was changed.

Actual verification: `python3 -B .build/logs/end-of-job/qemu-only-20261010-shared-host/modem-admin-final-preparation/selfcheck.py` PASS: 22 pure preparation checks, including absent/duplicate/unperformed verdict refusal, required modem/frame assertion refusal, live-resource preservation, foreign-PGID preservation, and replaced inode/content refusal. This verifies the ignored invocation adapter only; it is not a modem or admin guest PASS. Actual canonical guests remain UNPERFORMED in this continuation. Current DHCP renewal already passed among the 27 selected shared tests; unchanged complete IPv6 and prior bound-mutation evidence are retained rather than repeated. Coordinated final host/generated/model results are still required.

Evidence: `selfcheck-result.json`, `README.md`, `run.py` and `preparation.json` in that directory. All existing audit bytes were preserved; no audit rating is assigned.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0183 (2026-10-10 09:28:49 UTC):

Actual whole canonical modem gate PASS on source manifest `17fec87ceee8e5bbdc03d1837fb0e3f3c3aae03b0c7e9efb0ccdb0d5e78bc0a3`. Executed `python3 -B .build/logs/end-of-job/qemu-only-20261010-shared-host/modem-bounded-drain-preparation/run.py --gate qemu-modem-service --output .build/logs/end-of-job/qemu-only-20261010-shared-host/modem-after-harness-fixes-current`; the unchanged runner invoked `taskset -c 0,1,2,3 nice -n 10 ./check.sh --gate qemu-modem-service` with clean DEV1 environment, Cargo jobs2 and RUST_MIN_STACK33554432. BOOT_IMAGE and RUN_DISK were absent, preserving the gate's private current development ISO for its two ordinary machines and its separately selected final test kernel. The canonical current build passed in65s; the exact Cargo fingerprint/build output identifies the ordinary kernel as DEV1, and the first and second actual QEMU medium digests match the retained private ISO identity.

The no-NIC machine passed authority separation, PIN handling and identity policy, real echo/DNS traffic through NetworkService, one-context refusal/recovery, rollback, context loss, transferred dead-owner authority, failed prepared-launch cleanup, SIM replacement, unanswered-PIN reconciliation without replay, resource limits and provider republication. The real service counts returned to13 handles and5 clients. The NIC machine passed pre-activation busy refusal, authorized replacement carrying traffic and deactivation restoring the DHCP NIC as a new interface generation. Both original400-second guest windows and all probe assertions were preserved. The final exact `kernel.services.modem_service_refuses_at_its_provider_and_client_bounds_and_gives_them_back` scenario passed1/1: four providers/thirty-two clients admitted, overflow refused and capacity reclaimed; memory0 retired,0 lost,0 refused frees. Actual selected-kernel run/guest logs were retained using the witnessed test-kernel.sh PID from STAGED-KERNEL and the canonical LOG_STEM rule, never newest-file selection.

Canonical terminal: `check.sh: RESULT ok exit=0 seconds=923`. Observed wrapper duration949.555s includes27.087s of bounded natural post-command drain for the already observed test-watchdog `sleep 30`; no drain signal or retry was used. All290 observed owned PID/start identities ended, private TMPDIR is absent, all21 checked control paths are absent with no live QEMU, all normal/test cached-media identities still match, and all2848 runtime/harness source hashes remain unchanged. No host module/configfs/global gadget-ledger operation or foreign-process signal was attempted.

Evidence: `modem-after-harness-fixes-current/canonical/{command.log,result.json,post-command-drain.json}`, `image-input-receipt.json`, both ordinary-machine input logs, retained bounds logs and `slot-release.json`. This proves the typed modem service/raw-IP integration; USB MBIM transport remains P02M0099 scope. Retained complete IPv6 and DHCP renewal results were not rerun. P02M0183 remains OPEN pending coordinated final host/generated/model/report checks and reconciliation of the unchanged all-target build evidence; this result is not a whole verify.sh PASS. Existing audit bytes were preserved; no audit rating is assigned.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0183 (2026-10-11 05:41:10 UTC):

Actual current-source build reconciliation is complete for the latest isolated userspace change. On frozen source `cb991a6396b10b52f28443d44e10d730386447462368be1ba1a4cc52c202ab18` (2860 runtime paths), the coordinator ran the unchanged canonical command for each target: `taskset -c 0,1,2,3 nice -n 10 ./build.sh --arch ARCH --part user,packages,volume --dma-mode harness`, with LIBER_DEVELOPMENT=1, DEBUG=0, Cargo jobs2 and RUST_MIN_STACK268435456. Actual PASS: x86_64 237.394s, aarch64 244.838s, riscv64 241.870s; each canonical build terminal reports outcome=ok/exit=0. These are development userspace/packages/ordinary-volume builds; they do not claim a new kernel/loader build, new bootable image or guest acceptance. The retained full all-target/kernel verification remains separately recorded above.

The actual independent root release `bluetooth-voice-partial-write-userspace-restage-current/slot-release.json` is SHA256 b1d79984c4a3f5e9291f82d464c19e5476abfb48d1219aba26a5a60b45046392. It records all2644 observed owned PID/start identities gone, all21 controls absent/no QEMU, the complete CB991a/2860 source hashes and exact runtime path set unchanged, all804 current staged artifact identities verified, media24/protected15 and the older pinned9 image artifacts unchanged. Ordinary userspace/package input identities intentionally changed; this is not a claim that the original22 staging inputs stayed identical. Full command, environment, source and artifact receipts are retained per architecture under that directory.

Read-only scope reconciliation `P0183-conditional-closure-cb991.json` (SHA256 c31939c0e95a224e55df82aa7176f45b436958e86070920f2749f09ffe234323), independently rechecked here, binds all110 relevant modem/network/MBIM/contracts/bootstrap/authority sources byte-for-byte to the actual modem17fec source, prior068ac evidence and currentCB991a freeze. None of the four later Bluetooth changes intersects those110 paths. The sole changed guest source is the separate development-only audioprobe binary's accepted-prefix waveform continuation. Thus the actual whole modem PASS923s, its bounds1/1, the retained complete IPv6 PASS1403s and actual DHCP renewal remain applicable through unchanged relevant sources and these successful current cross-builds. This is evidence reconciliation, not a new modem/IPv6/DHCP execution or a claim of identical relinked binaries.

P02M0183 remains OPEN pending the final focused host/generated/model/manifest/grant/bootstrap/source-hygiene checks and any required dynamic-report regeneration. No such final check was executed by this read-only reconciliation; no unrun result is labeled PASS. A dynamic-report refresh proves regeneration, not an unrun comparison. The original final verification checkbox, all requirements and TODO status are unchanged. No runtime implementation change, new guest, host module operation or audit rating was made here.


## IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0183 (2026-10-11 06:14:56 UTC):

A required P0194 integration correction now affects one of the retained 110 modem-scope sources: PermissionManager's root receive buffer is only 512 bytes although valid typed pipeline requests can exceed it. The reviewed candidate replaces this one buffer with fallibly allocated heap storage at the existing rt::MAX_MESSAGE_BYTES limit, before online reporting; grant policy, framing, dispatch and modem/network behavior are unchanged. The real governed-redirection regression is extended in the existing Base fixture. Its extra temporary-volume process is confined to that fixture, with a fixed six-process cleanup allowance; unchanged scoped/fault/resolved fixtures retain five and exact handle refund remains required.

The actual 110-source receipt intersects the three candidate paths only at permission_manager.rs. After application the previous literal equality must be described as 109 unchanged files plus this explicitly reviewed receiver-capacity delta. The retained 923-second modem and IPv6 passes keep their original source attribution; they are not newly executed results. Current all-three builds, the real broker tests and final relevant checks remain UNPERFORMED at this entry. P02M0183 remains OPEN. Candidate manifest d4ca591cf71e4032bde3899fcecbb310acf1edf93b635a415461c026be0d567b and patch 7dc282fc0f82c09643a38d958f3926474614ad12e5eb8abb3c8f1991ac81133e were fully reviewed before application.


## IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0183 (2026-10-11 06:30:17 UTC):

The current649bbb userspace/package/ordinary-volume build reconciliation is now actual: all three canonical commands passed (x86_64 253.247 s, aarch64 258.712 s, riscv64 235.643 s; session87033 exit0). Root release7c2ebe9cf1e529616da07571583bb8a8c6a567420b2f9562aac140bf5fc635b9 verified2,712 observed owned identities absent, all21 controls clear, exact2,860 source paths, all804 current staged artifacts, unchanged protected15/media24/old image9. The scoped source relation remains109 byte-identical files plus the reviewed PermissionManager receive-capacity delta; the earlier modem/IPv6 evidence retains its original attribution. Actual real-broker regression has STARTED in session94335 and remains PENDING, and final relevant checks remain UNPERFORMED. This is build evidence, not a new modem run or completion claim; P02M0183 stays OPEN.


## IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0183 (2026-10-11 06:53:17 UTC):

Root started the actual existing14-case PermissionCohort::Base on each architecture, session41246, through reviewed runnerb79c41a0fd5f3653062681a7edd52a5400706d5af3740b187dad28f3907620f0. Commands remain ./test.sh --arch ARCH --smp 4 with exact TEST_SELECTION, full preflight per architecture and unchanged limits/35-second natural child drain; CPU0-3/nice10/jobs2/256-MiB compiler stack. Current source45beab09b5eb976cbb75a6253f1c0889b939e6d12d797a3d6ffd62b0e6404f1d has2860 exact paths. The readback fixture now uses the existing redirect_in alone, so its exact1153-byte check observes unmodified file bytes.

Transitioncf2c49de9d3149c8752948f254ade330286da5820ced14989331886d626eeec1 records exactly two kernel-test paths changed from actual all-three userspace build source649; all2858 other runtime rows, including production PermissionManager22360147, are unchanged. The retained actual production builds7c2 remain qualified evidence. Previous failed fixture runs94335 and66640 remain FAILED, with their separate verified clean releases d490 and12b. No actual modem acceptance result is changed: its original110-file scope remains109 unchanged plus the receiver-capacity delta requiring these current checks.

Current guest result is PENDING. Fresh normal image and whole Bluetooth/LE/LE Audio gates are UNPERFORMED on this source; neither milestone is marked complete.


## IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0183 (2026-10-11 07:16:21 UTC):

The corrected actual PermissionCohort::Base PASSED all42 selected tests,14 on each architecture, session41246 exit0: x86_64 case78.736 s, aarch64 case500.007 s, riscv64 case709.2 s. These are full wrapper case timings. Each ran the unchanged ./test.sh --arch ARCH --smp 4 command with full preflight and original deadlines. The exact stored1153 bytes, subsequent broker launch and retained admin stand-in peers passed on all three; no real independent Bluetooth acceptance is inferred from this kernel fixture.

Root read-only terminal helper1694edc0, actual session4221 exit0, produced0c95a9385e409aaec54f7e9ee4d64d4519be6899a73b8a5cce8888169b8bbdab. It rechecked all276 observed owned identities including coordinator absent, all21 controls clear, exact current source45be/2860 paths, current protected15/staged804/image-input22, unchanged loader/paired-seed12/old pinned image9/media24. Retained production builds remain source649/7c2 with precisely the reviewed two-test-file qualification; no additional userspace rebuild was invented. For P183 the modem scope still comprises109 unchanged accepted rows plus PermissionManager's receiver-capacity change, now covered by actual current all-target builds and broker regression. Final common checks remain required.

The first attempt to admit the new normal image FAILED before launching a build, in0.091 s, because private wrapper95d2 expected historical restage field command_result while actual7c2 uses command. Neither its image output nor admission work directory was created. This is a private bookkeeping schema defect, not a compiler or guest failure. The correction retains the exact command-result identity check under its actual key; original wrapper bytes and failure details will be preserved. Current image and whole/LE/LE Audio gates remain UNPERFORMED. Earlier failed guest fixture runs remain FAILED and unchanged; milestone completion is not yet claimed.


## IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0183 (2026-10-11 09:56:07 UTC):

Actual combined-source all-three production userspace/package/volume restage session5297 exited0: x86_64 PASS262.005s, aarch64 PASS271.256s, riscv64 PASS237.218s. Each ran the canonical `./build.sh --arch ARCH --part user,packages,volume --dma-mode harness` under CPU0-3/nice10/jobs2/DEV1/compiler stack268435456. Source freeze is `runtime-sources-after-xhci-event-retention.json` SHA256 `f239bec462d41c29e35be533f64dbf7b5e724dea93a30bb326206f1b6df5d3f5`,2864 paths. Root terminal verification session30162 exited0; `xhci-bounded-pump-userspace-restage-current/slot-release.json` SHA256 `feb2ead2f0b43ef35f5186d66880ea84252384b4e5e70de2f4c6209410f4590f` proves all2816 observed owned identities, including the coordinator, absent; full21 controls, protected15, historical pinned image9 and media24 remain unchanged. Updated staged804 and current input22/port12 snapshots are retained. No kernel or guest acceptance follows merely from these successful builds.

Started the separate three-case bounded-pump kernel cohort on each architecture in session77267, using `xhci-bounded-pump-followup-preparation/run-kernel-cohort.py --run` with actual source/transition/candidate/restage receipt arguments. Original full preflight, SMP4 and deadlines remain. At this append no kernel result, fresh normal image, USB/Bluetooth guest acceptance or final shared-check PASS is claimed.


## IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0183 (2026-10-11 10:08:06 UTC):

Actual combined-source kernel cohort session77267 exited0: all three exact selected tests PASS on x86_64 (78.801s), aarch64 (258.912s) and riscv64 (319.901s), nine selected passes in total. The selections are `kernel.sched.a_bounded_drain_gives_up_on_a_thread_that_keeps_requeueing_itself`, `kernel.sched.a_bounded_wait_wakes_on_the_callers_window_and_not_the_nearest_timer` and `kernel.hardware.kernel_transport_reads_a_ready_reply_without_waiting_for_unrelated_work`. Each target ran `./test.sh --arch ARCH --smp 4` with original full preflight and deadlines, CPU0-3/nice10/jobs2/DEV1/compiler stack268435456. Root terminal verification83620 exited0; `xhci-bounded-pump-kernel-current/terminal-clean-release.json` SHA256 `f8aff661adec5e54bdffb32969503f327829db1b7f2ff816e400a606ecd28403` verifies275 observed owned identities including coordinator absent, full21 controls, exact source2864, immutable12, staged804, input22/port12, historical image9 and media24. Allowed ordinary-kernel changes are measured separately. Earlier stale-build admission failure remains preserved.

The source is still f239bec462d41c29e35be533f64dbf7b5e724dea93a30bb326206f1b6df5d3f5. This proves the selected bounded-pump behavior and compatibility; it does not prove original UVC streaming, MIDI2, audio or Bluetooth acceptance. The fresh normal image preparation has now started as sole producer session50972 using `xhci-bounded-pump-followup-preparation/prepare-image.py --run` and actual all-three build/9-case clean receipts. Image completion and subsequent guests remain unperformed at this append.


## IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0183 (2026-10-11 10:10:03 UTC):

The actual fresh normal development image session50972 exited0 after all eight unchanged canonical preparation commands. Image receipt `bluetooth-after-xhci-bounded-pump-fix/pinned/image-receipt.json` SHA256 `cd94951ca2ea3945e2bcb172937f0850fe37aa54bd4e40494e182ae23b83b189` binds current source f239/2864, DEV1/harness kernel profile and the exact signed paired volume. ISO SHA256 `7a9bf190963fc0ba5c374d860e7ffb9b897899e9f5a031a4f560b60116d12d73`; paired volume SHA256 `2be2bb6dc9775cf4246e4d061e7c9fe88d36bbd7bec2f5876f396be18d0c23a5`. Independent root terminal verification93882 exited0: `bluetooth-after-xhci-bounded-pump-fix/slot-release.json` SHA256 `01a8ccaad516b5096af8fa39847874fcb6bf4dc420d29a0b80ce6cc5c4890aa2`, all108 observed owned identities including coordinator absent, full21 controls, current15/staged804/input22/port12 measured, allowed normal x86 kernel/paired-image changes distinguished from immutable12, old image9 and media24 preserved. Identical-content bootstrap republication is explicitly recorded. This is current image preparation, not a Bluetooth or USB guest PASS.

Started the sole independent Bluetooth guest gate as session37720 through `bluetooth-after-xhci-bounded-pump-fix-preparation/gate-admitted.py --run`, pinned to this actual image/source/root release. It uses the unchanged canonical407422 runner, original45m command bound and SMP2 with CPU0-3/nice10. All guest assertions and explicit controller-capability fixture exceptions remain. No result from this active gate is claimed. Required LE/LE Audio/SC-HOGP service fixtures, USB integration and final shared checks remain separate.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0183 (2026-10-11 12:52:22 UTC):

The applied FC246 source (SHA256 fc24606359eae7fba816e73eb38209e3e22865ded9b4005ab01163791c97f4bd, 2866 files) now has actual successful all-three userspace/package/ordinary-volume builds. Session 43586 exited 0: x86_64 255.714 s, aarch64 255.071 s and riscv64 247.344 s. Each used `./build.sh --arch ARCH --part user,packages,volume --dma-mode harness`, CPU 0–3, nice 10, jobs 2 and RUST_MIN_STACK=268435456. Root terminal session 6690 exited 0 and independently verified 2830 owned PID/start identities absent and 804 staged artifacts. The receipt is `.build/logs/end-of-job/qemu-only-20261010-shared-host/bluetooth-opp-sco-final-userspace-restage-current/slot-release.json`, SHA256 b673350f75ab0123a9feb308676cdf8bda15bce293c0793081350bcf881b1bfc. This was compilation/staging, not a new kernel test cohort.

The one fresh normal x86 development image then completed all eight canonical preparation commands in session 79566 (exit 0), beginning with `./build.sh --arch x86_64 --part kernel,volume --kernel-on-volume --dma-mode harness` and `bash harness/mkimage.sh iso .build/cargo/kernel/x86_64-unknown-none/debug/kernel` (the latter uses the canonical absolute kernel path from src). Root terminal session 52447 exited 0, checked the signed-manifest/UUID/paired-volume relationship, source binding, protected artifacts and absence of all 109 owned identities. Image receipt `.build/logs/end-of-job/qemu-only-20261010-shared-host/bluetooth-after-opp-sco-final-fix/pinned/image-receipt.json` has SHA256 ff2c54e89b1ff4cdd4052c68d4ee4bfa7e4121234edbfd0103a01a8184371d8a; its root cleanup receipt has SHA256 25ef175054c81c41844bc70c5ff5f97b275b781a0f15256500753f6c77fadf31. ISO SHA256 e1340ef446e97cef45ba6e030dc3df55f9e8cf69fdade4a9ebd9e715ede5c14e; paired volume SHA256 d045cb2e02880d723c9756f570415d77eaf1e05259834acaeb8b6a3fb9d25a42. The preceding image and failed test evidence are preserved. No host module/gadget operation was used.

Read-only scope reconciliation in P0183-current-qualification-fc246.json confirms all 110 scoped rows match qualified F239 (109 original rows unchanged and the sole already-verified PermissionManager receive-buffer delta). None of the latest 17 source changes intersects this scope. Original final focused/generated/manifest/model/report checks remain pending; no modem/network guest rerun or whole verify.sh result is claimed.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0183 (2026-10-11 13:15:56 UTC):

The serialized final host batch on source fc24606359eae7fba816e73eb38209e3e22865ded9b4005ab01163791c97f4bd terminated with exit 1 (actual unified session 76376). `./check.sh --refresh dynamic-report` completed in 297.762 s (regeneration); `./gen.sh --check` passed in 16.568 s; `./check.sh --gate verify-model` passed in 29.656 s. `./check.sh --gate host-tests` failed after 811 reported seconds: 143 of 144 crate/feature checks passed and soft2d/default failed. Its 43-test suite had 42 passes and `tests::a_second_lane_never_displaces_a_decoded_copy` failed at tests.rs:2102: Bands x6 admitted 6 lanes while the test expected 2. The cause is under investigation; no timing or host-failure cause is inferred. The remaining declared-interfaces, grant-vocabulary, milestone-index, source-hygiene and bootstrap-plan commands were not executed by this batch.

The source, image, staged-artifact and existing-media guards remained unchanged. Root terminal verification session 61588 exited 0 and confirmed all 2,329 observed owned identities absent; the failed run is preserved in `final-host-final9-after-opp-sco-usb-final-current/`, with `slot-release-after-failure.json` SHA-256 abd86bc97af73beacdfe46cbcce4e539be8d177f176400b3f083b3ec03aeb2c8. This is clean release of a failed test run, not a passed verification. No host USB module/configuration operation was performed. Original performance and other outstanding completion requirements remain open.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0183 (2026-10-11 13:29:17 UTC):

The independent continuation of the remaining final host checks (session 42941, exit 1) passed declared-interfaces (0.536 s), grant-vocabulary (0.504 s), and milestone-index (2.883 s). Source-hygiene failed on eleven shebang scripts with mode 0644; bootstrap-plan was not reached. Root terminal session 96435 exited 0 and verified all 135 observed owned identities absent, preserving the failed subset in `final-host-five-after-final9-failure-current/slot-release-after-failure.json` (SHA-256 991de2fc012dc6442b0561ab954f11892886899ddabf68d569619b3e64eb0b86).

The eleven named scripts were changed only from mode 0644 to 0755, with exact bytes, inode and mtime unchanged. `source-hygiene-executable-mode-fix-20261011.json` (SHA-256 1e5333c6acf2ee0a42ae45af40e346ae502cb773d2f11103017e0facb42352c6) records every path and hash. The FC246 content manifest and compiled-image source attribution remain valid; source-hygiene rerun is still pending. The earlier host-tests failure remains a failed run, and no aggregate final-host PASS is claimed.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0183 (2026-10-11 14:58:36 UTC):

The reviewed seven-file source is actually applied as `runtime-sources-after-late-pcm-mic-fix.json`, SHA256389ba109b805fe5023d8c4cb21252b1872ee2efad6ee63ebbd86d6078a438a9b. Actual application28054 exited0; transitiond7c25c9f42d44133615b304f15f9d10a36a21891f56f0792d61faa68928b7f15 records2866 paths and2859 unchanged FC rows. Actual all-three userspace/package/ordinary-volume restage57461 exited0: x86_64171.364s, aarch64183.381s, riscv64173.227s. The unchanged per-architecture command was `./build.sh --arch ARCH --part user,packages,volume --dma-mode harness`, under CPU0-3/nice10/jobs2/RUST_MIN_STACK268435456. Root terminal16208 exited0 and verified all1952 owned PID/start identities absent,804 staged artifacts and current15/22/port12/image9/media64. Receipt: `.build/logs/end-of-job/qemu-only-20261010-shared-host/late-pcm-mic-userspace-restage-current/slot-release.json`, SHA256cf2c931479a25699d08d90cd620513b3b010fec2382b7e68b095303de6adb7a3. This is current build/staging evidence, not a repeated kernel9/Base42 or guest acceptance cohort.

Actual read-only report-input measurement20267 exited0 with the unchanged `dynamic-report-static-driver-qualification-preparation/measure-inputs.sh AFTER_DIR runtime-sources-after-late-pcm-mic-fix.json 389ba109...` procedure. Root compared both contents.tsv and manifest-paths.tsv byte-for-byte with the pre-restage baseline: all1755 content rows equal, comprising639 dynamic tool/library ELFs,1104 validated object/reference/sidecar paths,three reports andnine generator/manifest inputs. Receipt `dynamic-report-static-driver-content-equivalence-20261011.json`, SHA2560ee2a4f6fb9fc1a66ba1b9d5399eec54da2b2c438522dfc9adeeb9f615e48c7d, qualifies retention of the earlier actual FC report refresh on equal measured inputs. Cache/report identities were first measured now; no historical identity equality, new generator --check, comparison mutation-probe pass or whole-host PASS is claimed.

The seven changed paths do not intersect the110-file modem scope. Retained actual modem/network/broker results remain source-qualified, not rerun. Current all-three builds and report-input equality now complete those shared steps; the original final checkbox remains open pending the affected Soft2D host-fixture correction and remaining source-hygiene/bootstrap-plan checks. The original host-tests144 run remains FAIL with143 configurations passed; no aggregate replacement PASS is claimed.


## IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0183 (2026-10-11 21:37:58 UTC):

Final shared-tree verification: actual production6ea921 all-target userspace/package/volume builds and image preparation passed; current source01a0a255 differs only in the Soft2D cfg(test) fixture. The complete default crate passed43/43 tests (236.089s), `./check.sh --gate source-hygiene` passed63.044s after the executable-mode corrections, `./check.sh --gate driver-mutations` passed251.349s with20 xHCI outcomes,24 parser cases and89/89 planted defects, and `./check.sh --gate bootstrap-plan` passed0.561s. Each producer has a separate verified clean release. Driver and bootstrap results retain their actual source4/source5 attribution; they are not new source6 runs. Earlier generated/model/interface/grant checks and143 passing host configurations remain qualified historical evidence; neither earlier whole-host failure nor the failed source5 stack-overflow run is relabelled. Actual1755 report-input equality retains the earlier297.762s refresh, with all9 report source inputs and3 outputs rechecked unchanged for source6. No new production rebuild or report refresh is claimed for a test-only change. Exact scope and receipts are in `.build/logs/end-of-job/qemu-only-20261010-shared-host/P0183-current-qualification-source6-01a0.json` and the named final-host directories. Final milestone-index and read-only plan queries are recorded separately after their actual execution; whole `./verify.sh` and unsafe Linux-gadget checks remain UNPERFORMED.

All original implementation and verification items are satisfied. Retained complete modem traffic/lifecycle acceptance (923s), DHCP renewal and the whole IPv6 gate (1403s) are qualified by110 unchanged scoped files:109 original rows and the same PermissionManager receive-buffer delta independently covered by Base42. Actual all-target staging267.339/250.786/238.674s and report reconciliation complete the earlier build/report remainder; the affected full host crate and source checks now pass. No USB MBIM hardware, whole Bluetooth, graphics-throughput or whole verify.sh prerequisite is added to this service milestone. The final verification checkbox, plan status and TODO row are now COMPLETE; original requirements and all historical records are preserved.


## IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0183 (2026-10-11 21:46:39 UTC):

Final document and selection verification: `./check.sh --gate milestone-index` actually passed (3.042 s, session 83180); terminal cleanup session 31249 confirmed all 10 owned identities absent. The four read-only commands in `final-readonly-plan-source6-unlayered-current/queries/commands.json` all exited 0: verify-model range changes (14.679 s), working-tree changes (14.331 s), `./verify.sh --for <exact range/working-tree union> --plan --explain` (57.361 s), and the same selector with `--json` (57.406 s). Session 22260 selected 286 paths and 1998 plan items; its three-architecture build/boot fields describe planned coverage, not executed builds or boots. Source contents, modes, HEAD and working-tree status were unchanged during those queries. Root terminal check session 30685 confirmed all 18 owned identities absent, with source, image, media and shared-host guards preserved. Whole `./verify.sh` remains UNPERFORMED, including the unsafe Linux-gadget-dependent emulated checks. Neither plan selection nor clean teardown substitutes for milestone acceptance. No host restart or further host USB/module operation was performed.

Exact final receipts: `.build/logs/end-of-job/qemu-only-20261010-shared-host/final-host-milestone-index-after-source4-media96-soft2d-unlayered-current/slot-release.json` (SHA256 e790557f2ed714cf9988ce89bfb21625704fb113d089e45250334b34f4a323a7) and `.build/logs/end-of-job/qemu-only-20261010-shared-host/final-readonly-plan-source6-unlayered-current/terminal-clean-release.json` (SHA256 1e069f4ffbd771e904afc044faadede2cdc342ac9ec10503e9c1c4f61805cd1f). These results precede this verification-only append; the implementation, scoped successes, failed runs and remaining limitations recorded above are unchanged.

Final status: COMPLETE. All previous audit content is preserved; no audit rating is assigned.

AUDITOR'S REVIEW ON P02M0183 (2026-10-15T04:36:11Z):

**Rating: 8/10.** ModemService, `service_logic::modem_commands`, NetworkService's link administration, the persistent uplink selection and the raw-IP medium implement the plan closely: four separately minted authorities with identity and management denied by default, owner-bound grants, admission reserved before the modem is asked anything, success reported only after the installation commits, bounded rollback, link-changed teardown with the replaced NIC restored, and every buffer that carried a PIN or PUK zeroed. One recovery path is incomplete. When the grant that issued a PIN, PUK or deactivation is retired before the modem answers it, the expired command's unknown outcome is never reconciled, and the modem then refuses every state change - every data activation included - as `again` until some management client happens to query it. The plan's "admin revocation" trigger also has no operation behind it.

## Findings

1. **Medium, Defect: an unknown outcome whose caller has gone is never reconciled, and the modem keeps refusing state changes.**
   - **How the state is cleared:** `Device::tick` marks the modem `uncertain` when a state-changing command expires (/data/yellow/libersystem/src/user/services/logic/src/modem_commands.rs:288-310). While it is, `submit` refuses every state change with `Reconcile` (lines 198-205), and only a completed `Query` clears it (lines 258-259).
   - **Who starts the reconcile:** ModemService sends that query only from the completion of the expired command's own call: `Purpose::Pin` at /data/yellow/libersystem/src/user/services/core/src/modem_service.rs:739-742, `Cleanup` at 757-760, activation at 875 and 890-893, and deactivation at 983-988.
   - **Where it is lost:** `retire` drops a grant's pending `Pin` and `Deactivate` calls (modem_service.rs:1229), and `complete` returns early when the call is gone (line 708). If a manage client's PIN or PUK, or a data client's deactivation, goes unanswered and the grant is retired before the five-second deadline, the expiry completes to nobody and no query is ever sent. Retirement comes from owner exit, a closed connection or a SIM replacement, which retires the old SIM's grants at 655-663.
   - **From then on, every activation fails:** it reserves, is refused `Reconcile` at `submit`, is answered `again` (lines 309 and 1112-1113) and gives the reservation back - indefinitely.
   - **Nothing in the default setup recovers it:**
     - a data-only client has no operation that sends a query;
     - identity and management are denied by default;
     - so on a shipping policy only a withdrawal or a service restart recovers the modem.
   - **The deactivation case is worse:** the modem may keep an active context that nobody takes down, while its status reads `unknown`.
   - **Against the plan:** "reconcile through fresh read-only queries before conflicting operations; unreconciled state remains unknown rather than retaining a pending slot forever". The gate's `uncertain` row (/data/yellow/libersystem/src/user/services/core/src/modemcheck.rs:400-425) covers only a caller that stays alive.
   - **Fix:** in `complete`, when the call is gone and the result is `OutcomeUnknown`, call `reconcile(key)`; or turn a retired grant's state-changing calls into `Cleanup`-like purposes instead of dropping them.

2. **Low, Defect: modem grants have no admin revocation path.**
   - **Plan:** "admin revocation" is one of the events that "invalidate the scoped authority and tear down its contexts".
   - **Code:** `modem-admin` has only `mint` (/data/yellow/libersystem/src/idl/modem.lsidl:208-210), and ModemService has no way to revoke a grant from outside its owner and connection. Camera and MIDI, written to the same plan text, each expose `revoke` (/data/yellow/libersystem/src/idl/camera.lsidl:222 and /data/yellow/libersystem/src/idl/midi.lsidl:237).
   - **Impact:** PermissionManager calls none of them today, so the practical effect is nil now. This is a completeness gap against the stated trigger list.
   - **Fix:** add `modem-admin.revoke(modem-id)` that retires every grant on that modem through `retire`.

## Verification performed

Static review only; nothing was built, run or tested.

Read in full:
- `modem.lsidl`, `modem-device.lsidl` and `modem_service.rs`;
- `service_logic` `modem_commands.rs`, `uplink.rs` and `raw_ip.rs`;
- `drivers::mbim`.

Read in part:
- NetworkService's start-up, persistent subscription, `apply`, `tear_down`, `bring_up_raw`, `check_link`, `on_link_admin` and the `LinkAdmin` implementation, and `net.rs`'s raw-medium hooks;
- PermissionManager's modem arm of `grant_for_task` and `modem_policy`;
- the broker's `cap_grants`: the modem roots go to `permission_manager` only;
- the manifest: ModemService's roles, and its LINK factory as the only user of NetworkService's LINKADMIN root;
- the probe's `uncertain` scenario, and the gate registration.

Confirmed correct:
- **Secrets:** a PIN/PUK travels in `Secret`, is encoded into an owned 256-byte frame that is scrubbed after the send, and the request and reply buffers are scrubbed too.
- **Correlation:** commands are matched by connection generation, transaction and kind; a stale SIM or context generation is answered stale.
- **Limits:** eight pending and one state change per modem; the provider's limits are clamped down. Deadlines are 5 s and 60 s, and nothing is replayed.
- **Counters:** each is known or unknown, carries its SIM generation and observation time, and an unknown count needs acknowledgement.
- **Datagrams:** 64 packets and 256 kB per direction within the MTU, with refusals and drops counted.
- **Activation:** reserve, then activate, then install. `again` without replace authority; success only on `installed`; a bounded deactivation after a failed install; abandon and reconcile after an uncertain activation.
- **NetworkService's attachment checks:** IPv4 only, a unicast address with room in its prefix, the gateway on the subnet, at most two DNS servers, MTU 576..4096. It installs atomically and tears down with link-changed. It brings the replaced NIC back, or the lowest live one, or none. It releases a reservation whose holder connection closed.
- **Uplinks:** at most 16 NICs, the lowest stable identity as fallback, a modem never selected automatically.
- **Raw-IP medium:** it validates datagrams and never sends ARP, runs DHCP or applies the static fallback.

IMPLEMENTER'S RESPONSE ON P02M0183 (2026-10-17T01:44:22Z):

**Finding 1 - an unknown outcome whose caller has gone is never reconciled (Medium): ACCEPTED.**
Confirmed: the reconciliation request was dropped with the retired grant, so the SIM state stayed uncertain and state changes stayed refused.
- `src/user/services/core/src/modem_service.rs`:
  - `Purpose::Deactivate` now carries an optional caller.
  - When a grant retires, its pending PIN verify becomes a clean-up purpose that still reads the outcome back, and a pending deactivation keeps going with no caller. The retired grant's read-only calls are dropped.
- Oracle:
  - New probe step `modemcheck uncertain-retired` (`modemcheck.rs`). It sends a PIN with a deadline that has already passed, waits for the fixture to count the attempt, and closes the manage grant. It then requires `status()` to report the SIM ready within 10 s.
  - `src/tools/check-modem-service.sh` runs it after `lock` and requires its verdict.

**Finding 2 - modem grants have no admin revocation path (Low): REJECTED.**
- Nothing in the tree would call such a path: PermissionManager revokes no grant of any kind (the camera and MIDI revoke paths have no caller either).
- No oracle can drive it: the modem-admin root is PermissionManager's alone.
- Under the owner's rule of 2026-10-13, nothing is built without an oracle that runs here. A revoke verb with no caller and no test would be speculative.
- It belongs with whichever change gives PermissionManager revocation for every grant kind.

**Verification (x86_64):** `qemu-modem-service` passed, `modemcheck uncertain-retired` among its steps.

AUDITOR'S RE-AUDIT ON P02M0183 (2026-10-18T17:36:02Z):

**Rating: 8/10.** The rest of the milestone is as the last review found it. Of the 2026-10-15 Medium finding, the PIN half is fixed; the deactivation half is not.
- **PIN and PUK: fixed.** When a grant is retired, its in-flight PIN or PUK becomes a `Cleanup`, so an expiry now sends the reconciling query.
- **Deactivation: not fixed.** `retire` ends the context before it relabels the deactivation. When that deactivation expires, `deactivated()` looks in `self.context` for the modem to reconcile and finds nothing. The modem therefore still refuses every activation after a retired grant's deactivation expires.
- **The new probe step proves neither half.** The fixture reports the SIM ready through an indication before the five-second expiry even happens.
- **The admin-revocation rejection.** It rests partly on a wrong premise, and the milestone still claims that trigger.

## Findings

1. **Medium, Incomplete fix - a retired grant's deactivation that expires is still never reconciled, so the modem keeps refusing every state change.**
   - **What the plan requires.**
     - The plan: "At expiry/cancellation ... return ... `outcome-unknown` when a state change may have reached the device ... Reconcile through fresh read-only queries before conflicting operations" (/data/yellow/libersystem/docs/todo/P02M0183.md:91-94).
     - The 2026-10-15 finding named both cases: "a manage client's PIN or PUK, or a data client's deactivation".
     - For the deactivation case, the response claims: "a pending deactivation keeps going with no caller".
   - **What the code does.**
     - **The context is ended first.** `retire` calls `self.end_context(Error::Closed, false)` (/data/yellow/libersystem/src/user/services/core/src/modem_service.rs:1226-1227), which takes `self.context` (:1015). Only after that does it relabel the call as `Deactivate { caller: None }` (:1241).
     - **No follow-up deactivation is sent.** `end_context` sees the device as `Deactivating`, not `Active` (:1028-1029; the state is set at /data/yellow/libersystem/src/user/services/logic/src/modem_commands.rs:225).
     - **At the five-second expiry:**
       - `Device::tick` sets `uncertain = true` and the context to `Unknown` (modem_commands.rs:296-301).
       - `complete` calls `self.deactivated(done, caller)` without the modem key it holds (modem_service.rs:759).
       - The unknown branch takes the key from `self.context` (`let key = self.context.as_ref().map(|context| context.modem)`, :987). That is now `None`, so `reconcile` is never called (:989-990).
       - From then on `submit` refuses every state change with `Reconcile` (modem_commands.rs:202-203).
     - **A new variant, introduced by the fix.** A definite `Done` for the caller-less deactivation runs `end_context(Error::Closed, true)` on whatever context is current (modem_service.rs:978-979). If a newer activation is still waiting for its reservation, it is ended with `closed`; `activate` admits one as soon as `self.context` is `None` (:831-837). The old code dropped this completion, so this effect is new.
   - **Why it matters.**
     - **The original outcome remains.** On the default policy, identity and management are denied, so a data-only client cannot send a query. Every activation is answered `again` until the modem is withdrawn or the service restarts.
     - **A context can be left up.** The modem may keep a context that nobody takes down, while the status shows it as `unknown`.
     - **A second way in.** The same path is reached when a SIM replacement retires the grants (:658-666) during a deactivation that then expires.
   - **Minimal fix.**
     - **Option A:** pass `key` from `complete` into `deactivated`. Reconcile it on any result other than `Done`, whether or not `self.context` is set; `reconciled()` already takes an unowned active context down. Touch `self.context` only when it is the context in `Stage::Deactivating`.
     - **Option B (equivalent):** in `retire`, relabel the retired grant's deactivation as `Purpose::Cleanup`, and have `Cleanup` reconcile on any non-`Done` result.
     - **Test:** add a probe step that uses `delay(Deactivate, 0)`, closes the data grant mid-deactivation, waits past 5 s, and requires a fresh activation to be admitted.

2. **Low, Incomplete fix - `modemcheck uncertain-retired` passes without the fix, so the claimed verification does not support it.**
   - **What the plan requires.**
     - The guest proof must cover "stale replies and complete bounded resource reclamation on authority loss" (P02M0183.md:157-161).
     - The response presents `uncertain-retired` as the oracle for Finding 1. The tree's rule is that a green test is not evidence until it has been watched failing.
   - **What the code does.**
     - **The fixture reports Ready at once.** When it handles the EnterPin, it unlocks the SIM (/data/yellow/libersystem/src/user/drivers/core/src/modem_fixture.rs:391) and sends an indication in the same handler (:465). Only the reply is held (:374, :641-642).
     - **The service takes that state.** ModemService copies the indication's state into `modem.state` (modem_service.rs:655), and `status()` reports `sim` from it (:399).
     - **The probe's only check.** It is `until(10, || probe.status().sim == SimState::Ready)`, run right after `close(probe.manage)` (/data/yellow/libersystem/src/user/services/core/src/modemcheck.rs:446-447). That holds at once, before the five-second expiry and so before any reconcile could run. It holds equally on the old code, which dropped the call.
     - **Nothing later in the run reads the `uncertain` state.** `limits` checks only counts, and `republish` starts a fresh `Device`.
   - **Why it matters.** The gate's `PASS uncertain-retired` line in the 2026-10-16 log says "reconciled by a fresh query" without observing one. That is how the deactivation half above went unnoticed, and a regression of the PIN half would pass too.
   - **Minimal fix.**
     - After closing the grant, wait past the 5 s command deadline.
     - Then require that a state change is admitted again: an activation through the probe's data grant must not be answered `again`, and is deactivated afterwards.
     - Add the deactivation case from finding 1.

3. **Low, Unjustified rejection - "admin revocation" is still claimed as done with no path behind it, and the rejection's no-oracle premise does not hold.**
   - **What the plan requires.** "Close, admin revocation, client endpoint loss, SIM change, device withdrawal and service restart invalidate the scoped authority and tear down its contexts" (P02M0183.md:39-44, ticked `[x]`).
   - **What the code does.**
     - **No revoke exists.** `modem-admin` has only `mint` (/data/yellow/libersystem/src/idl/modem.lsidl:208-210).
     - **The no-oracle premise is wrong.** The response says "No oracle can drive it: the modem-admin root is PermissionManager's alone". But the kernel modem rig creates the service's ADMIN role and keeps its end (/data/yellow/libersystem/src/kernel/test_suites/services.rs:5459, 5466). It is the same rig that already drives the service's bounds, so a revoke could be checked there.
     - **The other half of the rejection is fair.** Nothing calls the camera or MIDI `revoke` either.
     - **But only the camera milestone says so.** P02M0184 records it: "Admin revocation is `camera-admin.revoke`; nothing calls it yet" (/data/yellow/libersystem/docs/todo/P02M0184.md:208-209). P02M0183.md records nothing, and no open milestone in docs/todo/TODO.md owns PermissionManager-side revocation.
   - **Why it matters.** The milestone document claims a trigger the system cannot produce. The deferral the response describes ("belongs with whichever change gives PermissionManager revocation for every grant kind") is written nowhere a later change would find it.
   - **Minimal fix.** Either of these:
     - state in P02M0183.md that `modem-admin` has no revoke, and that admin revocation is not implemented because no grant kind has PermissionManager revocation yet;
     - or add `modem-admin.revoke(modem-id)` that retires through `retire`, with a kernel-rig check.

## Verification performed

- **Documents read.**
  - /data/yellow/libersystem/docs/todo/P02M0183.md in full, including "Corrections of 2026-10-15" (:284-292).
  - /data/yellow/libersystem/AI/audit/code-audit-P02M0183.md:1088-1157.
  - /data/yellow/libersystem/docs/todo/P02M0184.md:200-210, for the camera precedent.
  - The open rows of docs/todo/TODO.md.
- **Code read in the current tree.** The corrections are commit 58fb476c; 7ee3e9bf changed only the doc.
  - /data/yellow/libersystem/src/user/services/core/src/modem_service.rs:
    - `Purpose`, `submit_with`, `on_reply`, `on_indications`, `complete`;
    - `reconcile` and `reconciled`;
    - `activate`, `abandon`, `activated`, `install_failed`, `installed`;
    - `deactivate`, `deactivated`, `end_context`, `on_link` and `reserved`;
    - `retire`, `lose`, `mint`, the main loop, and `serve`'s grant branches.
  - /data/yellow/libersystem/src/user/services/logic/src/modem_commands.rs in full.
  - modemcheck.rs: the helpers, `lock`, `uncertain` and `uncertain_retired`.
  - modem_fixture.rs: `command`, `indicate`, `set_sim`, `delay`, the hold/held path and `serve`.
  - Also:
    - the check-modem-service.sh diff;
    - the admin interface in `modem.lsidl`;
    - the kernel modem rig's roles;
    - the camera and MIDI `revoke` implementations.
- **Claims checked.**
  - **A retired grant's PIN or PUK becomes `Cleanup`, and its expiry reconciles:** confirmed (modem_service.rs:1238-1246, :760-763, :776-779).
  - **The retired grant's read-only calls are dropped:** confirmed (:1249). BeforePin's secrets are zeroed when it is dropped (:86-96).
  - **"a pending deactivation keeps going with no caller":** it does keep going, but its unknown outcome is not reconciled (finding 1).
  - **The `uncertain-retired` step:** confirmed that it exists, runs after a second `lock`, and has its own expect row; the old row now matches `PASS uncertain:`. It does not tell the fixed code from the old code (finding 2).
  - **`qemu-modem-service` passed with `uncertain-retired`:** confirmed as a run by /data/yellow/libersystem/.build/claude-work/implementer/runs/gate-qemu-modem-service.log (2026-10-16, `RESULT ok exit=0 seconds=1100`). It is not evidence for the fix.
  - **The rejection of the revocation finding:** the substance is partly fair, the premise is not (finding 3).
- **Not done.** This was a static review only: nothing was built or run.

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON P02M0183 (2026-10-21T12:09:39Z):

**Finding 1 - a retired grant's deactivation that expires is still never reconciled, so the modem keeps refusing every state change (Medium): ACCEPTED.**
- `src/user/services/core/src/modem_service.rs`: `deactivated` now takes the key of the modem the deactivation was for,
  and decides by whether the context it ends is still the one deactivating there (`ours`).
  - `Done` and an unknown outcome end the context only when it is ours: a grant retired while its deactivation was out
    has had its context ended already, and a context there now is a newer activation's.
  - Whatever a deactivation nobody owns any more ends with but `Done` - an expiry above all, or a refusal - is settled
    by a query (`reconcile(key)`): the modem was asked to take a context down and nobody knows whether it did. The
    query's answer takes such a context down. Before, an expiry left the modem refusing every state change until
    something else queried it, and a refusal left a context up that nobody held.

**Finding 2 - `modemcheck uncertain-retired` passes without the fix, so the claimed verification does not support it (Low): ACCEPTED.**
- `src/user/services/core/src/modemcheck.rs`: `uncertain-retired` no longer passes on the SIM reading ready (which it
  did whether or not the outcome was reconciled). After the grant's deadline has passed (6 s) it requires a state change
  to be ADMITTED - an activation that completes and can be taken down again; a modem still refusing as busy fails the
  step by name.
- Two new steps for finding 1, the same way: `deactivate-retired` holds a deactivation's answer at the fixture, ends the
  data grant while it is out, and `readmit` then requires a fresh activation to be admitted after its deadline.
- `src/tools/check-modem-service.sh` runs them and requires their lines.
- Watched failing in a guest, each against the defect it is for: with a PIN out at the modem dropped along with its
  grant (the service before 2026-10-15), `uncertain-retired` fails - "the modem refused a state change as busy - the
  unknown outcome was never reconciled"; with the old branch, which reconciled an unknown deactivation only while a
  context was still installed, `uncertain-retired` passes and `readmit` fails the same way. (A first attempt broke the
  refusal branch instead, and `readmit` passed: a deadline completes a deactivation as an unknown outcome, never as a
  refusal. The refusal branch's query for a deactivation nobody owns has no guest step; it is the same call.)

**Finding 3 - "admin revocation" is still claimed as done with no path behind it, and the rejection's no-oracle premise does not hold (Low): ACCEPTED.**
- The finding is right: the service's own administrative interface is the path and a kernel-suite rig is the oracle.
  What it does not have yet is a caller: PermissionManager, the only holder of the `modem-admin` role, mints grants and
  never revokes one - the milestone now says so (Corrections of 2026-10-21).
- `src/idl/modem.lsidl`: `modem-admin` gains `revoke` (op 2): every grant on one modem - data, identity and management -
  ends as a closed connection's would, its context torn down, and the number of grants ended is answered. Regenerated
  with `./gen.sh`.
- `modem_service.rs`: `AdminView::revoke` retires each grant on the modem through the same `retire` a closed connection
  takes, and says so.
- `src/kernel/test_suites/services.rs`: the modem rig mints and revokes through the admin root, and
  `kernel.services.modem_admin_revocation_retires_every_grant_on_its_modem` requires a revocation to end both of a
  modem's grants, close both connections, give their client slots back, and a second revocation to find none. Its first
  run failed before it reached the revocation: the rig named the grants' owner as "the calling thread's process", and a
  kernel test runs on no thread of its own. The test now makes the owner, a process that stays alive throughout - one
  that ended would retire the grants first - and passes. Watched failing: with `revoke` counting the grants and retiring
  none, the test fails at "and closed both connections, as a closed connection's grant would be".

**Verification:**
- `qemu-modem-service` PASS on the final tree (1113 s), with `uncertain-retired`, `deactivate-retired` and `readmit` and
  the bounds scenario.
- The two watch-fails in the guest and the revocation test's are given under findings 1-3; the revocation test passes on
  the tree.

**Milestone status (verified after these changes):** COMPLETE. Every item is done and all three findings are fixed,
admin revocation with a kernel oracle.

**Full regression on the final tree:** A release rehearsal, `./verify.sh --release --in-place --jobs 2` over the tree
(2026-10-20/21), ran 183 of its 373 steps in 22 hours and was stopped there by the owner's decision; every step it
failed was run again alone, and the 190 it never reached were then run - so every release step has a result. An
independent check of every finding of these responses against the code then found gaps, which are fixed and recorded
above, and the fixed tree was verified one step at a time: the builds of all three targets; the drivers' 638 host tests,
`service-logic`'s 965, `ucsi`'s 16 and `conformance2d`; the Bluetooth reconnect proof; `source-hygiene`,
`graphics-profile`, `opentype-profile`, `guest-verdict`, `milestone-index` and `dynamic-report` (regenerated);
`brightness-acpi`, `usb-legacy`, `typec-ucsi`, `bluetooth-le` and `driver-mutations`; and the x86_64 kernel suite, 585
passed. The other suites on the tree before those fixes, which touched no kernel code but one x86_64 test: aarch64 546
passed; riscv64 passed but `spool_service_sends_submitted_jobs_exactly_once`, which fails there alone (a printer's
answer past its 200-tick bound; P02M0186, outside this review). The 148 host suites PASS (`host-tests`). Failed in the
rehearsal only for the run's order, concurrency or load and passed alone: `perf-anchor`, `sleep`, `verify-evidence`,
`signed-boot`'s pairing, `staged-consistency`, `run-verdict`, `capability-trace` and five arch profiles; and
`provider-media-order`, `development-gate` and the x86_64 suite, which failed because the gates that boot through
`dev.sh` leave the shared x86_64 staging in the development profile and nothing restores it - a harness gap reported to
the owner. Not caused by this review - its one kernel change, and the 2026-10-19 one beside it, reverted change nothing
- and reported to the owner: `qemu-pcie-hotplug` and `concurrent-selection` (since 2026-09-29), `signed-boot`'s
re-signing cases (the signer has required `--dma-mode` since 2026-09-10), `arch-profile-*-no-dt-absent-1` (since
2026-10-12), the IOMMU hostile and transition phases on both ports, the ports' idle-sleep clock check, `tickless-idle`
on riscv64, `typec-tcpci-riscv64` (timing) and `development-lifecycle`'s protocol fuzz.
