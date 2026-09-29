IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0195 (2026-09-27T22:41:08Z):

Status: IN PROGRESS (P02M0195a first, in the owner's agreed order; P02M0195b follows P02M0196b). This record is updated as the work proceeds; the final state is at its end.

### P02M0195a - what was implemented (2026-09-28)

THE TWO CONTRACTS (IDL, generated with `./gen.sh`; 35 packages, no drift):
- `src/idl/i2c-device.lsidl` - `liber:i2c-device@1`: `i2c-functionality` (plain, max-transfer, quick, byte,
  byte-data, word-data, block-write, block-read, i2c-block-read, pec), `i2c-status` (ok, no-device,
  interrupted, too-long, controller, unsupported, pec), `i2c-reply`, and the interface ops 1-15 (functionality,
  address, write, read, write-read, quick, the SMBus transactions). NO address argument anywhere: the
  connection is the address.
- `src/idl/gpio-device.lsidl` - `liber:gpio-device@1`: `gpio-trigger` (none/rising/falling/both/high/low),
  `gpio-line`, `gpio-event {level, sequence}`, interface line / level / events (stream) / acknowledge.
- Crates `src/user/libs/protocol/{i2c-device-proto,gpio-device-proto}`, aggregate `src/proto` re-exports,
  manifest `[[sources]]`/`[[libraries]]` rows, `gen.sh` PACKAGES/EXTERNAL/AGGREGATE_EXTERNAL.

THE CLIENT SIDE (new crate `src/user/libs/driver/i2c-client`, source row in the manifest, host suite
`host.i2c-client`, 6 tests): `ScopedBus<T: Transport>` implements `hid_i2c::I2cBus` UNCHANGED over the
contract's generated client (refuses at bind a controller without plain I2C; refuses an address other than the
connection's own with `BusError::NoDevice` BEFORE anything is sent; bounds by the controller's declared
maximum; maps `i2c-status` onto `BusError`); `Smbus<T>` is the second client type (the one SSIF will use):
the consumer states the transactions it needs at `new` and is refused there when the controller does not
declare one; a transaction it did not state is not sent. DECISION: the plan puts the wrapper "in the HID
driver"; the HID driver is P02M0195b, so the wrapper is a library the HID driver (and SSIF, P02M0201) will link
statically, which keeps `hid-i2c` free of IPC exactly as the plan asks and lets the kernel oracle drive the
real driver through it. `hid-i2c` itself is untouched (its manifest/module comments are corrected by
P02M0195b, as the plan assigns).

SCOPED CONNECTIONS (earlier in this session, recorded here for completeness): provider kinds `i2c-bus` = 22,
`gpio-lines` = 23 in `device.lsidl`, `driver_protocol::provider`, the system manifest's kind names and
DeviceManager's mappings; `driver_protocol::Scope {Whole, I2cAddress(u8), GpioLine{line, trigger}}`,
`encode_connect` / `decode_connect_scoped` (an unscoped CONNECT is the two-byte payload it always was, so the
protocol version is unchanged); `drivers::common::Serving` keeps each endpoint's scope (`scope_at`);
`driver_binding::{scoped_only, keeps_offered_endpoint, openable, admits_scoped}` (pure, host-tested);
DeviceManager: `publish_all` closes a bus provider's offered endpoint and does not count it, the catalogue's
`open` refuses a bus, `mint_scoped_connection` refuses a scope that does not fit the kind and counts scoped
mints against `consumers`; `system-manifest` refuses a role or permission row naming either kind.

THE DRIVERS:
- `src/user/drivers/core/src/virtio_i2c.rs` (new binary `virtio_i2c`): features ZERO_LENGTH_REQUEST and
  INDIRECT_DESC; one request queue; three DMA pages (control: out headers, statuses, two indirect tables;
  write; read). The offered endpoint's near end is closed after `online`; the publication is registered with
  no endpoint (`Serving::from_offers(&[(0, 0)])`). A connection is admitted only with `Scope::I2cAddress(a)`
  for an address no other connection holds; anything else is refused on the connection itself (closed +
  `DISCONNECT`). Transfers go through `i2c::plain` / `smbus::compose` + `Queue::submit_chains` (both requests
  of a write-then-read published together, through indirect tables when negotiated); every non-OK status is
  `interrupted`; PEC in software (`Composed::finish`); the block read with the device's count answers
  `unsupported`; reads/writes past 2048 bytes `too-long`.
- `src/user/drivers/core/src/virtio_gpio.rs` (new binary `virtio_gpio`): FEATURE_IRQ; `ngpio` and
  `gpio_names_size` from config space; line names read once with GET_NAMES; request queue polled one request
  at a time; the event queue interrupt-driven on the device's own MSI-X vector (DeviceManager's `use_msix`
  allowlist gains `virtio_gpio`), one two-descriptor buffer per line at descriptors 2L..2L+1. A connection
  is admitted only with `Scope::GpioLine{line, trigger}` for a line no other connection holds (and, for a
  trigger, a line the event page/queue can serve); `Level` scope sets input and reads; an interrupt scope sets
  input, SET_IRQ_TYPE, then queues the buffer. Events: taken off the queue on the interrupt, the level read with
  GET_VALUE as the event is taken, delivered ONCE on the connection's stream (`events` refused - no handle -
  on a level connection), `acknowledge` refused unless an event is outstanding, and it re-queues the buffer.
  A connection that goes gives its line back (SET_IRQ_TYPE none, SET_DIRECTION none, then the event queue is
  drained so the disarmed buffer is back before the line can be taken again).
- A defect found in my own earlier `gpio.rs` while writing the oracle and FIXED: `Lines::take` queued the event
  buffer BEFORE SET_IRQ_TYPE. A device returns a buffer queued for a line whose interrupt type is still none at
  once, as invalid, and the line then has no buffer to report on. The order is now direction, trigger, buffer;
  the host test that pinned the old order is renamed and pins the new one
  (`an_interrupt_line_is_armed_with_its_trigger_before_its_buffer`).
- `Cargo.toml` `[[bin]]` rows; manifest `[[programs]]` rows `virtio_i2c` (virtio-type 34, `dma =
  "trusted-untranslated"`, `provides = [{ kind = "i2c-bus", most = 1, consumers = 8 }]`, heartbeat 100) and
  `virtio_gpio` (virtio-type 41, `gpio-lines`, most 1, consumers 16). `abi`: `VIRTIO_TYPE_I2C = 34`,
  `VIRTIO_TYPE_GPIO = 41` and their device-type names.

THE FIXTURE (`src/harness/vhost-i2c-gpio.py`, executable): one process serving both vhost-user sockets and a
control socket (raise/lower/level/pec/status); VIRTIO_F_ACCESS_PLATFORM with REPLY_ACK + BACKEND_REQ, the IOTLB
(updates and invalidations acknowledged, misses asked on the backend channel while the main channel is served),
indirect descriptor tables; the register device at 0x50 (registers; BLOCK register 0xF0 answering a device
count; MODE register 0xFE: no PEC / PEC / wrong PEC). DECISIONS made while building the oracle:
- THE CONTROL MAILBOX (register 0xE0 at 0x50): a write of 0xE0 + a command's text runs that command through
  the SAME `command()` the control socket runs, and a read answers the reply. The plan says the kernel test
  raises lines "through the control socket"; a test inside the guest cannot reach a Unix socket on the host,
  so the socket's commands are carried in-band through the device model the plan already gives ("the lines it
  can raise"). The socket itself serves gates and other fixtures (P02M0196d) unchanged. REPORTED as a
  deviation in mechanism, not in what is proved: the line is still moved by the device model outside the
  guest, and the driver still sees only its own event queue.
- The write half of a write-then-read carries no PEC (SMBus puts the PEC at the end of the read), and the
  read's PEC covers the write only when the two are one transfer (FAIL_NEXT) - the model had treated every
  write as PEC-checked and every read after any write as combined.
- Self-test: 8 tests (memory table, IOTLB updates/misses/invalidations, translated-space retry, indirect ring
  reads, register device + PEC incl. the combined write, I2C FAIL_NEXT, GPIO held buffers, the mailbox).

THE HARNESS: `qemu-run.sh` `qemu_attach_i2c_fixture` - with `I2C_FIXTURE` set, `vhost-user-i2c-pci` at
00:15.0 and `vhost-user-gpio-pci` at 00:16.0 (below the 0x17..0x1e the development fixtures' `edu` functions
hold), guest RAM on `memory-backend-memfd,share=on` for that run only, `iommu_platform=on` exactly when the
machine has the virtio-iommu (x86_64 from its `iommu` decision; the ports from `port_iommu_decide`), never
`disable-legacy`; on the ports the devices go on the boot commands and not into the device-tree dump.
`test-kernel.sh`: `I2C_FIXTURE=bus|hid` is compile-time (option_env) and runtime; the backend is started in a
run-private `liber-i2c.*` directory, waited for by its ready file, and reaped by the exit trap with the
directory; a fixture run publishes no whole-suite evidence (like a gadget run).

THE ORACLES (`src/kernel/test_suites/hardware.rs`, tag `i2c` - new `TestTag::I2c`; `send_scoped_connect` in
`tests.rs`; kernel deps `i2c-device-proto`, `gpio-device-proto`, `i2c-client`, `hid-i2c`):
- `kernel.hardware.virtio_i2c_serves_one_address_per_connection_and_every_transaction_it_declares`: bind
  (offered endpoint dropped unread), the declared functionality, plain write/read/write-read through
  `ScopedBus`, 0x51 refused on the 0x50 connection before sending (the register it names unchanged), the bound
  (client and controller), the block read with the device's count `unsupported` and an SSIF-shaped consumer
  refused at bind, every declared SMBus transaction without PEC, a write without PEC refused by a checking
  device, every transaction with PEC right, reads with PEC wrong failing as `pec`, an unoccupied address
  failing (`interrupted`), a second 0x50 connection / an unscoped one / a line-scoped one refused with a
  DISCONNECT each, 0x50 served again after the first disconnects (its departure reported), and the killed
  controller's connections closed.
- `kernel.hardware.virtio_gpio_delivers_each_event_once_and_holds_the_line_until_it_is_acknowledged`: both
  controllers bound (lines moved through the mailbox), the line names, a level connection (levels, no stream,
  nothing to acknowledge), rising/falling/both/high/low each on its own line delivered once at the level it was
  taken, silent while masked, acknowledged once; the two level lines still asserted delivered again after the
  acknowledgement, the edge lines not until they move; a second connection for a held line (interrupt and
  level), an unscoped one, an address-scoped one and a line the controller lacks refused with a DISCONNECT
  each; the killed controller's connections closed.
- `bind_bus_controller` takes the ELF bytes; each test does its own `lookup(b"drivers/...lsexe")`, which is
  what the verification model's reachability scan reads.

THE GATES AND THE MODEL: `i2c-bus` (`src/tools/check-i2c-bus.sh`: the two oracles with `IOMMU=1 I2C_FIXTURE=bus`,
asserting the gate row + enforcing-required from the runner and the kernel, both oracles' own claim lines, no
NOT RUN, no failure) and `i2c-backend` (the backend's self-test on the host), both in `check.sh`, the catalog's
GATES (141 -> 142 entries) and `GATES_THAT_BOOT_A_GUEST` (i2c-bus; 46 -> 47), release-required
`gate.i2c-bus`, `gate.i2c-backend`, `host.i2c-client`, `host.i2c-device-proto`, `host.gpio-device-proto`.

DEVICEMANAGER DEVELOPMENT-BOOT SELF-TEST `device_manager::tests::scoped_bus_publication` (called beside
`unopened_provider_withdrawal`): an `i2c-bus` and a `gpio-lines` provider through the real `publish_all` with
real channels; the far end of each offered endpoint is closed at publication, each entry holds no handle and
counts no consumer, and the catalogue's `open` refuses both.

ADJACENT FIXES REQUIRED BY THE CHANGE:
- `src/tools/check-declared-interfaces.py`: the wire-constant pattern `[A-Z_]+` read `I2C_BUS` (a digit) as
  absent; now `[A-Z][A-Z0-9_]*`.
- `src/tools/check-driver-connections.py`: `Serving::accept` takes the scope; a fifth regression
  (`a_scoped_connection_is_served_under_the_scope_its_connect_named`) and a fourth mutation (`lost connection
  scope`).
- `src/tools/component-oracle-exceptions.txt`: `gamepad_fixture` (from P02M0192 - see that record) named with
  its gate oracle.
- `src/user/drivers/core/src/smbus/tests.rs`: a published read-word transaction with its PEC (MLX90614's
  datasheet example: B4 07 B5 D2 3A -> PEC 0x30) through `compose` and `finish`. NOTE: the plan asks for "the
  SMBus specification's own examples"; I had no copy of the specification offline, so the suite carries the
  CRC catalogue's CRC-8/SMBUS check value (0xF4 over "123456789") and this published device example - not the
  specification's own - and says so here.

### P02M0195a - verification (2026-09-28)

PASSED (commands run from the repository root):
- `./build.sh --arch x86_64` (840 s) and later `./build.sh --arch x86_64 --part user,packages,volume` after each
  driver change - ok.
- `I2C_FIXTURE=bus TEST_SELECTION=kernel.hardware.virtio_i2c_serves_one_address_per_connection_and_every_transaction_it_declares,kernel.hardware.virtio_gpio_delivers_each_event_once_and_holds_the_line_until_it_is_acknowledged ./test.sh --arch x86_64`
  - 2 passed, three consecutive runs, then once more after the mutation checks below were reverted.
- `./check.sh --gate i2c-bus` (the same two oracles with `IOMMU=1`: the runner's "run mode gate, DMA mode
  enforcing-required" line, the kernel's adoption of it, both oracles' claim lines, no NOT RUN, no failure) - PASS.
- WATCHED FAILING (mutation checks, reverted afterwards): the virtio-i2c driver changed to admit a second
  connection for a held address -> the I2C oracle fails "a second connection for a held address is refused";
  the virtio-gpio driver changed to give an event buffer back at delivery -> the GPIO oracle fails "line 3 is
  silent until its event is acknowledged".
- Host suites: `cargo test --manifest-path src/user/drivers/core/Cargo.toml --lib` (395), `src/user/libs/driver/
  i2c-client` (6), `src/user/libs/protocol/{i2c-device-proto,gpio-device-proto}` (3 + 3), `src/user/libs/driver/
  binding` (86), `src/user/libs/driver/protocol` (76), `src/tools/system-manifest` (25);
  `python3 src/harness/vhost-i2c-gpio.py --self-test` (10); `python3 src/tools/check-driver-connections.py` (5
  regressions, 4 mutations rejected; it also runs `check-provider-catalogue.py` - 8 regressions, 6 mutations -
  and `check-audio-provider-recovery.py`).
- Gates: `i2c-backend`, `declared-interfaces` (23 provider kinds agree), `no-fixed-provider-slots`,
  `driver-protocol-note` (16 notes on x86_64's volume), `source-hygiene`, `verify-model` (consistent) - ok;
  `./gen.sh --check` - no drift; the test kernel (`TEST=1 I2C_FIXTURE=bus cargo build --tests`) and the
  production kernel build without warnings.

FAILED / PRE-EXISTING: `./check.sh --gate component-oracles` fails with 14 staged drivers and services that have
neither an oracle nor a stated reason (admin_fixture, bt_fixture, camera_fixture, midi_fixture, modem_fixture,
power_fixture, smartcard_fixture and seven services) - none of them this milestone's; `virtio_i2c` and
`virtio_gpio` are covered by the two oracles. (`gamepad_fixture`, which my P02M0192 work added to that list,
now has its line in `component-oracle-exceptions.txt`.)

NOT PERFORMED (the job's end, per the owner's rule that slow architectures run last):
- the two oracles on aarch64 and riscv64 - which is also the first run of those arms of `qemu_attach_i2c_fixture`
  (devices on the boot commands, not in the tree's dump; `iommu_platform=on` from `port_iommu_decide`);
- a development boot showing `DeviceManager: a bus provider's offered endpoint was closed at publication and
  counted nothing` (the `dev.gpu-restart` check now requires the line).

DEFECTS FOUND BY THE ORACLES AND FIXED (each in this milestone's own new code):
1. `gpio::Lines::take` queued the event buffer BEFORE SET_IRQ_TYPE (found while writing the oracle; the device
   returns such a buffer invalid).
2. The backend's `put_used` wrote the used index with `struct.pack_into`, which zero-fills before writing a
   byte at a time; the drivers - correctly - refused a completion when the index they polled read 0 (seen in
   the driver's own report: "old_used 20 ... seen 0"). Indexes now move with one 16-bit access
   (`memoryview.cast("H")`), for the available index's reads too.
3. The backend drained a ring inside the handling of SET_VRING_ENABLE; with the IOMMU the first buffer access
   misses, and QEMU answers a miss only after it has the reply it waits on - a deadlock (QEMU aborted). Drains a
   message asks for now run after the reply, and a miss serves every device's main channel while it waits.
4. QEMU sends no IOTLB invalidation for mappings a driver's domain loses after the device's vhost side is
   stopped, so the second binding of a controller read the first binding's freed pages through stale entries.
   The backend empties its IOTLB when a ring stops.
5. The drivers' synchronous waits were bounded by 10,000,000 spins - tens of milliseconds in an unoptimised
   driver, which a device on the host can exceed - and are bounded in time now (`virtio::SUBMIT_BUDGET_TICKS`,
   one second, in `Queue::submit_chains`, which both drivers use).
6. `BusController::kill` first used `Process::terminate`, which does not wake a thread parked in a wait - the
   production kill is SIG_KILL, which does; the oracle now kills through `syscall::deliver_signal` (made
   `pub(crate)` for it), the path DeviceManager's kill takes.
Also added while diagnosing, and kept: each driver says why it refused a connection and which request its
device did not answer; the backend takes `--trace` (one timestamped line per message, request and IOTLB entry),
and `test-kernel.sh` keeps its log beside the guest's as `<run>-i2c-backend.log`.

## P02M0195b - started (2026-09-29T02:02:54Z)

P02M0196b is in (the namespace's devices with their connections joined to the controllers' rows, node channels,
`_DSM`), so P02M0195b begins in the agreed order. The plan's assumptions, checked against the code before writing:
- DeviceManager has NO child binding: a requirement is a provider KIND satisfied by any publication (`requirements_met`),
  the dependency closure and the depth order work on kinds, DeviceManager never reads a row's `connections`, and no
  `RESOURCE` kind carries a connection - scoped mints existed only for the ACPI service. All of it is this step's.
- The generic HID parser (`drivers::hid`) records each field's application collection, so routing a report by its
  collection needs only accessors; it had NO usage constants for Mouse, Pointer, Touch Screen or Touch Pad.
- InputService takes the first live `pointer` and `touch` provider at bootstrap and closes both subscriptions; the
  `gamepad` subscription it keeps open is the pattern to follow. Its services tests answer the four subscriptions in
  the order input, pointer, touch, gamepad, which is kept.
- The emitter has `I2cSerialBusV2`, `GpioInt` and `_DSM`; the backend names line 0 `hid-touchpad` and line 1
  `hid-touchscreen` already; `fdt` reads a `hid-over-i2c` node under a `virtio,device22` child as an I2C device with its
  address and a `GpioLine` interrupt (checked by an extended fdt test before anything was changed there).
- The sleep exchange (SUSPEND/RESUME) does not exist yet: P02M0197 lands after this half, so by the plan's own rule
  ("carried by whichever of P02M0197 and this half lands second") that item is P02M0197's to carry.

## P02M0195b - what was implemented (2026-09-29)

THE CHILD BINDING (DeviceManager, `src/user/services/core/src/device_manager.rs`):
- `Need` - one connection's requirement: the controller's FUNCTION (a PCI function or a platform number, generation
  0, so a controller rebound under a new generation satisfies it again) and the provider kind that serves it (`i2c-bus`
  for an I2C address, `gpio-lines` for a GPIO line). `needs_of` reads them off a platform row's `connections` at
  `Node::new`; `controller_function` turns the controller's row index into its binding identity.
- `needs_met` beside `requirements_met`: `gate_on_requirements` parks a child in `DependencyPending` until both
  controllers publish ("waiting for the controllers its firmware connections are on"); `settle_dependencies` wakes it;
  `stop_nodes_that_lost_a_dependency` dooms a child whose controller serves its kind no more or is itself being
  stopped (`StopIntent::DependencyLost`, "connected through a controller whose binding ended"); `dependency_depths`
  puts a child one level above each controller, so dependents stop first. No restart budget is spent - the existing
  dependency path.
- THE MINT AT BIND: `begin_bind` takes `children: Option<(&mut Catalogue, &[Publisher])>`; for a platform row with
  connections each is minted through `child_connection` - `driver_protocol::connection_scope` (below), the controller's
  row, the `_AEI` exclusion (a GPIO line the controller's firmware node lists in `aei()` is refused: "a line the ACPI
  service holds for the firmware's own _AEI events"), the provider slot (`Catalogue::serving`), the live publisher -
  and handed over as a `Connection` resource in the row's order. `mint_scoped_connection` is split into a lookup and
  `mint_on(catalogue, &Publisher, slot, scope)`, so a bind that holds only its own node mints on a snapshot of the live
  controllers (`publishers_for`, empty and allocation-free for a node with no connections) taken by
  `start_candidate_at`. `start_candidate`, `admit_arrival` and `serve_bus_events` take the catalogue mutably; the
  boot phase's binds (boot-critical drivers only) pass `None`.
- A refused connection fails the bind under the new cause `connection-refused` (not retryable: a ten-bit address, an
  SPI device, a line held for `_AEI`, a connection joined to no controller); a controller that will not take one more
  connection is `resource-exhausted`. The IDL enum gained `connection-refused = 13` (pre-release, `gen.sh
  --accept-breaking`), with the binding library's name and retry column, DeviceManager's text and wire mapping and the
  system graph's name.
- THE PROTOCOL: `ResourceKind::Connection = 12`, `MAX_CONNECTIONS`, `MAX_BIND_RESOURCES` grown by it; the drivers'
  `common::Resources` keeps `connections` in the row's order. `connection_scope(source, &abi::Connection)`: an ACPI
  `GpioInt`'s mode and polarity (level low -> `Low`, level high -> `High`, edge low -> `Falling`, edge high -> `Rising`,
  neither -> `Level` for level reads) and a tree specifier's interrupt-type cell (1/2/3/4/8 -> rising/falling/both/high/
  low; anything else refused) into ONE line trigger; an I2C address with `CONNECTION_I2C_TEN_BIT` or past seven bits
  refused; SPI refused. The ABI gained `CONNECTION_I2C_TEN_BIT`, and the ACPI model's `describe` now carries a ten-bit
  address as one instead of dropping the flag.

THE DRIVER (`src/user/drivers/core/src/i2c_hid_driver.rs`, bin `i2c_hid`; pure parts `src/i2c_hid.rs`):
- The connections by kind; the descriptor register from the node's `_DSM` (UUID 3CDFF6F7-4267-4555-AD05-B30A3D8938DE,
  revision 1, function 1, through the node channel it asks DeviceManager for) or the tree's `hid-descr-addr`
  (`tree_descriptor_register` over the property block its claim's window reads); `_PS0` where the node has it;
  `hid_i2c::Device::probe` over `i2c_client::ScopedBus` (refused, and the binding failed with nothing published, on a
  malformed descriptor); READY there, because the bind window is 2 s and the reset bound 5 s; SET_POWER on, RESET, the
  reset indication awaited on the line within `RESET_BOUND_TICKS`, RESET once more, then `DeviceNotResponding`
  (`ResetHandshake`); the report descriptor to `drivers::hid`; `publications` - a Mouse or Pointer application
  collection publishes `pointer`, a Touch Screen `touch`, a Touch Pad or anything else nothing - each offered late with
  a `Serving` publication; no Input Mode or Device Mode report is ever sent. The loop: on the line's event, input
  reports read until the line is quiet (its level against the scoped trigger's asserted level), each routed by the
  application collection its report id belongs to (`route`) - a pointer frame (`Pointer::feed`) or contact frames
  (`contact_frames`) to that publication's consumers, non-blocking - then the event acknowledged; an event with nothing
  behind it resets the device once (`Storm`), the next fails the binding. STOP: SET_POWER sleep, `_PS3`.
- `common::wait_or_answer_until` (new): the combined wait with a deadline and, optionally, the provider set.
- `drivers::hid`: `Layout::application_of` and `Layout::applications`. AND A DEFECT FOUND BY THIS DRIVER'S TOUCHSCREEN:
  `Layout::contacts` began a contact at each Contact Identifier, so a descriptor declaring the tip switch BEFORE the
  identifier - the common order - gave each finger's tip to the finger before it. A contact now begins where its
  finger's logical collection begins (`Segment::logical`, the innermost logical collection, numbered as opened); a
  flat descriptor still begins one at each identifier. A regression test in the parser's own suite.
- `hid-i2c`'s module comment and the manifest's `hid-i2c` and `i2c-client` source-row comments now name the consumer
  and say SSIF uses the contract's SMBus transactions, not `I2cBus`. Manifest row `i2c_hid`: `transport = "platform"`,
  `PNP0C50` and `ACPI0C50` as `_HID` and `_CID`, the `hid-over-i2c` compatible, `dma = "none"`, `pointer` and `touch`
  at most one each. The system-manifest test's DMA table names it.

INPUTSERVICE (`input_service.rs`): `Followed` - the `pointer` and `touch` subscriptions KEPT OPEN (two of the
catalogue's subscriber places), every live provider attached up to its kind's bound (four pointers merged into the one
cursor, one touch surface), one past the bound waiting (said once) and attached when one of its kind detaches; a
provider detached on its withdrawal or when its connection closes and not opened again until published again; a
detached surface's contacts lifted (`Input::lift_contacts`), as focus loss lifts them; each attach and detach said.
The `input` kind keeps its bootstrap-only discovery. The subscription order (input, pointer, touch, gamepad) is
unchanged, so the existing services tests answer it as before. Manifest comment corrected.

THE FIXTURES:
- `vhost-i2c-gpio.py --hid`: `HidDevice`, the HID-over-I2C register protocol (descriptor, report descriptor, command and
  data registers, the input register a plain read empties), its line level and active low (asserted while a report,
  the reset indication or a hold waits), RESET, SET_POWER (asleep: nothing reported), SET_REPORT of the touchpad's Input
  Mode, a power loss (nothing until a RESET), a held line, a malformed descriptor; the precision touchpad at 0x2C
  (descriptor register 0x20; mouse, touch pad and configuration collections; sixteen moves and a click through the ONE
  collection its mode selects) and the touchscreen at 0x10 (register 0x01; two fingers down and the lift); control
  commands `hid script|hold|lose|status NAME` and `hid malformed NAME on|off`; lines 0 and 1 idle high. A host test.
- `acpi-fixture.py --hid-out`: the HID SSDT - below the virtio-i2c function's `SA8_`, `TPAD`/`TSCR` with vendor `_HID`,
  `_CID` `PNP0C50`, `I2cSerialBusV2` (0x2C/0x10), `GpioInt` (level, active low) on `SB0_` lines 0/1, `_DSM` answering
  0x20/0x01. `aml_emitter.py`: `GpioInt`'s consumer bit set, and the host tests now check `I2cSerialBusV2`,
  `GpioInt`/`GpioIo` and a `_DSM` returning an integer field by field against the specification's offsets (the
  committed sample regenerated; the interpreter's suite passes on it).
- `fdt_edit.py hid-fixture`: the virtio-i2c function's node with its `virtio,device22` `i2c` child (seven-bit
  addresses), the two `hid-over-i2c` nodes (`reg`, `hid-descr-addr`, `interrupts-extended` level-low on the virtio-gpio
  function's `gpio` child). The firmware fixture's GPIO node corrected to Linux's binding: PCI id `pci1af4,1069`
  (virtio 0x29 is 0x1040 + 41) and a child named `gpio`. The fdt crate's PCI-child test extended to the I2C child.
- `qemu-run.sh`: `qemu_attach_hid_table` (x86_64: the SSDT built by the run and loaded with `-acpitable`) and
  `i2c_hid_dtb_args` (aarch64/riscv64 through UEFI: the dumped tree edited and handed back with `-dtb`; refused beside
  `DMA_DTB_NODE`).
- `hidcheck` (development probe; grants Device, DevicePolicy, Input, Display): `watch`, `storm`, `cycle`, `malformed`,
  each cueing the gate first. Gate `i2c-hid` (`tools/check-i2c-hid.sh`, registered in `check.sh`, the verification
  model's catalogue and the release-required list).
- Kernel services test `kernel.services.pointer_and_touch_providers_are_followed_as_they_are_published_and_withdrawn`.

NOT IN THIS STEP, BY THE PLAN'S OWN RULE: ACROSS A SLEEP (the suspend and resume exchange and the gate's sleep half) is
carried by whichever of P02M0197 and this half lands second - P02M0197 does.

## P02M0195b - found and fixed on the way (2026-09-29)

- THE WALK ORDER. The first boot with the HID SSDT published `TPAD` and `TSCR` before the ACPI service had reported
  `SB0_` (the GPIO controller's companion - it comes after `SA8_` in the namespace), so the kernel left each device's
  GPIO connection unjoined ("names a controller acpi:\_SB_.PCI0.SB0_ that no row carries") and DeviceManager refused the
  bind by name ("connection 1 is refused: a connection joined to no controller" - the child-binding path doing its job).
  The service's walk now reports reservations, then COMPANIONS, then everything else (`acpi_service.rs`, `publish`).
- READY BEFORE THE NODE. The driver first asked for its node channel before READY, and DeviceManager answers node
  requests only for an online binding, with a 2 s bind window: it now reports READY right after the handshake and
  publishes nothing until its descriptor, RESET and report descriptor are through - so a malformed descriptor still
  fails the binding with nothing published.
- DeviceManager's new lines are printed WHOLE (`say_line`, and `say_acpi` built on it): printed in pieces, they landed
  inside other programs' lines.
- THE TABLET. The gate's "the tablet still moves the cursor" failed while every other case passed. Isolated on a
  development instance: a probe polled 150 times in 30 s and was served the same ring throughout, a separate client's
  one-shot view (`hidcheck ring`) showed the same, and QMP `input-send-event` with ABSOLUTE axes moved the tablet at once
  (the ring grew from 1 event to 7). The monitor's `mouse_move` queues RELATIVE motion, which QEMU routes to the PS/2
  mouse - nothing here drives it - and never to a tablet; its `mouse_button` does reach the tablet, which is why a
  button-only check had looked like movement. The gate drives the tablet through QMP. THE BLUETOOTH GATE'S "unrelated
  pointer" helper uses the monitor's `mouse_move` too, so its "moved" can only have come from the click reports' own
  positions - reported to the owner, not changed here.
- The test's column helper rounded down (column 11 mapped into cell 10); it rounds up now.

## P02M0195b - verification (2026-09-29)

COMMANDS AND RESULTS (PASSED unless said):
- Gate `i2c-hid` (`./check.sh --gate i2c-hid`, the development ISO from `LIBER_DEVELOPMENT=1 ./image.sh --format iso`):
  PASS, 601 s - both devices bound as children (`acpi:\_SB_.PCI0.SA8_.TPAD`/`TSCR`), their descriptors read at 0x20 and
  0x01, the touchpad publishing `pointer` alone and the touchscreen `touch` alone, InputService attaching both beside the
  tablet; `hidcheck watch` (the moves and click in order, the two-finger contact and lift), `storm` (each driver's one
  reset on a held line, then delivering), `cycle` (virtio-i2c disabled: both bindings stopped as lost dependencies, their
  publications - platform devices 36 and 37 - detached with the tablet still attached and moving the cursor; enabled:
  both bound again, re-publications attached, delivering), `malformed` (the touchscreen's binding failed, "its HID
  descriptor at register 0x1 is refused", nothing published). Logs: `.build/logs/i2c-hid/`.
- Kernel, x86_64: `TEST_SELECTION=<the five InputService services tests> ./test.sh --arch x86_64 --timeout 1800` - 5
  passed, `kernel.services.pointer_and_touch_providers_are_followed_as_they_are_published_and_withdrawn` among them.
- Host suites: drivers 415 (the `i2c_hid` module's 5 tests and the parser's new contact test), fdt 121, driver protocol
  78, driver binding 89, hid-i2c 13, i2c-client 6, system-manifest 28, acpi-model 7, aml 45.
- Gates: `source-hygiene`, `arch-surface`, `test-tags`, `i2c-backend` (the backend's suite, the HID model's test
  included), `firmware-fixtures` (`fdt_edit.py` and `acpi-fixture.py` self-tests), `aml-emitter` PASS; `./gen.sh
  --check` exit 0 after `connection-refused`.
- The development boot of the gate showed DeviceManager's `scoped_bus_publication` self-test passing ("a bus provider's
  offered endpoint was closed at publication and counted nothing"), the run P02M0195a's host-suite item waited for.
- NOT RUN: aarch64 and riscv64 (the tree fixture's dump and edit, the gate through the tree, every build), by the
  standing order, at the end of the job; the sleep half (P02M0197's). `verify-model`'s own suite cannot load the model
  in this tree ("kernel test `the_global_clock_advances_once_per_period_however_many_cores_tick` ... has no
  `tagged_test!` declaration") - the pre-existing failure recorded before this step; the catalogue and release list
  were updated by hand in step with `check.sh`.
- After the walk-order change in the ACPI service: gate `acpi` PASS again (481 s), every case as before.
