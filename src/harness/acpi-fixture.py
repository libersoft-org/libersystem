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
#   acpi-fixture.py --self-test        build the table and check it, writing nothing
#
# THE SLEEP GATE'S DEVICES (`--out FILE --sleep`, and `--memory FILE --sleep`), in the same table so their `Notify` has
# the GPIO controller's `_AEI` to come from - line SLEEP_LINE, whose `_E06` raises what the pages' event byte names:
#   PNP0C0D  (`\_SB.LID0`) - the lid: `_LID` the pages' byte, and `_PRW`.
#   PNP0C0C  (`\_SB.PWRB`) - a control-method power button, and `_PRW`.
#   PNP0C0E  (`\_SB.SLPB`) - a control-method sleep button, and `_PRW`.
#   ACPI000E (`\_SB.TAD0`) - a Time and Alarm Device: `_GCP` from the pages, `_GRT` the pages' sixteen bytes - the harness
#            keeps them running - and `_STV`, `_TIV`, `_GWS` and `_CWS` over the pages' timers, `_STV` counted.
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


def ssdt_body(ucsi_version=0x0210, sleep=False):
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
				E.offset_to(SLEEP_EVENTS * 8), E.unit('SLEV', 8), E.unit('LIDO', 8), E.offset_to(TAD_GCP * 8), E.unit('TGCP', 32),
				E.unit('TACT', 32), E.unit('TDCT', 32), E.unit('TAST', 32), E.unit('TDST', 32), E.unit('STVN', 32),
				E.offset_to(TAD_GRT * 8), E.unit('TGRT', 128),
			], access='AnyAcc'),
			# THE UCSI STAGING AREAS AND COUNTERS.
			E.field('HPGS', [
				E.offset_to(UCSI_PRESENT * 8), E.unit('UCPR', 8), E.offset_to((UCSI_DOORBELL - UCSI_PRESENT - 1) * 8),
				E.unit('ODBL', 32), E.unit('LOG2', 32), E.unit('NCNT', 32), E.unit('OCTL', 64), E.unit('IVER', 16), E.offset_to(16), E.unit('ICCI', 32),
				E.offset_to((UCSI_MESSAGE_OUT - UCSI_CCI - 4) * 8), E.unit('OMSG', 2048), E.unit('IMSG', 2048),
			], access='AnyAcc'),
		]),
		E.scope(gpio, [
			E.name('_AEI', E.buffer(E.resource_template(*[E.gpio_int([line], gpio, edge=True) for line in [AEI_LINE, POWER_LINE, UCSI_LINE] + ([SLEEP_LINE] if sleep else [])]))),
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
		] if sleep else [])),
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
		] + (sleep_devices(ivsh) if sleep else [])),
		E.scope('\\_TZ', [
			E.thermal_zone('TZ00', [
				E.method('_TMP', 0, [E.ret(ivsh + '.TTMP')]),
				E.name('_CRT', 3682),
				E.name('_HOT', 3632),
				E.name('_PSV', 3532),
				E.name('_AC0', 3432),
			]),
		]),
	]


def ssdt(ucsi_version=0x0210, sleep=False):
	return E.table('SSDT', ssdt_body(ucsi_version, sleep), oem_table_id=b'LIBACPIF')


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


def grt_bytes(unix):
	"""`_GRT`'s sixteen bytes for a Unix time: UTC, the zone unspecified, valid."""
	import datetime
	at = datetime.datetime.fromtimestamp(unix, tz=datetime.timezone.utc)
	return struct.pack('<HBBBBBBHhB3x', at.year, at.month, at.day, at.hour, at.minute, at.second, 1, 0, 2047, 0)


def create_memory(path, ucsi_version=None, sleep=False):
	data = bytearray(MEMORY_SIZE)
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
	for pair in pairs:
		name, _, value = pair.partition('=')
		if name not in POWER_FIELDS or not value:
			raise SystemExit(f'acpi-fixture: {pair!r} is not NAME=VALUE with a NAME of {", ".join(POWER_FIELDS)}')
		offset, size = POWER_FIELDS[name]
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
	tcpc = tcpc_ssdt()
	if sum(tcpc) & 0xFF or struct.unpack('<I', tcpc[4:8])[0] != len(tcpc):
		failures.append('the TCPCI table\'s checksum or length')
	for needle in (b'PRP0001', b'TCPC', b'CON0', b'tcpci', b'usb-c-connector', b'sink-pdos', b'op-sink-microwatt', E.i2c_serial_bus_v2(TCPC_ADDRESS, f'\\_SB.PCI0.S{I2C_SLOT << 3:02X}', speed=400000), E.gpio_int([TCPC_LINE], f'\\_SB.PCI0.S{GPIO_SLOT << 3:02X}', edge=False, active_low=True)):
		if needle not in tcpc:
			failures.append(f'the TCPCI table lacks {needle!r}')
	if failures:
		for failure in failures:
			print(f'acpi-fixture: {failure}', file=sys.stderr)
		return 1
	print(f'acpi-fixture: the SSDT builds ({len(table)} bytes)')
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
	parser.add_argument('--sleep-event', nargs=3, metavar=('FILE', 'CONTROL', 'NAME'))
	parser.add_argument('--tad-clock', nargs=2, metavar=('FILE', 'UNIX'))
	parser.add_argument('--tad-read', metavar='FILE')
	parser.add_argument('--power-read', metavar='FILE')
	parser.add_argument('--self-test', action='store_true')
	args = parser.parse_args()
	if args.self_test:
		return self_test()
	if args.out:
		with open(args.out, 'wb') as out:
			out.write(ssdt(args.ucsi_version or 0x0210, args.sleep))
	if args.hid_out:
		with open(args.hid_out, 'wb') as out:
			out.write(hid_ssdt())
	if args.tcpc_out:
		with open(args.tcpc_out, 'wb') as out:
			out.write(tcpc_ssdt())
	if args.memory:
		create_memory(args.memory, args.ucsi_version, args.sleep)
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
	return 0


if __name__ == '__main__':
	sys.exit(main())
