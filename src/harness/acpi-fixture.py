#!/usr/bin/env python3
# THE ACPI GATE'S FIXTURE SSDT, built with the harness's AML emitter and handed to QEMU with `-acpitable`.
#
# THE MECHANISMS, as the plan decides them, for this gate and for the owners' gates that add their devices here:
#
#   THE REGION. An `ivshmem-plain` function at slot 0x14 over a `memory-backend-file` the harness reads and writes.
#   Its companion node - `\_SB.PCI0.SA0`, which QEMU's DSDT declares, opened with `Scope` - reads BAR2 through a `PCI_Config` region and declares its `SystemMemory`
#   region over the HARNESS'S PAGES alone (the first 0x2000 bytes) - authorised by the firmware-held rule, the function
#   becoming the service's. A device an owner adds keeps its `_CRS` range OUTSIDE those pages and declares its own
#   region over that range: `LSFX0001`'s is at 0x4000.
#   THE NOTIFICATIONS. `_AEI` on the `vhost-user-gpio-pci` function's line 2 (the backend's `acpi-aei` line), through
#   the GPIO controller's companion node `\_SB.PCI0.SB0`, whose `_E02` notifies `LSFX0001` with 0x80; on its line 3,
#   whose `_E03` notifies the battery, the adapter and the thermal zone with 0x80 - the power classes' status change;
#   and on its line 4, whose `_E04` is the UCSI PPM's notification (below).
#   THE FIELDS. A `GeneralPurposeIo` field over line 5, for input reads only, and a `GenericSerialBus` field over the
#   bus fixture's register device at 0x50 on the `vhost-user-i2c-pci` function's companion `\_SB.PCI0.SA8`.
#
# THE DEVICES THIS GATE CHECKS WITH, each matched only by the development image:
#   LSFX0001 (`\_SB.LSF1`) - the method and `Notify` source: `RDVL` answers the harness's dword; `WRVL` writes its own
#            region inside its own `_CRS` range, which its driver reads through the claim; `OTHR` reaches `LSFX0003`'s
#            region over that claimed range, which the kernel refuses; `GPRD` and `GPWR` read and write the line field
#            (the write refused by name); `GSBW` and `GSBR` write and read the register; `_DSD` and `_DSM`.
#   LSFX0003 (`\_SB.LSF3`) - another node, declaring a region over LSFX0001's range.
#   LSFX0002 (`\_SB.LSF4`, `_UID` 0, and `\_SB.LSF5`, `_UID` 1) - no `_CRS`; each `_STA` reads a byte of the
#            harness's pages, for a restart's reconciliation.
#
# THE POWER CLASSES, matched by the production image's `acpi_power` rows as a laptop's firmware would be:
#   PNP0C0A  (`\_SB.BAT0`) - a control-method battery: `_STA` from the pages, a static revision 1 `_BIX` in milliwatt
#            hours (50000 design, 48000 last full, 11100 mV, warning 5000, low 2000), `_BST` from the pages.
#   ACPI0003 (`\_SB.ADP0`) - an AC adapter: `_PSR` from the pages.
#   THERMALZONE (`\_TZ.TZ00`) - `_TMP` from the pages; `_CRT` 368.2 K, `_HOT` 363.2 K, `_PSV` 353.2 K, `_AC0` 343.2 K.
#
# THE UCSI DEVICE (`\_SB.UCSI`, `USBC000` with `_CID` `PNP0CA0`), present only when the memory was written with
# `--ucsi-version` - the Type-C gate's, with `ucsi-ppm.py` as the platform's policy manager behind its `_DSM`, its
# staging areas and line 4. Its mailbox layout follows the version the table is written for.
#
# THE HARNESS'S PAGES: 0x00 the dword `RDVL` answers, 0x10 and 0x11 the `_STA` bytes of `_UID` 0 and 1; the battery's
# `_STA` byte at 0x20 and its `_BST` dwords from 0x24 (state, rate, remaining, voltage); the adapter's `_PSR` byte at
# 0x34; the zone's `_TMP` dword at 0x38, in tenths of a kelvin; from 0x1000 the UCSI device's presence byte, its three
# counters (doorbell, function 2, notifications), the outbound CONTROL, the inbound VERSION and CCI, and the two
# 256-byte message staging areas at 0x1100 and 0x1200.
#
# THE HID-OVER-I2C GATE'S SSDT (`--hid-out`), a table of its own: below the I2C controller's companion node, the
# backend's two HID models as laptops describe theirs - a vendor `_HID` with `_CID` `PNP0C50`, the `I2cSerialBusV2`
# address, a `GpioInt` (level, active low) naming the model's line on the GPIO controller's node, and a `_DSM` answering
# the HID descriptor register.
#
# THE TCPCI GATE'S SSDT (`--tcpc-out`), a table of its own: below the same companion node, the backend's port controller
# (`vhost-i2c-gpio.py --tcpc`) as a `PRP0001` device whose `_DSD` names `compatible` `tcpci` - the `I2cSerialBusV2`
# address, the alert's `GpioInt` (level, active low) - and the connector as the hierarchical data node `connector`:
# `usb-c-connector`, a sink, Fixed 5 V at 3 A and 15 V at 2 A, operating at 15 W.
#
#   acpi-fixture.py --out FILE [--ucsi-version V]      write the SSDT, the UCSI mailbox laid out for V (default 2.1)
#   acpi-fixture.py --hid-out FILE     write the HID-over-I2C SSDT
#   acpi-fixture.py --tcpc-out FILE    write the TCPCI SSDT
#   acpi-fixture.py --memory FILE [--ucsi-version V]   create the ivshmem backing file (1 MiB) with both devices
#                                                    present - and the UCSI device, with VERSION V, when V is given
#   acpi-fixture.py --poke FILE OFFSET BYTES-HEX   write bytes into the backing file
#   acpi-fixture.py --set FILE NAME=VALUE...       write the power classes' named values (`POWER_FIELDS`)
#   acpi-fixture.py --power-storm FILE CONTROL     raise the power line many times through the GPIO backend's control
#                                                  socket, the zone's reading changed before each; then write
#                                                  `STORM_FINAL` and raise it until one more event fires
#   acpi-fixture.py --sleep-event FILE CONTROL NAME   the sleep devices' event NAME - lid-close, lid-open, power-button,
#                                                  sleep-button - written into the pages and line SLEEP_LINE raised
#                                                  until an event fires
#   acpi-fixture.py --tad-clock FILE UNIX          set the Time and Alarm Device's clock to UNIX (UTC)
#   acpi-fixture.py --tad-read FILE                print the timers the TAD's driver programmed, as JSON
#   acpi-fixture.py --power-read FILE              print the sleep devices' power resources and states, as JSON
#   acpi-fixture.py --processor-read FILE          print what the kernel wrote to the processors' registers, `_PPC`,
#                                                  each fan's level and `_SCP`, as JSON
#   acpi-fixture.py --raise CONTROL LINE           raise LINE through the GPIO backend's control socket until an event
#                                                  fires - PROCESSOR_LINE notifies C000 with 0x80
#   acpi-fixture.py --self-test        build the table and check it, writing nothing
#
# THE SLEEP GATE'S DEVICES (`--out FILE --sleep`, and `--memory FILE --sleep`), in the same table so their `Notify` has
# the GPIO controller's `_AEI` to come from - line SLEEP_LINE, whose `_E06` raises what the pages' event byte names:
#   PNP0C0D  (`\_SB.LID0`) - the lid: `_LID` the pages' byte, and `_PRW`.
#   PNP0C0C  (`\_SB.PWRB`) - a control-method power button, and `_PRW`.
#   PNP0C0E  (`\_SB.SLPB`) - a control-method sleep button, and `_PRW`.
#   ACPI000E (`\_SB.TAD0`) - a Time and Alarm Device: `_GCP` from the pages, `_GRT` the pages' sixteen bytes - the harness
#            keeps them running - and `_STV`, `_TIV`, `_GWS` and `_CWS` over the pages' timers, `_STV` counted.
# THE PROCESSOR-POWER GATE'S OBJECTS (`--out FILE --processors`, and `--memory FILE --processors`), in the same table so
# their registers are in the same function's BAR and their `Notify` has the GPIO controller's `_AEI` to come from:
#   `\_SB.CPUS.C000` - `_PSS` (3000, 2400, 1800 and 1200 MHz, control 0x10 to 0x13) through `_PCT`'s control and status
#            registers, `_PPC` the pages' byte and `_PSD` software-all over itself; line PROCESSOR_LINE's `_E07`
#            notifies it with 0x80. ITS `_OST` IS QEMU'S: the q35 DSDT declares one on every `\_SB.CPUS` processor for
#            CPU hot-plug, it stands before this table's, and QEMU keeps what it was told (`query-acpi-ospm-status`).
#   `\_SB.CPUS.C001` - CPPC: `_CPC` revision 3, 255 to 50, its desired-performance and energy-preference registers.
#   `\_SB.CPUS.C002` - `_PSS` through a `_PCT` of model-specific registers, which the kernel refuses whole.
#   Every one of them an `_LPI` of three states - the halt, then two entered by a read of the same two registers on every
#   core, 50 and 400 us out. The kernel's registers - `_PCT`'s, `_CPC`'s and `_LPI`'s - are on PROCESSOR_PAGE, a page of
#   their own past every node's range, which only the kernel maps (the development build's fixture exception).
#   THE ZONE gains `_TC1` 4, `_TC2` 3, `_TSP` 1 s, `_PSL` naming C000 and C001, `_AL0` naming FAN0 and `_SCP`.
# THE FANS, in the sleep table and the processor-power table: `\_SB.FAN0` (`PNP0C0B`, three `_FPS` levels - control 0, 50
# and 100) and `\_SB.FAN1` (`PNP0C0B`, `_FIF`'s fine-grain control in steps of 5 %), `_FSL` and `_FST` over the pages.
#
# THE BRIGHTNESS GATE'S DEVICES (`--out FILE --brightness`, and `--memory FILE --brightness`), in the same table so their
# `Notify` has the GPIO controller's `_AEI` to come from - line BRIGHTNESS_LINE, whose `_E08` notifies the value the
# pages' event byte names, 0x80 to the sensor and anything else to the panel:
#   `\_SB.PCI0.S08` - q35's VGA function, QEMU's own node opened with `Scope` (a run with `VGA=std`): `_DOS` recording its
#            argument and counting itself in the pages, beside a `_DOD` naming the panel's `_ADR`, as firmware declares both.
#   `\_SB.PCI0.S08.LCD0` - the panel's output, `_ADR` 0x400: `_BCL` an AC default of 70, a battery default of 30 and the
#            levels 0 to 100 in tens; `_BCM` writes the level into the pages and counts itself; `_BQC` reads it.
#   ACPI0008 (`\_SB.ALS0`) - an ambient-light sensor: `_ALI` the pages' dword in lux, `_ALR` a five-point curve.
#
# AND TWO POWER RESOURCES: `\_SB.PSLP` in the `_PR0` of both the lid and the TAD - on while either is in D0, off only
# when both have left it - and `\_SB.PWAK` in the lid's `_PRW`, on while its wake is armed. Each resource's state and the
# counts of its `_ON` and `_OFF` are in the pages, and so is the last `_PSx` each device ran; the lid says `_S0W` and
# `_S3W` are D3hot, the TAD says neither, so it stays in D0 through a sleep its timer is to wake.

import argparse
import os
import struct
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import aml_emitter as E  # noqa: E402

IVSHMEM_SLOT = 0x14
I2C_SLOT = 0x15
GPIO_SLOT = 0x16
AEI_LINE = 2
POWER_LINE = 3
UCSI_LINE = 4
FIELD_LINE = 5
I2C_ADDRESS = 0x50
I2C_REGISTER = 0x10
HARNESS_PAGES = 0x2000
DEVICE_RANGE = 0x4000
MEMORY_SIZE = 1 << 20
VALUE_OFFSET = 0x00
PRESENT_OFFSETS = (0x10, 0x11)
BATTERY_STA = 0x20
BATTERY_BST = 0x24
AC_PSR = 0x34
ZONE_TMP = 0x38
# THE FIRST VALUES: a present battery charging at 5 W with 24 of 48 Wh at 11.1 V, on mains, the zone at 300.2 K.
BATTERY_FIRST = (0x1F, 0b10, 5000, 24000, 11100)
AC_FIRST = 1
ZONE_FIRST = 3002
# THE NAMED VALUES the gate writes: (offset, bytes).
POWER_FIELDS = {
	'battery-sta': (BATTERY_STA, 1),
	'battery-state': (BATTERY_BST, 4),
	'battery-rate': (BATTERY_BST + 4, 4),
	'battery-remaining': (BATTERY_BST + 8, 4),
	'battery-voltage': (BATTERY_BST + 12, 4),
	'ac-online': (AC_PSR, 1),
	'zone-temperature': (ZONE_TMP, 4),
}
# THE STORM: at least this many events fired, within this many seconds, and the zone's last reading - 330.0 K, which
# no reading of the storm itself is.
STORM_EVENTS = 60
STORM_SECONDS = 30
STORM_FINAL = 3300
DSM_UUID = '5c3c6b2e-8d7a-4f5b-9a41-2e1d7f0a6b93'
# THE SLEEP GATE'S PAGES: the event byte `_E06` reads and clears (bit 0 the lid, 1 the power button, 2 the sleep button),
# the lid's byte (1 open), and the TAD's capabilities, its two timers and their wake statuses, the count of `_STV` calls
# and its `_GRT` buffer.
SLEEP_LINE = 6
SLEEP_GPE = 0x0E
SLEEP_EVENTS = 0x40
LID_OPEN = 0x41
TAD_GCP = 0x44
TAD_AC_TIMER = 0x48
TAD_DC_TIMER = 0x4C
TAD_AC_STATUS = 0x50
TAD_DC_STATUS = 0x54
TAD_STV_CALLS = 0x58
TAD_GRT = 0x60
TAD_DISABLED = 0xFFFFFFFF
# THE POWER RESOURCES' BYTES: each resource's state, its `_ON` count and its `_OFF` count, then the lid's and the TAD's
# last `_PSx` (0xFF before the first).
POWER_PAGE = 0x70
POWER_BYTES = ('pslp_on', 'pslp_ons', 'pslp_offs', 'pwak_on', 'pwak_ons', 'pwak_offs', 'lid_ps', 'tad_ps')
SLEEP_EVENT_BITS = {'lid-close': 1, 'lid-open': 1, 'power-button': 2, 'sleep-button': 4}

# THE PROCESSOR-POWER GATE'S PAGES: C000's `_PPC` byte, each fan's level and its `_FSL` count, and the zone's `_SCP`
# mode and count.
PROCESSOR_LINE = 7
PROCESSOR_PPC = 0x80
FAN_PAGE = 0x90
ZONE_SCP = 0xA0
# THE KERNEL'S REGISTERS, on a page of their own: `_PCT`'s control and status, `_CPC`'s desired performance and energy
# preference, and the two `_LPI` entry registers.
PROCESSOR_PAGE = 0x8000
PCT_CONTROL = PROCESSOR_PAGE + 0x00
PCT_STATUS = PROCESSOR_PAGE + 0x04
CPC_DESIRED = PROCESSOR_PAGE + 0x08
CPC_PREFERENCE = PROCESSOR_PAGE + 0x0C
LPI_ENTRY = (PROCESSOR_PAGE + 0x10, PROCESSOR_PAGE + 0x14)
PSS_STATES = [(3000, 15000, 10, 10, 0x10, 0x10), (2400, 10000, 10, 10, 0x11, 0x11), (1800, 6000, 10, 10, 0x12, 0x12), (1200, 3000, 10, 10, 0x13, 0x13)]
# THE `_LPI` STATES past the halt: (residency, latency) in microseconds.
LPI_STATES = [(150, 50), (1200, 400)]
CPPC_LEVELS = (255, 200, 100, 50)
SPACE_MEMORY = 0
SPACE_FIXED_HARDWARE = 0x7F
PROCESSOR_FIELDS = {'ppc': (PROCESSOR_PPC, 1), 'fan0': (FAN_PAGE, 4), 'fan1': (FAN_PAGE + 8, 4)}

# THE BRIGHTNESS GATE'S PAGES: the panel's level `_BCM` writes and `_BQC` reads, `_BCM`'s count, `_DOS`'s last argument and
# count, the event byte `_E08` notifies and clears, and the sensor's illuminance in lux.
BRIGHTNESS_LINE = 8
BRIGHTNESS_PAGE = 0xB0
BRIGHTNESS_LEVEL = BRIGHTNESS_PAGE
BRIGHTNESS_CALLS = BRIGHTNESS_PAGE + 1
DOS_VALUE = BRIGHTNESS_PAGE + 2
DOS_CALLS = BRIGHTNESS_PAGE + 3
BRIGHTNESS_EVENT = BRIGHTNESS_PAGE + 4
ILLUMINANCE = BRIGHTNESS_PAGE + 8
VGA_SLOT = 0x01
# `_BCL`: the AC default, the battery default, then the levels - the firmware default a boot starts the region at is the
# AC one, since the fixture's adapter says the machine is on mains.
BCL = (70, 30, 0, 10, 20, 30, 40, 50, 60, 70, 80, 90, 100)
# `_ALR`: (adjustment in percent of normal, illuminance in lux).
ALR = ((70, 0), (73, 10), (85, 80), (100, 300), (150, 1000))
BRIGHTNESS_FIELDS = {'level': (BRIGHTNESS_LEVEL, 1), 'illuminance': (ILLUMINANCE, 4), 'dos': (DOS_VALUE, 1), 'dos-calls': (DOS_CALLS, 1)}

# THE UCSI DEVICE (`\_SB.UCSI`), absent until the harness's `UCPR` byte says a PPM is there: `_HID` `USBC000` and `_CID`
# `PNP0CA0`, as shipping laptops name it, its mailbox `_CRS` range at UCSI_RANGE past the harness's pages and its own
# region over it. Its `_DSM` behaves as firmware does: function 1 copies CONTROL and MESSAGE_OUT into the OUTBOUND
# staging area and rings the doorbell (`ODBL`); function 2 copies the INBOUND staging area into VERSION, CCI and
# MESSAGE_IN and counts itself (`LOG2`); line UCSI_LINE's `_E04` makes the same copy, counts the notification (`NCNT`)
# and runs `Notify(0x80)`. The staging areas and the counters are in the harness's pages, which the ivshmem function's
# companion declares - no other node's region covers the claimed range. `ucsi-ppm.py` is the PPM behind them.
UCSI_UUID = '6f8398c2-7ca4-11e4-ad36-631042b5008f'
UCSI_RANGE = 0x6000
UCSI_PRESENT = 0x1000
UCSI_DOORBELL = 0x1004
UCSI_REFRESHES = 0x1008
UCSI_NOTIFIES = 0x100C
UCSI_CONTROL = 0x1010
UCSI_VERSION = 0x1018
UCSI_CCI = 0x101C
UCSI_MESSAGE_OUT = 0x1100
UCSI_MESSAGE_IN = 0x1200


def ucsi_mailbox_units(version):
	"""VERSION, CCI, CONTROL, MESSAGE_IN and MESSAGE_OUT as the version lays them out: 16-byte messages before 2.0,
	256-byte ones from it."""
	message = 128 if version < 0x0200 else 2048
	return [E.unit('VER_', 16), E.offset_to(16), E.unit('CCI_', 32), E.unit('CTRL', 64), E.unit('MSGI', message), E.unit('MSGO', message)]


def sleep_devices(ivsh):
	"""THE LID, THE CONTROL-METHOD BUTTONS AND THE TIME AND ALARM DEVICE, each able to wake the machine through `_PRW`."""
	prw = E.name('_PRW', E.package(SLEEP_GPE, 4))
	timer = lambda ac, dc: lambda: [E.if_(E.lequal(E.arg(0), 0), [E.ret(ac)]), E.ret(dc)]

	# A POWER RESOURCE SWITCHED THROUGH THE PAGES: its state, and each `_ON` and `_OFF` counted.
	def resource(n, order, state, ons, offs):
		return E.power_resource(n, 0, order, [
			E.method('_STA', 0, [E.ret(ivsh + '.' + state)]),
			E.method('_ON', 0, [E.store(1, ivsh + '.' + state), E.increment(ivsh + '.' + ons)], serialized=True),
			E.method('_OFF', 0, [E.store(0, ivsh + '.' + state), E.increment(ivsh + '.' + offs)], serialized=True),
		])

	# `_PS0` AND `_PS3`, each writing the state it entered.
	def states(field):
		return [E.method('_PS0', 0, [E.store(0, ivsh + '.' + field)]), E.method('_PS3', 0, [E.store(3, ivsh + '.' + field)])]

	return [
		resource('PSLP', 0, 'PSST', 'PSON', 'PSOF'),
		resource('PWAK', 1, 'PWST', 'PWON', 'PWOF'),
		E.device('LID0', [
			E.name('_HID', E.eisaid('PNP0C0D')),
			E.method('_LID', 0, [E.ret(ivsh + '.LIDO')]),
			E.name('_PRW', E.package(SLEEP_GPE, 3, '\\_SB.PWAK')),
			E.name('_PR0', E.package('\\_SB.PSLP')),
			E.name('_S0W', 3),
			E.name('_S3W', 3),
		] + states('LIDP')),
		E.device('PWRB', [E.name('_HID', E.eisaid('PNP0C0C')), prw]),
		E.device('SLPB', [E.name('_HID', E.eisaid('PNP0C0E')), prw]),
		E.device('TAD0', [
			E.name('_HID', E.string('ACPI000E')),
			prw,
			E.name('_PR0', E.package('\\_SB.PSLP')),
		] + states('TADP') + [
			E.method('_GCP', 0, [E.ret(ivsh + '.TGCP')]),
			E.method('_GRT', 0, [E.ret(ivsh + '.TGRT')]),
			E.method('_STV', 2, [
				E.increment(ivsh + '.STVN'),
				E.if_(E.lequal(E.arg(0), 0), [E.store(E.arg(1), ivsh + '.TACT')]),
				E.if_(E.lequal(E.arg(0), 1), [E.store(E.arg(1), ivsh + '.TDCT')]),
				E.ret(0),
			], serialized=True),
			E.method('_TIV', 1, timer(ivsh + '.TACT', ivsh + '.TDCT')()),
			E.method('_GWS', 1, timer(ivsh + '.TAST', ivsh + '.TDST')()),
			E.method('_CWS', 1, [
				E.if_(E.lequal(E.arg(0), 0), [E.store(0, ivsh + '.TAST')]),
				E.if_(E.lequal(E.arg(0), 1), [E.store(0, ivsh + '.TDST')]),
				E.ret(0),
			], serialized=True),
		]),
	]


def brightness_devices(ivsh, no_dos=False):
	"""THE PANEL BELOW Q35'S VGA FUNCTION, ITS ADAPTER'S `_DOS` AND `_DOD`, AND THE LIGHT SENSOR."""
	vga = f'\\_SB.PCI0.S{VGA_SLOT << 3:02X}'
	return [
		E.scope(vga, [
			*([] if no_dos else [E.method('_DOS', 1, [E.store(E.arg(0), ivsh + '.DOSV'), E.increment(ivsh + '.DOSN')], serialized=True)]),
			E.name('_DOD', E.package(0x80010400)),
			E.device('LCD0', [
				E.name('_ADR', 0x400),
				E.name('_BCL', E.package(*BCL)),
				E.method('_BCM', 1, [E.store(E.arg(0), ivsh + '.BLVL'), E.increment(ivsh + '.BCMN')], serialized=True),
				E.method('_BQC', 0, [E.ret(ivsh + '.BLVL')]),
			]),
		]),
		E.scope('\\_SB', [
			E.device('ALS0', [
				E.name('_HID', E.string('ACPI0008')),
				E.method('_ALI', 0, [E.ret(ivsh + '.ALSI')]),
				E.name('_ALR', E.package(*[E.package(adjustment, lux) for adjustment, lux in ALR])),
			]),
		]),
	]


def patched(name_, buffers, ivsh):
	"""A METHOD'S BODY that writes each `(index, offset, space, bits)` register - its address the BAR's base plus `offset` -
	into the package `name_` at `index`, and answers the package: the register descriptors are data, and the base is
	read at run time."""
	body = []
	for at, (index, offset, space, bits) in enumerate(buffers):
		buffer_name = f'RB{at:02X}'
		body += [
			E.name(buffer_name, E.buffer(E.resource_template(E.generic_register(space, bits, 0)))),
			E.create_dword_field(buffer_name, 7, f'RA{at:02X}'),
			E.store(E.add(E.call(ivsh + '.BASE'), offset), f'RA{at:02X}'),
			E.store(buffer_name, E.index(name_, index)),
		]
	return body + [E.ret(name_)]


def lpi(ivsh):
	"""`_LPI`: the halt, then the two states entered by a read of LPI_ENTRY - the same registers on every core."""
	none = E.buffer(E.resource_template(E.generic_register(0, 0, 0)))
	halt = E.package(1, 1, 1, 0, 0, 0, E.buffer(E.resource_template(E.generic_register(SPACE_FIXED_HARDWARE, 1, 0, offset=1))), none, none, E.string('C1'))
	states = [E.package(residency, latency, 1, 0, 0, 0, 0, none, none, E.string(f'C{2 + at}')) for at, (residency, latency) in enumerate(LPI_STATES)]
	body = [E.store('LPS0', E.index('PLPI', 3))]
	for at, offset in enumerate(LPI_ENTRY):
		state = f'LPS{at + 1}'
		body += [
			E.name(f'RB{at:02X}', E.buffer(E.resource_template(E.generic_register(SPACE_MEMORY, 32, 0)))),
			E.create_dword_field(f'RB{at:02X}', 7, f'RA{at:02X}'),
			E.store(E.add(E.call(ivsh + '.BASE'), offset), f'RA{at:02X}'),
			E.store(f'RB{at:02X}', E.index(state, 6)),
			E.store(state, E.index('PLPI', 4 + at)),
		]
	return [
		E.name('LPS0', halt),
		E.name('LPS1', states[0]),
		E.name('LPS2', states[1]),
		E.name('PLPI', E.package(0, 0, 1 + len(LPI_STATES), 0, 0, 0)),
		E.method('_LPI', 0, body + [E.ret('PLPI')], serialized=True),
	]


def processor_objects(ivsh):
	"""THE THREE PROCESSORS' POWER OBJECTS, added to the nodes QEMU's DSDT declares."""
	pss = E.name('_PSS', E.package(*[E.package(*state) for state in PSS_STATES]))
	highest, nominal, nonlinear, lowest = CPPC_LEVELS
	cpc = [23, 3, highest, nominal, nonlinear, lowest, 0, 0, 0, 0] + [0] * 9 + [0, 0, 0, 0]
	msr = lambda address: E.buffer(E.resource_template(E.generic_register(SPACE_FIXED_HARDWARE, 64, address)))
	return [
		E.scope('\\_SB.CPUS.C000', [
			pss,
			E.name('PPCT', E.package(0, 0)),
			E.method('_PCT', 0, patched('PPCT', [(0, PCT_CONTROL, SPACE_MEMORY, 32), (1, PCT_STATUS, SPACE_MEMORY, 32)], ivsh), serialized=True),
			E.method('_PPC', 0, [E.ret(ivsh + '.PPCV')]),
			E.name('_PSD', E.package(E.package(5, 0, 0, 0xFC, 1))),
		] + lpi(ivsh)),
		E.scope('\\_SB.CPUS.C001', [
			E.name('PCPC', E.package(*cpc)),
			E.method('_CPC', 0, patched('PCPC', [(7, CPC_DESIRED, SPACE_MEMORY, 32), (19, CPC_PREFERENCE, SPACE_MEMORY, 32)], ivsh), serialized=True),
		] + lpi(ivsh)),
		E.scope('\\_SB.CPUS.C002', [
			E.name('_PSS', E.package(*[E.package(*state) for state in PSS_STATES[:2]])),
			E.name('_PCT', E.package(msr(0x199), msr(0x198))),
		] + lpi(ivsh)),
	]


def fans(ivsh):
	"""FAN0, WITH LEVELS, AND FAN1, LEFT TO THE OPERATING SYSTEM IN FINE STEPS: `_FSL` writes the level into the pages
	and counts itself, `_FST` answers it with a speed of forty rpm per unit."""
	def fan(n, uid, fif, fps, level, count):
		return E.device(n, [
			E.name('_HID', E.eisaid('PNP0C0B')),
			E.name('_UID', uid),
			E.name('_FIF', E.package(*fif)),
			E.name('_FPS', E.package(0, *[E.package(control, 0xFFFFFFFF, speed, noise, power) for control, speed, noise, power in fps])),
			E.method('_FSL', 1, [E.store(E.arg(0), ivsh + '.' + level), E.increment(ivsh + '.' + count)], serialized=True),
			E.name('PFST', E.package(0, 0, 0)),
			E.method('_FST', 0, [E.store(ivsh + '.' + level, E.index('PFST', 1)), E.store(E.multiply(ivsh + '.' + level, 40), E.index('PFST', 2)), E.ret('PFST')], serialized=True),
		])
	return [
		fan('FAN0', 0, (0, 0, 0, 0), [(0, 0, 0, 0), (50, 2000, 30, 1500), (100, 4000, 45, 3000)], 'F0LV', 'F0LN'),
		fan('FAN1', 1, (0, 1, 5, 0), [(0, 0, 0, 0), (100, 4000, 45, 3000)], 'F1LV', 'F1LN'),
	]


def ssdt_body(ucsi_version=0x0210, sleep=False, processors=False, brightness=False, brightness_no_dos=False):
	# THE NODES QEMU'S DSDT ALREADY HAS for the three functions - `S` and the slot times eight - opened with `Scope`: a
	# second node with the same `_ADR` would be a second companion of one function.
	ivsh = f'\\_SB.PCI0.S{IVSHMEM_SLOT << 3:02X}'
	i2cb = f'\\_SB.PCI0.S{I2C_SLOT << 3:02X}'
	gpio = f'\\_SB.PCI0.S{GPIO_SLOT << 3:02X}'
	return [
		E.scope(ivsh, [
			E.operation_region('IVCF', 'PCI_Config', 0, 0x40),
			E.field('IVCF', [E.offset_to(0x18 * 8), E.unit('BR2L', 32), E.unit('BR2H', 32)], access='DWordAcc'),
			# THE BAR'S ADDRESS, below 4 GiB: q35's DSDT is revision 1, so every integer in the namespace is 32 bits -
			# the run places the BAR there (`qemu_attach_acpi_fixture`) and the gate checks `BR2H` is zero.
			E.method('BASE', 0, [E.ret(E.band('BR2L', 0xFFFFFFF0))]),
			E.operation_region('HPGS', 'SystemMemory', 'BASE', HARNESS_PAGES),
			E.field('HPGS', [
				E.unit('VAL0', 32), E.offset_to((PRESENT_OFFSETS[0] - 4) * 8), E.unit('PRS0', 8), E.unit('PRS1', 8),
				E.offset_to((BATTERY_STA - PRESENT_OFFSETS[1] - 1) * 8), E.unit('BSTA', 8), E.offset_to((BATTERY_BST - BATTERY_STA - 1) * 8),
				E.unit('BSTS', 32), E.unit('BRAT', 32), E.unit('BREM', 32), E.unit('BVOL', 32),
				E.unit('APSR', 8), E.offset_to((ZONE_TMP - AC_PSR - 1) * 8), E.unit('TTMP', 32),
			], access='AnyAcc'),
			# THE POWER RESOURCES' BYTES.
			E.field('HPGS', [
				E.offset_to(POWER_PAGE * 8), E.unit('PSST', 8), E.unit('PSON', 8), E.unit('PSOF', 8), E.unit('PWST', 8), E.unit('PWON', 8),
				E.unit('PWOF', 8), E.unit('LIDP', 8), E.unit('TADP', 8),
			], access='AnyAcc'),
			# THE SLEEP GATE'S BYTES, THE TAD'S TIMERS AND ITS CLOCK.
			E.field('HPGS', [
				E.offset_to(SLEEP_EVENTS * 8), E.unit('SLEV', 8), E.unit('LIDO', 8), E.offset_to((TAD_GCP - LID_OPEN - 1) * 8),
				E.unit('TGCP', 32), E.unit('TACT', 32), E.unit('TDCT', 32), E.unit('TAST', 32), E.unit('TDST', 32), E.unit('STVN', 32),
				E.offset_to((TAD_GRT - TAD_STV_CALLS - 4) * 8), E.unit('TGRT', 128),
			], access='AnyAcc'),
			# THE PROCESSOR-POWER GATE'S BYTES: `_PPC`, the fans and `_SCP`.
			E.field('HPGS', [
				E.offset_to(PROCESSOR_PPC * 8), E.unit('PPCV', 8),
				E.offset_to((FAN_PAGE - PROCESSOR_PPC - 1) * 8), E.unit('F0LV', 32), E.unit('F0LN', 32), E.unit('F1LV', 32), E.unit('F1LN', 32),
				E.offset_to((ZONE_SCP - FAN_PAGE - 16) * 8), E.unit('SCPM', 8), E.unit('SCPN', 8),
			], access='AnyAcc'),
			# THE BRIGHTNESS GATE'S BYTES: the panel's level and `_BCM` count, `_DOS`, the event, the illuminance.
			E.field('HPGS', [
				E.offset_to(BRIGHTNESS_PAGE * 8), E.unit('BLVL', 8), E.unit('BCMN', 8), E.unit('DOSV', 8), E.unit('DOSN', 8), E.unit('BEVT', 8),
				E.offset_to((ILLUMINANCE - BRIGHTNESS_EVENT - 1) * 8), E.unit('ALSI', 32),
			], access='AnyAcc'),
			# THE UCSI STAGING AREAS AND COUNTERS.
			E.field('HPGS', [
				E.offset_to(UCSI_PRESENT * 8), E.unit('UCPR', 8), E.offset_to((UCSI_DOORBELL - UCSI_PRESENT - 1) * 8),
				E.unit('ODBL', 32), E.unit('LOG2', 32), E.unit('NCNT', 32), E.unit('OCTL', 64), E.unit('IVER', 16), E.offset_to(16), E.unit('ICCI', 32),
				E.offset_to((UCSI_MESSAGE_OUT - UCSI_CCI - 4) * 8), E.unit('OMSG', 2048), E.unit('IMSG', 2048),
			], access='AnyAcc'),
		]),
		E.scope(gpio, [
			E.name('_AEI', E.buffer(E.resource_template(*[E.gpio_int([line], gpio, edge=True) for line in [AEI_LINE, POWER_LINE, UCSI_LINE] + ([SLEEP_LINE] if sleep else []) + ([PROCESSOR_LINE] if processors else []) + ([BRIGHTNESS_LINE] if brightness else [])]))),
			E.method(f'_E{AEI_LINE:02X}', 0, [E.notify('\\_SB.LSF1', 0x80)]),
			# THE PPM'S NOTIFICATION, as a laptop's notification method makes it: the copy, then `Notify`.
			E.method(f'_E{UCSI_LINE:02X}', 0, [E.call('\\_SB.UCSI.COPY'), E.increment(ivsh + '.NCNT'), E.notify('\\_SB.UCSI', 0x80)]),
			E.method(f'_E{POWER_LINE:02X}', 0, [E.notify('\\_SB.BAT0', 0x80), E.notify('\\_SB.ADP0', 0x80), E.notify('\\_TZ.TZ00', 0x80)]),
		] + ([
			# THE SLEEP DEVICES' EVENTS, as the pages' byte names them, and the byte cleared.
			E.method(f'_E{SLEEP_LINE:02X}', 0, [
				E.if_(E.band(ivsh + '.SLEV', 1), [E.notify('\\_SB.LID0', 0x80)]),
				E.if_(E.band(ivsh + '.SLEV', 2), [E.notify('\\_SB.PWRB', 0x80)]),
				E.if_(E.band(ivsh + '.SLEV', 4), [E.notify('\\_SB.SLPB', 0x80)]),
				E.store(0, ivsh + '.SLEV'),
			], serialized=True),
		] if sleep else []) + ([
			# THE PROCESSOR'S PERFORMANCE LIMIT CHANGED: C000 notified, `_PPC` read again from the pages.
			E.method(f'_E{PROCESSOR_LINE:02X}', 0, [E.notify('\\_SB.CPUS.C000', 0x80)]),
		] if processors else []) + ([
			# THE BRIGHTNESS GATE'S EVENT, as the pages' byte names it: 0x80 to the sensor, a panel notification - 0x85 to
			# 0x89 - to the panel; the byte cleared.
			E.method(f'_E{BRIGHTNESS_LINE:02X}', 0, [
				E.if_(E.lequal(ivsh + '.BEVT', 0x80), [E.notify('\\_SB.ALS0', 0x80)]),
				*[E.if_(E.lequal(ivsh + '.BEVT', value), [E.notify(f'\\_SB.PCI0.S{VGA_SLOT << 3:02X}.LCD0', value)]) for value in (0x85, 0x86, 0x87, 0x88, 0x89)],
				E.store(0, ivsh + '.BEVT'),
			], serialized=True),
		] if brightness else [])),
		E.scope('\\_SB', [
			E.device('LSF1', [
				E.name('_HID', E.string('LSFX0001')),
				E.name('_UID', 0),
				E.method('_CRS', 0, [
					E.name('RBUF', E.buffer(E.resource_template(E.memory32_fixed(0, 0x1000)))),
					E.create_dword_field('RBUF', 4, 'MBAS'),
					E.store(E.add(E.call(ivsh + '.BASE'), DEVICE_RANGE), 'MBAS'),
					E.ret('RBUF'),
				], serialized=True),
				# ITS OWN REGION INSIDE ITS OWN `_CRS` RANGE: shared with its driver while the claim holds it.
				E.operation_region('OWNR', 'SystemMemory', E.add(E.call(ivsh + '.BASE'), DEVICE_RANGE), 0x100),
				E.field('OWNR', [E.unit('OWN0', 32)], access='DWordAcc'),
				E.method('RDVL', 0, [E.ret(ivsh + '.VAL0')]),
				E.method('WRVL', 1, [E.store(E.arg(0), 'OWN0'), E.ret('OWN0')]),
				E.method('OTHR', 0, [E.ret(E.call('\\_SB.LSF3.PEEK'))]),
				E.operation_region('GPR0', 'GeneralPurposeIo', 0, 1),
				E.field('GPR0', [E.connection(E.gpio_io([FIELD_LINE], gpio, restriction='input')), E.unit('LIN5', 1)], access='ByteAcc'),
				E.method('GPRD', 0, [E.ret('LIN5')]),
				E.method('GPWR', 0, [E.store(1, 'LIN5')]),
				E.operation_region('GSB0', 'GenericSerialBus', 0, 0x100),
				E.field('GSB0', [E.connection(E.i2c_serial_bus_v2(I2C_ADDRESS, i2cb)), E.offset_to(I2C_REGISTER * 8), E.access_as('BufferAcc', 0x06), E.unit('R10_', 8)], access='BufferAcc'),
				E.name('WBUF', E.buffer([0, 1, 0])),
				E.method('GSBW', 1, [E.create_byte_field('WBUF', 2, 'WDAT'), E.store(E.arg(0), 'WDAT'), E.store('WBUF', 'R10_')], serialized=True),
				E.method('GSBR', 0, [E.store('R10_', E.local(0)), E.ret(E.deref(E.index(E.local(0), 2)))]),
				E.name('_DSD', E.dsd({'fixture-mode': 3})),
				E.dsm(DSM_UUID, {1: [E.ret(E.add(E.deref(E.index(E.arg(3), 0)), 1))]}),
			]),
			E.device('LSF3', [
				E.name('_HID', E.string('LSFX0003')),
				E.operation_region('OVER', 'SystemMemory', E.add(E.call(ivsh + '.BASE'), DEVICE_RANGE), 0x10),
				E.field('OVER', [E.unit('OVR0', 32)], access='DWordAcc'),
				E.method('PEEK', 0, [E.ret('OVR0')]),
			]),
			E.device('LSF4', [E.name('_HID', E.string('LSFX0002')), E.name('_UID', 0), E.method('_STA', 0, [E.if_(ivsh + '.PRS0', [E.ret(0x0F)]), E.ret(0)])]),
			E.device('LSF5', [E.name('_HID', E.string('LSFX0002')), E.name('_UID', 1), E.method('_STA', 0, [E.if_(ivsh + '.PRS1', [E.ret(0x0F)]), E.ret(0)])]),
			E.device('BAT0', [
				E.name('_HID', E.eisaid('PNP0C0A')),
				E.name('_UID', 0),
				E.method('_STA', 0, [E.ret(ivsh + '.BSTA')]),
				# REVISION 1: the unit (milliwatt hours), the capacities, the technology, the voltage, the levels, the cycle
				# count, the accuracy, four sampling and averaging times, two granularities, four strings and swapping.
				E.name('_BIX', E.package(1, 0, 50000, 48000, 1, 11100, 5000, 2000, 10, 100000, 0xFFFFFFFF, 0xFFFFFFFF, 0xFFFFFFFF, 0xFFFFFFFF, 100, 100, E.string('LSFX-BAT'), E.string('0001'), E.string('LION'), E.string('LiberSoft'), 0)),
				# `_BST` FROM THE PAGES, stored into the package it answers with: a package's elements are data, not
				# expressions, so firmware fills one this way.
				E.name('PBST', E.package(0, 0, 0, 0)),
				E.method('_BST', 0, [
					E.store(ivsh + '.BSTS', E.index('PBST', 0)),
					E.store(ivsh + '.BRAT', E.index('PBST', 1)),
					E.store(ivsh + '.BREM', E.index('PBST', 2)),
					E.store(ivsh + '.BVOL', E.index('PBST', 3)),
					E.ret('PBST'),
				], serialized=True),
			]),
			E.device('UCSI', [
				E.name('_HID', E.string('USBC000')),
				E.name('_CID', E.eisaid('PNP0CA0')),
				E.name('_UID', 0),
				E.method('_STA', 0, [E.if_(ivsh + '.UCPR', [E.ret(0x0F)]), E.ret(0)]),
				E.method('_CRS', 0, [
					E.name('RBUF', E.buffer(E.resource_template(E.memory32_fixed(0, 0x1000)))),
					E.create_dword_field('RBUF', 4, 'MBAS'),
					E.store(E.add(E.call(ivsh + '.BASE'), UCSI_RANGE), 'MBAS'),
					E.ret('RBUF'),
				], serialized=True),
				# ITS OWN REGION OVER ITS OWN MAILBOX, which the claim shares with its driver.
				E.operation_region('MBOX', 'SystemMemory', E.add(E.call(ivsh + '.BASE'), UCSI_RANGE), 0x1000),
				E.field('MBOX', ucsi_mailbox_units(ucsi_version), access='AnyAcc'),
				E.method('COPY', 0, [E.store(ivsh + '.IVER', 'VER_'), E.store(ivsh + '.ICCI', 'CCI_'), E.store(ivsh + '.IMSG', 'MSGI')], serialized=True),
				E.dsm(UCSI_UUID, {
					1: [E.store('CTRL', ivsh + '.OCTL'), E.store('MSGO', ivsh + '.OMSG'), E.increment(ivsh + '.ODBL'), E.ret(0)],
					2: [E.call('COPY'), E.increment(ivsh + '.LOG2'), E.ret(0)],
				}),
			]),
			E.device('ADP0', [
				E.name('_HID', E.string('ACPI0003')),
				E.method('_PSR', 0, [E.ret(ivsh + '.APSR')]),
			]),
		] + (sleep_devices(ivsh) if sleep else []) + (fans(ivsh) if sleep or processors else [])),
		E.scope('\\_TZ', [
			E.thermal_zone('TZ00', [
				E.method('_TMP', 0, [E.ret(ivsh + '.TTMP')]),
				E.name('_CRT', 3682),
				E.name('_HOT', 3632),
				E.name('_PSV', 3532),
				E.name('_AC0', 3432),
			] + ([
				E.name('_TC1', 4),
				E.name('_TC2', 3),
				E.name('_TSP', 10),
				E.name('_PSL', E.package('\\_SB.CPUS.C000', '\\_SB.CPUS.C001')),
				E.name('_AL0', E.package('\\_SB.FAN0')),
				E.method('_SCP', 1, [E.store(E.arg(0), ivsh + '.SCPM'), E.increment(ivsh + '.SCPN')], serialized=True),
			] if processors else [])),
		]),
	] + (processor_objects(ivsh) if processors else []) + (brightness_devices(ivsh, brightness_no_dos) if brightness else [])


def ssdt(ucsi_version=0x0210, sleep=False, processors=False, brightness=False, brightness_no_dos=False):
	return E.table('SSDT', ssdt_body(ucsi_version, sleep, processors, brightness, brightness_no_dos), oem_table_id=b'LIBACPIF')


HID_OVER_I2C = '3cdff6f7-4267-4555-ad05-b30a3d8938de'
# (node, _HID, address, descriptor register, GPIO line) - the backend's two models (`vhost-i2c-gpio.py --hid`).
HID_DEVICES = [('TPAD', 'LSFX0C50', 0x2C, 0x20, 0), ('TSCR', 'LSFX0C51', 0x10, 0x01, 1)]


def hid_body():
	i2cb = f'\\_SB.PCI0.S{I2C_SLOT << 3:02X}'
	gpio = f'\\_SB.PCI0.S{GPIO_SLOT << 3:02X}'
	devices = []
	for node, hid, address, register, line in HID_DEVICES:
		devices.append(E.device(node, [
			E.name('_HID', E.string(hid)),
			E.name('_CID', E.string('PNP0C50')),
			E.name('_UID', 0),
			E.method('_STA', 0, [E.ret(0x0F)]),
			E.name('_CRS', E.buffer(E.resource_template(E.i2c_serial_bus_v2(address, i2cb, speed=400000), E.gpio_int([line], gpio, edge=False, active_low=True)))),
			E.dsm(HID_OVER_I2C, {1: [E.ret(register)]}),
		]))
	return [E.scope(i2cb, devices)]


def hid_ssdt():
	return E.table('SSDT', hid_body(), oem_table_id=b'LIBHIDF ')


# The backend's port controller (`vhost-i2c-gpio.py --tcpc`): its address and its alert line.
TCPC_ADDRESS = 0x52
TCPC_LINE = 5
# THE BOARD: Fixed 5 V at 3 A and 15 V at 2 A - less at 15 V than the gate's charger offers - operating at 15 W.
TCPC_SINK_PDOS = [(5000 // 50) << 10 | 3000 // 10, (15000 // 50) << 10 | 2000 // 10]
TCPC_OPERATING_MICROWATTS = 15_000_000


def tcpc_body():
	i2cb = f'\\_SB.PCI0.S{I2C_SLOT << 3:02X}'
	gpio = f'\\_SB.PCI0.S{GPIO_SLOT << 3:02X}'
	return [E.scope(i2cb, [E.device('TCPC', [
		E.name('_HID', E.string('PRP0001')),
		E.name('_UID', 0),
		E.method('_STA', 0, [E.ret(0x0F)]),
		E.name('_CRS', E.buffer(E.resource_template(E.i2c_serial_bus_v2(TCPC_ADDRESS, i2cb, speed=400000), E.gpio_int([TCPC_LINE], gpio, edge=False, active_low=True)))),
		E.name('_DSD', E.dsd({'compatible': E.string('tcpci')}, [('connector', 'CON0')])),
		E.name('CON0', E.data_node({'compatible': E.string('usb-c-connector'), 'power-role': E.string('sink'), 'sink-pdos': E.package(*TCPC_SINK_PDOS), 'op-sink-microwatt': TCPC_OPERATING_MICROWATTS})),
	])])]


def tcpc_ssdt():
	return E.table('SSDT', tcpc_body(), oem_table_id=b'LIBTCPC ')


# WHERE EACH NAMED UNIT OF THE HARNESS PAGES' FIELDS SITS, at the byte offset the host's readers and writers use. A field
# list places its units RELATIVELY - a reserved run is a length, not a position - so a list that names a position where
# it means a length moves every unit after it, and the methods read bytes nobody writes: the TAD's `_GCP` once read
# zero that way, and its driver said the device had no clock. The self-test decodes every `HPGS` list of each table and
# holds each unit to this.
PAGE_UNITS = {
	'VAL0': VALUE_OFFSET, 'PRS0': PRESENT_OFFSETS[0], 'PRS1': PRESENT_OFFSETS[1], 'BSTA': BATTERY_STA, 'BSTS': BATTERY_BST,
	'BRAT': BATTERY_BST + 4, 'BREM': BATTERY_BST + 8, 'BVOL': BATTERY_BST + 12, 'APSR': AC_PSR, 'TTMP': ZONE_TMP,
	**{name: POWER_PAGE + at for at, name in enumerate(('PSST', 'PSON', 'PSOF', 'PWST', 'PWON', 'PWOF', 'LIDP', 'TADP'))},
	'SLEV': SLEEP_EVENTS, 'LIDO': LID_OPEN, 'TGCP': TAD_GCP, 'TACT': TAD_AC_TIMER, 'TDCT': TAD_DC_TIMER, 'TAST': TAD_AC_STATUS,
	'TDST': TAD_DC_STATUS, 'STVN': TAD_STV_CALLS, 'TGRT': TAD_GRT,
	'PPCV': PROCESSOR_PPC, 'F0LV': FAN_PAGE, 'F0LN': FAN_PAGE + 4, 'F1LV': FAN_PAGE + 8, 'F1LN': FAN_PAGE + 12, 'SCPM': ZONE_SCP,
	'SCPN': ZONE_SCP + 1,
	'UCPR': UCSI_PRESENT, 'ODBL': UCSI_DOORBELL, 'LOG2': UCSI_REFRESHES, 'NCNT': UCSI_NOTIFIES, 'OCTL': UCSI_CONTROL,
	'IVER': UCSI_VERSION, 'ICCI': UCSI_CCI, 'OMSG': UCSI_MESSAGE_OUT, 'IMSG': UCSI_MESSAGE_IN,
	'BLVL': BRIGHTNESS_LEVEL, 'BCMN': BRIGHTNESS_CALLS, 'DOSV': DOS_VALUE, 'DOSN': DOS_CALLS, 'BEVT': BRIGHTNESS_EVENT, 'ALSI': ILLUMINANCE,
}


def page_units(table):
	"""Every named unit of every `HPGS` field list in `table`, as (name, byte offset, bit remainder) in table order."""
	def length(at):
		lead = table[at]
		if lead >> 6 == 0:
			return lead & 0x3F, at + 1
		value = lead & 0x0F
		for i in range(lead >> 6):
			value |= table[at + 1 + i] << (4 + 8 * i)
		return value, at + 1 + (lead >> 6)

	units = []
	start = table.find(b'\x5b\x81', 36)
	while start >= 0:
		size, body = length(start + 2)
		end = start + 2 + size
		if table[body:body + 4] == b'HPGS' and end <= len(table):
			at, bit = body + 5, 0
			while at < end:
				if table[at] == 0x00:
					bits, at = length(at + 1)
				elif table[at] in (0x01, 0x03):
					at, bits = at + (3 if table[at] == 0x01 else 4), 0
				else:
					name = table[at:at + 4].decode('ascii', 'replace')
					bits, at = length(at + 4)
					units.append((name, bit // 8, bit % 8))
				bit += bits
		start = table.find(b'\x5b\x81', start + 2)
	return units


def grt_bytes(unix):
	"""`_GRT`'s sixteen bytes for a Unix time: UTC, the zone unspecified, valid."""
	import datetime
	at = datetime.datetime.fromtimestamp(unix, tz=datetime.timezone.utc)
	return struct.pack('<HBBBBBBHhB3x', at.year, at.month, at.day, at.hour, at.minute, at.second, 1, 0, 2047, 0)


def create_memory(path, ucsi_version=None, sleep=False, processors=False, brightness=False):
	data = bytearray(MEMORY_SIZE)
	# THE PANEL AT ITS FIRMWARE DEFAULT, as firmware leaves it at power-on, and a dim room.
	if brightness:
		data[BRIGHTNESS_LEVEL] = BCL[0]
		struct.pack_into('<I', data, ILLUMINANCE, 10)
	# THE SLEEP DEVICES: the lid open, the TAD with its clock, both wake timers disabled, and its clock at the host's time
	# until the gate sets another.
	if sleep:
		import time
		data[LID_OPEN] = 1
		struct.pack_into('<6I', data, TAD_GCP, 0b111, TAD_DISABLED, TAD_DISABLED, 0, 0, 0)
		data[TAD_GRT:TAD_GRT + 16] = grt_bytes(int(time.time()))
		data[POWER_PAGE + 6:POWER_PAGE + 8] = b'\xff\xff'
	# A PPM THERE, when the harness runs one: the device present, and the VERSION function 2 copies at bind.
	if ucsi_version is not None:
		data[UCSI_PRESENT] = 1
		struct.pack_into('<H', data, UCSI_VERSION, ucsi_version)
	for offset in PRESENT_OFFSETS:
		data[offset] = 1
	data[BATTERY_STA] = BATTERY_FIRST[0]
	struct.pack_into('<4I', data, BATTERY_BST, *BATTERY_FIRST[1:])
	data[AC_PSR] = AC_FIRST
	struct.pack_into('<I', data, ZONE_TMP, ZONE_FIRST)
	with open(path, 'wb') as out:
		out.write(data)


def poke(path, offset, data):
	with open(path, 'r+b') as handle:
		handle.seek(offset)
		handle.write(data)
		handle.flush()
		os.fsync(handle.fileno())


def set_values(path, pairs):
	fields = {**POWER_FIELDS, **PROCESSOR_FIELDS}
	for pair in pairs:
		name, _, value = pair.partition('=')
		if name not in fields or not value:
			raise SystemExit(f'acpi-fixture: {pair!r} is not NAME=VALUE with a NAME of {", ".join(fields)}')
		offset, size = fields[name]
		poke(path, offset, int(value, 0).to_bytes(size, 'little'))


def power_storm(path, control):
	"""THE HOST'S HALF OF THE STORM: the line lowered and raised as fast as the backend answers, each rise preceded by a
	new zone reading; an event fires only where the guest has its line armed, and those are counted. Then the last
	reading, and the line raised until an event follows it - so a notification always comes after the last write."""
	import socket
	import time
	sock = socket.socket(socket.AF_UNIX)
	sock.connect(control)
	sock.settimeout(5)

	def ask(text):
		sock.sendall((text + '\n').encode())
		return sock.recv(4096).decode()

	offset, _ = POWER_FIELDS['zone-temperature']
	fired = toggles = 0
	with open(path, 'r+b', buffering=0) as handle:
		deadline = time.monotonic() + STORM_SECONDS
		while fired < STORM_EVENTS and time.monotonic() < deadline:
			handle.seek(offset)
			handle.write(struct.pack('<I', 3100 + toggles % 100))
			ask(f'lower {POWER_LINE}')
			if 'fired' in ask(f'raise {POWER_LINE}'):
				fired += 1
			toggles += 1
			time.sleep(0.002)
		handle.seek(offset)
		handle.write(struct.pack('<I', STORM_FINAL))
		deadline = time.monotonic() + 10
		while True:
			ask(f'lower {POWER_LINE}')
			if 'fired' in ask(f'raise {POWER_LINE}'):
				fired += 1
				break
			if time.monotonic() >= deadline:
				raise SystemExit('acpi-fixture: no event followed the last reading')
			time.sleep(0.02)
	print(f'acpi-fixture: the storm raised the line {toggles + 1} or more times and {fired} events fired')
	return 0


def sleep_event(path, control, name):
	"""ONE SLEEP-DEVICE EVENT: the pages written, then line SLEEP_LINE lowered and raised until the backend says an event
	fired - an event fires only where the guest has its line armed."""
	import socket
	import time
	if name not in SLEEP_EVENT_BITS:
		raise SystemExit(f'acpi-fixture: {name!r} is none of {", ".join(SLEEP_EVENT_BITS)}')
	if name.startswith('lid-'):
		poke(path, LID_OPEN, bytes([1 if name == 'lid-open' else 0]))
	poke(path, SLEEP_EVENTS, bytes([SLEEP_EVENT_BITS[name]]))
	sock = socket.socket(socket.AF_UNIX)
	sock.connect(control)
	sock.settimeout(5)

	def ask(text):
		sock.sendall((text + '\n').encode())
		return sock.recv(4096).decode()

	deadline = time.monotonic() + 10
	while True:
		ask(f'lower {SLEEP_LINE}')
		if 'fired' in ask(f'raise {SLEEP_LINE}'):
			print(f'acpi-fixture: {name} raised')
			return 0
		if time.monotonic() >= deadline:
			raise SystemExit(f'acpi-fixture: no event fired for {name}')
		time.sleep(0.05)


def brightness_read(path):
	"""THE PANEL'S LEVEL AND `_BCM` COUNT, `_DOS`'S LAST ARGUMENT AND COUNT, AND THE ILLUMINANCE."""
	import json
	with open(path, 'rb') as handle:
		data = handle.read()
	print(json.dumps({'level': data[BRIGHTNESS_LEVEL], 'bcm_calls': data[BRIGHTNESS_CALLS], 'dos': data[DOS_VALUE], 'dos_calls': data[DOS_CALLS], 'illuminance': struct.unpack_from('<I', data, ILLUMINANCE)[0]}))
	return 0


def brightness_event(path, control, value):
	"""ONE BRIGHTNESS-GATE NOTIFICATION: the value written into the event byte, then line BRIGHTNESS_LINE raised until an
	event fires - 0x80 to the sensor, 0x85 to 0x89 to the panel."""
	poke(path, BRIGHTNESS_EVENT, bytes([value]))
	return raise_line(control, BRIGHTNESS_LINE)


def processor_read(path):
	"""WHAT THE KERNEL AND THE FIRMWARE WROTE: `_PCT`'s control and status, `_CPC`'s desired performance and preference,
	`_PPC`, each fan's level and `_FSL` count, and `_SCP`'s mode and count."""
	import json
	with open(path, 'rb') as handle:
		data = handle.read()
	dword = lambda offset: struct.unpack_from('<I', data, offset)[0]
	print(json.dumps({
		'pct_control': dword(PCT_CONTROL), 'pct_status': dword(PCT_STATUS), 'cpc_desired': dword(CPC_DESIRED), 'cpc_preference': dword(CPC_PREFERENCE),
		'ppc': data[PROCESSOR_PPC],
		'fan0': dword(FAN_PAGE), 'fan0_calls': dword(FAN_PAGE + 4), 'fan1': dword(FAN_PAGE + 8), 'fan1_calls': dword(FAN_PAGE + 12),
		'scp': data[ZONE_SCP], 'scp_calls': data[ZONE_SCP + 1],
	}))
	return 0


def raise_line(control, line):
	"""LINE `line` lowered and raised until the backend says an event fired - it fires only where the guest armed it."""
	import socket
	import time
	sock = socket.socket(socket.AF_UNIX)
	sock.connect(control)
	sock.settimeout(5)

	def ask(text):
		sock.sendall((text + '\n').encode())
		return sock.recv(4096).decode()

	deadline = time.monotonic() + 10
	while True:
		ask(f'lower {line}')
		if 'fired' in ask(f'raise {line}'):
			print(f'acpi-fixture: line {line} raised')
			return 0
		if time.monotonic() >= deadline:
			raise SystemExit(f'acpi-fixture: no event fired on line {line}')
		time.sleep(0.05)


def tad_read(path):
	import json
	with open(path, 'rb') as handle:
		handle.seek(TAD_AC_TIMER)
		ac, dc, ac_status, dc_status, calls = struct.unpack('<5I', handle.read(20))
	print(json.dumps({'ac_timer': ac, 'dc_timer': dc, 'ac_status': ac_status, 'dc_status': dc_status, 'stv_calls': calls}))
	return 0


def power_read(path):
	import json
	with open(path, 'rb') as handle:
		handle.seek(POWER_PAGE)
		values = handle.read(len(POWER_BYTES))
	print(json.dumps(dict(zip(POWER_BYTES, values))))
	return 0


def self_test():
	table = ssdt()
	failures = []
	if sum(table) & 0xFF:
		failures.append('the checksum')
	if struct.unpack('<I', table[4:8])[0] != len(table):
		failures.append('the length')
	for needle in (b'LSFX0001', b'LSFX0002', b'LSFX0003', b'_AEI', b'_E02', b'_E03', b'_E04', b'SA0_', b'GSB0', b'ACPI0003', b'BAT0', b'_BIX', b'_BST', b'TZ00', b'_TZ_', b'USBC000', b'UCSI', b'MBOX', b'ODBL', b'LOG2'):
		if needle not in table:
			failures.append(f'{needle!r} is missing')
	hid = hid_ssdt()
	if sum(hid) & 0xFF or struct.unpack('<I', hid[4:8])[0] != len(hid):
		failures.append('the HID table\'s checksum or length')
	for needle in (b'LSFX0C50', b'LSFX0C51', b'PNP0C50', b'SA8_', b'_DSM', E.i2c_serial_bus_v2(0x2C, f'\\_SB.PCI0.S{I2C_SLOT << 3:02X}', speed=400000), E.gpio_int([1], f'\\_SB.PCI0.S{GPIO_SLOT << 3:02X}', edge=False, active_low=True)):
		if needle not in hid:
			failures.append(f'the HID table lacks {needle!r}')
	sleep = ssdt(sleep=True)
	if sum(sleep) & 0xFF or struct.unpack('<I', sleep[4:8])[0] != len(sleep):
		failures.append('the sleep table\'s checksum or length')
	for needle in (b'LID0', b'PWRB', b'SLPB', b'TAD0', b'ACPI000E', b'_LID', b'_GRT', b'_STV', b'_GWS', b'_CWS', b'_E06', b'SLEV', b'PSLP', b'PWAK', b'_PR0', b'_S3W', b'_PS3', E.power_resource('PSLP', 0, 0, [])[:2]):
		if needle not in sleep:
			failures.append(f'the sleep table lacks {needle!r}')
		if needle in table and needle not in (b'SLEV',):
			failures.append(f'the plain table carries the sleep gate\'s {needle!r}')
	if len(grt_bytes(0)) != 16:
		failures.append('_GRT is not sixteen bytes')
	processors = ssdt(processors=True)
	if sum(processors) & 0xFF or struct.unpack('<I', processors[4:8])[0] != len(processors):
		failures.append('the processor table\'s checksum or length')
	for needle in (b'C000', b'C001', b'C002', b'_PSS', b'_PCT', b'_PPC', b'_PSD', b'_CPC', b'_LPI', b'_PSL', b'_AL0', b'_TC1', b'_TC2', b'_TSP', b'_SCP', b'FAN0', b'FAN1', b'_FIF', b'_FPS', b'_FSL', b'_FST', b'_E07', E.generic_register(SPACE_FIXED_HARDWARE, 64, 0x199)):
		if needle not in processors:
			failures.append(f'the processor table lacks {needle!r}')
		if needle in table and needle not in (b'C000',):
			failures.append(f'the plain table carries the processor gate\'s {needle!r}')
	brightness = ssdt(brightness=True)
	if sum(brightness) & 0xFF or struct.unpack('<I', brightness[4:8])[0] != len(brightness):
		failures.append('the brightness table\'s checksum or length')
	for needle in (b'S08_', b'_DOS', b'_DOD', b'LCD0', b'_BCL', b'_BCM', b'_BQC', b'ALS0', b'ACPI0008', b'_ALI', b'_ALR', b'_E08', b'BEVT'):
		if needle not in brightness:
			failures.append(f'the brightness table lacks {needle!r}')
		if needle in table and needle not in (b'BEVT',):
			failures.append(f'the plain table carries the brightness gate\'s {needle!r}')
	no_dos = ssdt(brightness=True, brightness_no_dos=True)
	if b'_DOS' in no_dos or any(needle not in no_dos for needle in (b'_DOD', b'_BCL', b'_BCM', b'_BQC')) or sum(no_dos) & 0xff:
		failures.append('the no-_DOS adapter must retain _DOD and the complete panel')
	if b'FAN0' not in sleep:
		failures.append('the sleep table lacks the fans')
	tcpc = tcpc_ssdt()
	if sum(tcpc) & 0xFF or struct.unpack('<I', tcpc[4:8])[0] != len(tcpc):
		failures.append('the TCPCI table\'s checksum or length')
	for needle in (b'PRP0001', b'TCPC', b'CON0', b'tcpci', b'usb-c-connector', b'sink-pdos', b'op-sink-microwatt', E.i2c_serial_bus_v2(TCPC_ADDRESS, f'\\_SB.PCI0.S{I2C_SLOT << 3:02X}', speed=400000), E.gpio_int([TCPC_LINE], f'\\_SB.PCI0.S{GPIO_SLOT << 3:02X}', edge=False, active_low=True)):
		if needle not in tcpc:
			failures.append(f'the TCPCI table lacks {needle!r}')
	# EVERY UNIT OF THE HARNESS PAGES WHERE THE HOST READS AND WRITES IT, in each table.
	placed = 0
	for label, built in (('plain', table), ('sleep', sleep), ('processor', processors), ('brightness', brightness)):
		for name, offset, remainder in page_units(built):
			placed += 1
			if name not in PAGE_UNITS:
				failures.append(f'the {label} table\'s harness pages name {name}, which PAGE_UNITS does not place')
			elif (offset, remainder) != (PAGE_UNITS[name], 0):
				failures.append(f'the {label} table places {name} at {offset:#x} (+{remainder} bits), not at {PAGE_UNITS[name]:#x}')
	if not {name for name, _, _ in page_units(sleep)} >= {'TGCP', 'TGRT', 'SLEV', 'PSST'}:
		failures.append('the sleep table\'s harness pages were not decoded')
	if failures:
		for failure in failures:
			print(f'acpi-fixture: {failure}', file=sys.stderr)
		return 1
	print(f'acpi-fixture: the SSDT builds ({len(table)} bytes), {placed} harness-page units in place')
	return 0


def main():
	parser = argparse.ArgumentParser(description='the ACPI gate fixture SSDT')
	parser.add_argument('--out')
	parser.add_argument('--hid-out')
	parser.add_argument('--tcpc-out')
	parser.add_argument('--memory')
	parser.add_argument('--poke', nargs=3, metavar=('FILE', 'OFFSET', 'HEX'))
	parser.add_argument('--set', nargs='+', metavar='FILE NAME=VALUE')
	parser.add_argument('--power-storm', nargs=2, metavar=('FILE', 'CONTROL'))
	parser.add_argument('--ucsi-version', type=lambda text: int(text, 0), help='a UCSI PPM is present, with this VERSION (0x0120, 0x0210)')
	parser.add_argument('--sleep', action='store_true', help='the table and the memory carry the sleep gate\'s devices')
	parser.add_argument('--processors', action='store_true', help='the table carries the processor-power gate\'s processors, fans and zone objects')
	parser.add_argument('--processor-read', metavar='FILE')
	parser.add_argument('--brightness', action='store_true', help='the table and the memory carry the brightness gate\'s panel, its adapter\'s methods and the light sensor')
	parser.add_argument('--brightness-no-dos', action='store_true', help='omit only the brightness adapter _DOS method; requires --brightness')
	parser.add_argument('--brightness-read', metavar='FILE')
	parser.add_argument('--brightness-event', nargs=3, metavar=('FILE', 'CONTROL', 'VALUE'))
	parser.add_argument('--brightness-set', nargs='+', metavar='FILE NAME=VALUE')
	parser.add_argument('--raise', nargs=2, metavar=('CONTROL', 'LINE'), dest='raise_')
	parser.add_argument('--sleep-event', nargs=3, metavar=('FILE', 'CONTROL', 'NAME'))
	parser.add_argument('--tad-clock', nargs=2, metavar=('FILE', 'UNIX'))
	parser.add_argument('--tad-read', metavar='FILE')
	parser.add_argument('--power-read', metavar='FILE')
	parser.add_argument('--self-test', action='store_true')
	args = parser.parse_args()
	if args.brightness_no_dos and not args.brightness:
		parser.error('--brightness-no-dos requires --brightness')
	if args.self_test:
		return self_test()
	if args.out:
		with open(args.out, 'wb') as out:
			out.write(ssdt(args.ucsi_version or 0x0210, args.sleep, args.processors, args.brightness, args.brightness_no_dos))
	if args.hid_out:
		with open(args.hid_out, 'wb') as out:
			out.write(hid_ssdt())
	if args.tcpc_out:
		with open(args.tcpc_out, 'wb') as out:
			out.write(tcpc_ssdt())
	if args.memory:
		create_memory(args.memory, args.ucsi_version, args.sleep, args.processors, args.brightness)
	if args.poke:
		poke(args.poke[0], int(args.poke[1], 0), bytes.fromhex(args.poke[2]))
	if args.set:
		set_values(args.set[0], args.set[1:])
	if args.power_storm:
		return power_storm(*args.power_storm)
	if args.sleep_event:
		return sleep_event(*args.sleep_event)
	if args.tad_clock:
		poke(args.tad_clock[0], TAD_GRT, grt_bytes(int(args.tad_clock[1], 0)))
	if args.tad_read:
		return tad_read(args.tad_read)
	if args.power_read:
		return power_read(args.power_read)
	if args.processor_read:
		return processor_read(args.processor_read)
	if args.brightness_read:
		return brightness_read(args.brightness_read)
	if args.brightness_event:
		return brightness_event(args.brightness_event[0], args.brightness_event[1], int(args.brightness_event[2], 0))
	if args.brightness_set:
		for pair in args.brightness_set[1:]:
			name, _, value = pair.partition('=')
			if name not in BRIGHTNESS_FIELDS or not value:
				raise SystemExit(f'acpi-fixture: {pair!r} is not NAME=VALUE with a NAME of {", ".join(BRIGHTNESS_FIELDS)}')
			offset, size = BRIGHTNESS_FIELDS[name]
			poke(args.brightness_set[0], offset, int(value, 0).to_bytes(size, 'little'))
	if args.raise_:
		return raise_line(args.raise_[0], int(args.raise_[1], 0))
	return 0


if __name__ == '__main__':
	sys.exit(main())
