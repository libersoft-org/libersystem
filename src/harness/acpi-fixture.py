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
#   THE NOTIFICATION. `_AEI` on the `vhost-user-gpio-pci` function's line 2 (the backend's `acpi-aei` line), through
#   the GPIO controller's companion node `\_SB.PCI0.SB0`, whose `_E02` notifies `LSFX0001` with 0x80.
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
# THE HARNESS'S PAGES: 0x00 the dword `RDVL` answers, 0x10 and 0x11 the `_STA` bytes of `_UID` 0 and 1.
#
# THE HID-OVER-I2C GATE'S SSDT (`--hid-out`), a table of its own: below the I2C controller's companion node, the
# backend's two HID models as laptops describe theirs - a vendor `_HID` with `_CID` `PNP0C50`, the `I2cSerialBusV2`
# address, a `GpioInt` (level, active low) naming the model's line on the GPIO controller's node, and a `_DSM` answering
# the HID descriptor register.
#
#   acpi-fixture.py --out FILE         write the SSDT
#   acpi-fixture.py --hid-out FILE     write the HID-over-I2C SSDT
#   acpi-fixture.py --memory FILE      create the ivshmem backing file (1 MiB) with both devices present
#   acpi-fixture.py --poke FILE OFFSET BYTES-HEX   write bytes into the backing file
#   acpi-fixture.py --self-test        build the table and check it, writing nothing

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
FIELD_LINE = 5
I2C_ADDRESS = 0x50
I2C_REGISTER = 0x10
HARNESS_PAGES = 0x2000
DEVICE_RANGE = 0x4000
MEMORY_SIZE = 1 << 20
VALUE_OFFSET = 0x00
PRESENT_OFFSETS = (0x10, 0x11)
DSM_UUID = '5c3c6b2e-8d7a-4f5b-9a41-2e1d7f0a6b93'


def ssdt_body():
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
			E.field('HPGS', [E.unit('VAL0', 32), E.offset_to((PRESENT_OFFSETS[0] - 4) * 8), E.unit('PRS0', 8), E.unit('PRS1', 8)], access='AnyAcc'),
		]),
		E.scope(gpio, [
			E.name('_AEI', E.buffer(E.resource_template(E.gpio_int([AEI_LINE], gpio, edge=True)))),
			E.method(f'_E{AEI_LINE:02X}', 0, [E.notify('\\_SB.LSF1', 0x80)]),
		]),
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
		]),
	]


def ssdt():
	return E.table('SSDT', ssdt_body(), oem_table_id=b'LIBACPIF')


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


def create_memory(path):
	data = bytearray(MEMORY_SIZE)
	for offset in PRESENT_OFFSETS:
		data[offset] = 1
	with open(path, 'wb') as out:
		out.write(data)


def poke(path, offset, data):
	with open(path, 'r+b') as handle:
		handle.seek(offset)
		handle.write(data)
		handle.flush()
		os.fsync(handle.fileno())


def self_test():
	table = ssdt()
	failures = []
	if sum(table) & 0xFF:
		failures.append('the checksum')
	if struct.unpack('<I', table[4:8])[0] != len(table):
		failures.append('the length')
	for needle in (b'LSFX0001', b'LSFX0002', b'LSFX0003', b'_AEI', b'_E02', b'SA0_', b'GSB0'):
		if needle not in table:
			failures.append(f'{needle!r} is missing')
	hid = hid_ssdt()
	if sum(hid) & 0xFF or struct.unpack('<I', hid[4:8])[0] != len(hid):
		failures.append('the HID table\'s checksum or length')
	for needle in (b'LSFX0C50', b'LSFX0C51', b'PNP0C50', b'SA8_', b'_DSM', E.i2c_serial_bus_v2(0x2C, f'\\_SB.PCI0.S{I2C_SLOT << 3:02X}', speed=400000), E.gpio_int([1], f'\\_SB.PCI0.S{GPIO_SLOT << 3:02X}', edge=False, active_low=True)):
		if needle not in hid:
			failures.append(f'the HID table lacks {needle!r}')
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
	parser.add_argument('--memory')
	parser.add_argument('--poke', nargs=3, metavar=('FILE', 'OFFSET', 'HEX'))
	parser.add_argument('--self-test', action='store_true')
	args = parser.parse_args()
	if args.self_test:
		return self_test()
	if args.out:
		with open(args.out, 'wb') as out:
			out.write(ssdt())
	if args.hid_out:
		with open(args.hid_out, 'wb') as out:
			out.write(hid_ssdt())
	if args.memory:
		create_memory(args.memory)
	if args.poke:
		poke(args.poke[0], int(args.poke[1], 0), bytes.fromhex(args.poke[2]))
	return 0


if __name__ == '__main__':
	sys.exit(main())
