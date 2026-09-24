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
