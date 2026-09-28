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
