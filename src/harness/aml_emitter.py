#!/usr/bin/env python3
# THE HARNESS'S AML EMITTER: complete, checksummed ACPI tables built from Python, for the fixture SSDTs QEMU is given
# with `-acpitable` - no `iasl` on this machine, and no gate may depend on a host package.
#
# Each helper answers the bytes one ASL construct compiles to, so a fixture reads like the ASL it stands for:
#
#   ssdt = table('SSDT', [
#       scope('\\_SB', [
#           device('FIX0', [
#               name('_HID', string('LSFX0001')),
#               name('_UID', 0),
#               method('_STA', 0, [ret(0x0F)]),
#           ]),
#       ]),
#   ])
#
# What it covers is what the fixtures need: devices with `_HID` or `_ADR`, `Scope` into a node QEMU's DSDT has,
# names, methods with control flow, arithmetic and stores of their arguments into fields, operation regions and
# fields - a GeneralPurposeIo or GenericSerialBus field's `Connection` included - mutexes, packages, `Notify`, a
# `_DSM` dispatching on `ToUUID` and function, a `_CRS` method that patches a `Memory32Fixed` base, a `PRP0001`
# `_DSD` with a hierarchical data-node child, and the resource descriptors down to `GpioInt`, `GpioIo` and
# `I2cSerialBusV2`.
#
#   aml_emitter.py --self-test     checks the encodings and writes nothing
import struct
import sys
import uuid as uuid_module

# ------------------------------------------------------------------ encoding primitives


def pkg_length(body):
	"""The package length prefixed to `body`: it counts its own bytes."""
	for extra in range(4):
		total = len(body) + 1 + extra
		if (extra == 0 and total < 0x40) or (extra == 1 and total < 0x1000) or (extra == 2 and total < 0x100000) or (extra == 3 and total < 0x10000000):
			if extra == 0:
				return bytes([total]) + body
			out = bytes([(extra << 6) | (total & 0x0F)])
			for i in range(extra):
				out += bytes([(total >> (4 + 8 * i)) & 0xFF])
			return out + body
	raise ValueError('a package too large to encode')


def field_length(bits):
	"""A field or reserved length: the package-length encoding of a plain number."""
	if bits < 0x40:
		return bytes([bits])
	for extra in range(1, 4):
		if bits < 1 << (4 + 8 * extra):
			out = bytes([(extra << 6) | (bits & 0x0F)])
			for i in range(extra):
				out += bytes([(bits >> (4 + 8 * i)) & 0xFF])
			return out
	raise ValueError('a field too long to encode')


def _seg(text):
	if not 1 <= len(text) <= 4:
		raise ValueError(f'{text!r} is not a name segment')
	padded = text.ljust(4, '_')
	if not (padded[0] == '_' or padded[0].isupper()) or not all(c == '_' or c.isupper() or c.isdigit() for c in padded[1:]):
		raise ValueError(f'{text!r} is not a name segment')
	return padded.encode()


def namestring(text):
	"""`\\_SB.PCI0`, `^PARN`, `FOO` as a name string."""
	out = b''
	rest = text
	if rest.startswith('\\'):
		out += b'\\'
		rest = rest[1:]
	while rest.startswith('^'):
		out += b'^'
		rest = rest[1:]
	segs = rest.split('.') if rest else []
	if not segs:
		return out + b'\x00'
	if len(segs) == 1:
		return out + _seg(segs[0])
	if len(segs) == 2:
		return out + b'\x2e' + _seg(segs[0]) + _seg(segs[1])
	return out + b'\x2f' + bytes([len(segs)]) + b''.join(_seg(s) for s in segs)


class Raw(bytes):
	"""Bytes that are already AML - an operand passed through as it is."""


def term(value):
	"""An operand: an int is a constant, a str a name, bytes already AML."""
	if isinstance(value, bytes):
		return bytes(value)
	if isinstance(value, bool):
		return integer(int(value))
	if isinstance(value, int):
		return integer(value)
	if isinstance(value, str):
		return namestring(value)
	raise TypeError(f'{value!r} is not an operand')


def integer(value):
	if value == 0:
		return b'\x00'
	if value == 1:
		return b'\x01'
	if value == 0xFFFFFFFFFFFFFFFF:
		return b'\xff'
	if value <= 0xFF:
		return b'\x0a' + struct.pack('<B', value)
	if value <= 0xFFFF:
		return b'\x0b' + struct.pack('<H', value)
	if value <= 0xFFFFFFFF:
		return b'\x0c' + struct.pack('<I', value)
	return b'\x0e' + struct.pack('<Q', value)


def string(text):
	return Raw(b'\x0d' + text.encode('ascii') + b'\x00')


def buffer(data, size=None):
	data = bytes(data)
	body = integer(len(data) if size is None else size) + data
	return Raw(b'\x11' + pkg_length(body))


def package(*elements):
	body = bytes([len(elements)]) + b''.join(term(e) for e in elements)
	return Raw(b'\x12' + pkg_length(body))


def eisaid(text):
	"""`EISAID("PNP0A08")`."""
	value = 0
	for letter in text[:3]:
		value = (value << 5) | (ord(letter) - ord('@'))
	for digit in text[3:]:
		value = (value << 4) | int(digit, 16)
	return Raw(integer(int.from_bytes(value.to_bytes(4, 'big'), 'little')))


def to_uuid(text):
	"""`ToUUID("...")`: the buffer, the first three fields little-endian."""
	return buffer(uuid_module.UUID(text).bytes_le)


def local(n):
	return Raw(bytes([0x60 + n]))


def arg(n):
	return Raw(bytes([0x68 + n]))


DEBUG = Raw(b'\x5b\x31')
NULL = Raw(b'\x00')


def _list(terms):
	return b''.join(term(t) for t in terms)


# ------------------------------------------------------------------ named objects


def name(n, value):
	return Raw(b'\x08' + namestring(n) + term(value))


def method(n, args, body, serialized=False):
	flags = (args & 0x07) | (0x08 if serialized else 0)
	return Raw(b'\x14' + pkg_length(namestring(n) + bytes([flags]) + _list(body)))


def scope(n, body):
	return Raw(b'\x10' + pkg_length(namestring(n) + _list(body)))


def device(n, body):
	return Raw(b'\x5b\x82' + pkg_length(namestring(n) + _list(body)))


def thermal_zone(n, body):
	return Raw(b'\x5b\x85' + pkg_length(namestring(n) + _list(body)))


def mutex(n, sync_level=0):
	return Raw(b'\x5b\x01' + namestring(n) + bytes([sync_level]))


def external(n, kind, args=0):
	return Raw(b'\x15' + namestring(n) + bytes([kind, args]))


SPACES = {'SystemMemory': 0, 'SystemIO': 1, 'PCI_Config': 2, 'EmbeddedControl': 3, 'SystemCMOS': 5, 'GeneralPurposeIo': 8, 'GenericSerialBus': 9}
ACCESS = {'AnyAcc': 0, 'ByteAcc': 1, 'WordAcc': 2, 'DWordAcc': 3, 'QWordAcc': 4, 'BufferAcc': 5}
UPDATE = {'Preserve': 0, 'WriteAsOnes': 1, 'WriteAsZeros': 2}


def operation_region(n, space, offset, length):
	return Raw(b'\x5b\x80' + namestring(n) + bytes([SPACES[space]]) + term(offset) + term(length))


def field_flags(access='ByteAcc', lock=False, update='Preserve'):
	return ACCESS[access] | (0x10 if lock else 0) | (UPDATE[update] << 5)


def unit(n, bits):
	"""A named field unit of `bits` bits."""
	return Raw(_seg(n) + field_length(bits))


def offset_to(bits):
	"""A reserved run of `bits` bits - what `Offset` and an unnamed unit compile to."""
	return Raw(b'\x00' + field_length(bits))


def access_as(access, attrib=0, length=None):
	if length is None:
		return Raw(bytes([0x01, ACCESS[access], attrib]))
	return Raw(bytes([0x03, ACCESS[access], attrib, length]))


def connection(resource):
	"""`Connection (...)` over one descriptor, as a buffer."""
	return Raw(b'\x02' + buffer(resource))


def field(region, units, access='ByteAcc', lock=False, update='Preserve'):
	return Raw(b'\x5b\x81' + pkg_length(namestring(region) + bytes([field_flags(access, lock, update)]) + b''.join(bytes(u) for u in units)))


# ------------------------------------------------------------------ statements and expressions


def store(value, target):
	return Raw(b'\x70' + term(value) + term(target))


def _binary(op, a, b, target):
	return Raw(bytes([op]) + term(a) + term(b) + term(target))


def add(a, b, target=NULL):
	return _binary(0x72, a, b, target)


def subtract(a, b, target=NULL):
	return _binary(0x74, a, b, target)


def multiply(a, b, target=NULL):
	return _binary(0x77, a, b, target)


def band(a, b, target=NULL):
	return _binary(0x7B, a, b, target)


def bor(a, b, target=NULL):
	return _binary(0x7D, a, b, target)


def shift_left(a, b, target=NULL):
	return _binary(0x79, a, b, target)


def increment(target):
	return Raw(b'\x75' + term(target))


def lequal(a, b):
	return Raw(b'\x93' + term(a) + term(b))


def lless(a, b):
	return Raw(b'\x95' + term(a) + term(b))


def lnot(a):
	return Raw(b'\x92' + term(a))


def if_(predicate, body, else_body=None):
	out = b'\xa0' + pkg_length(term(predicate) + _list(body))
	if else_body is not None:
		out += b'\xa1' + pkg_length(_list(else_body))
	return Raw(out)


def while_(predicate, body):
	return Raw(b'\xa2' + pkg_length(term(predicate) + _list(body)))


def ret(value):
	return Raw(b'\xa4' + term(value))


def notify(target, value):
	return Raw(b'\x86' + term(target) + term(value))


def acquire(target, timeout=0xFFFF):
	return Raw(b'\x5b\x23' + term(target) + struct.pack('<H', timeout))


def release(target):
	return Raw(b'\x5b\x27' + term(target))


def index(source, at, target=NULL):
	return Raw(b'\x88' + term(source) + term(at) + term(target))


def deref(reference):
	return Raw(b'\x83' + term(reference))


def create_dword_field(source, at, n):
	return Raw(b'\x8a' + term(source) + term(at) + namestring(n))


def create_byte_field(source, at, n):
	return Raw(b'\x8c' + term(source) + term(at) + namestring(n))


def create_qword_field(source, at, n):
	return Raw(b'\x8f' + term(source) + term(at) + namestring(n))


def shift_right(a, b, target=NULL):
	return _binary(0x7A, a, b, target)


def call(n, *args):
	"""A method invocation."""
	return Raw(namestring(n) + b''.join(term(a) for a in args))


def dsm(uuid_text, functions):
	"""A `_DSM` that answers `functions` - {index: [statements]} - for one UUID, function 0 answering which
	functions exist, every other UUID answering a zero buffer."""
	mask = 1
	for index_ in functions:
		mask |= 1 << index_
	bitmap = mask.to_bytes((mask.bit_length() + 7) // 8, 'little')
	body = [if_(lequal(arg(2), 0), [ret(buffer(bitmap))])]
	for index_, statements in sorted(functions.items()):
		body.append(if_(lequal(arg(2), index_), statements))
	return method('_DSM', 4, [if_(lequal(arg(0), to_uuid(uuid_text)), body), ret(buffer([0]))])


DEVICE_PROPERTIES = 'daffd814-6eba-4d8c-8a91-bc9bbf4aa301'
HIERARCHICAL_DATA = 'dbb8e3e6-5886-4ba6-8795-1319f52a966b'


def dsd(properties, children=()):
	"""A `_DSD`: device properties as {key: value} and hierarchical children as (name, data-node-name) pairs."""
	pairs = [package(string(key), value) for key, value in properties.items()]
	elements = [to_uuid(DEVICE_PROPERTIES), package(*pairs)]
	if children:
		elements += [to_uuid(HIERARCHICAL_DATA), package(*[package(string(child), node) for child, node in children])]
	return package(*elements)


def data_node(properties):
	"""A hierarchical data node's package: device properties only."""
	return package(to_uuid(DEVICE_PROPERTIES), package(*[package(string(key), value) for key, value in properties.items()]))


# ------------------------------------------------------------------ resource descriptors


def resource_template(*descriptors):
	return b''.join(descriptors) + b'\x79\x00'


def memory32_fixed(base, length, writable=True):
	return struct.pack('<BHBII', 0x86, 9, 1 if writable else 0, base, length)


def qword_memory(base, length, writable=True):
	"""`QWordMemory (ResourceConsumer, ...)`: a 64-bit memory range the device consumes."""
	return struct.pack('<BHBBB', 0x8A, 43, 0, 1, 1 if writable else 0) + struct.pack('<QQQQQ', 0, base, base + length - 1 if length else base, 0, length)


def io(base, length):
	return struct.pack('<BBHHBB', 0x47, 0x01, base, base, 1, length)


def interrupt(lines, level=True, active_low=False, consumer=True):
	flags = (1 if consumer else 0) | (0 if level else 2) | (4 if active_low else 0)
	body = struct.pack('<BB', flags, len(lines)) + b''.join(struct.pack('<I', line) for line in lines)
	return struct.pack('<BH', 0x89, len(body)) + body


def _gpio(interrupt_, flags, pins, controller, pull=0, debounce=0):
	# THE GENERAL FLAGS' BIT 0 SAYS "THIS DEVICE CONSUMES THE LINE" - `ResourceConsumer`, what a device's own `_CRS` and
	# a field's connection both are.
	pin_offset = 23
	source_offset = pin_offset + 2 * len(pins)
	end = source_offset + len(controller) + 1
	body = struct.pack('<BBHHBHHHBHHH', 1, 0 if interrupt_ else 1, 1, flags, pull, 0, debounce, pin_offset, 0, source_offset, end, 0)
	body += b''.join(struct.pack('<H', pin) for pin in pins) + controller.encode() + b'\x00'
	return struct.pack('<BH', 0x8C, len(body)) + body


def gpio_int(pins, controller, edge=True, active_low=False, shared=False, wake=False):
	flags = (1 if edge else 0) | (2 if active_low else 0) | (8 if shared else 0) | (16 if wake else 0)
	return _gpio(True, flags, pins, controller)


def gpio_io(pins, controller, restriction='input'):
	flags = {'none': 0, 'input': 1, 'output': 2, 'preserve': 3}[restriction]
	return _gpio(False, flags, pins, controller)


def i2c_serial_bus_v2(address, controller, speed=100000, ten_bit=False):
	type_data = struct.pack('<IH', speed, address)
	body = struct.pack('<BBBBHBH', 2, 0, 1, 0x02, 1 if ten_bit else 0, 1, len(type_data)) + type_data + controller.encode() + b'\x00'
	return struct.pack('<BH', 0x8E, len(body)) + body


def crs_patched(base_name, length, writable=True):
	"""A `_CRS` METHOD that answers a `Memory32Fixed` whose base is read at run time from `base_name` - an ivshmem
	BAR's address a `PCI_Config` field reads."""
	template = resource_template(memory32_fixed(0, length, writable))
	return method('_CRS', 0, [
		name('RBUF', buffer(template)),
		create_dword_field('RBUF', 4, 'BASE'),
		store(band(base_name, 0xFFFFFFF0), 'BASE'),
		ret('RBUF'),
	], serialized=True)


def crs_patched64(base, length, writable=True):
	"""The same over a `QWordMemory`, for a BAR the firmware placed above 4 GiB: `base` an expression - a method's
	value - read at run time."""
	template = resource_template(qword_memory(0, length, writable))
	return method('_CRS', 0, [
		name('RBUF', buffer(template)),
		create_qword_field('RBUF', 14, 'MIN_'),
		create_qword_field('RBUF', 22, 'MAX_'),
		store(base, 'MIN_'),
		store(add('MIN_', length - 1), 'MAX_'),
		ret('RBUF'),
	], serialized=True)


# ------------------------------------------------------------------ tables


def table(signature, body, revision=2, oem_id=b'LIBER ', oem_table_id=b'LIBFIXTR', oem_revision=1):
	aml = _list(body)
	length = 36 + len(aml)
	header = signature.encode() + struct.pack('<IBB', length, revision, 0) + oem_id[:6].ljust(6, b' ') + oem_table_id[:8].ljust(8, b' ') + struct.pack('<I', oem_revision) + b'LIBR' + struct.pack('<I', 1)
	data = bytearray(header + aml)
	data[9] = (-sum(data)) & 0xFF
	return bytes(data)


# ------------------------------------------------------------------ self-test


def self_test():
	failures = []

	def check(what, got, want):
		if got != want:
			failures.append(f'{what}: {got!r} != {want!r}')

	check('a short package length counts itself', pkg_length(b'\x00' * 5), b'\x06' + b'\x00' * 5)
	long_body = b'\x00' * 100
	encoded = pkg_length(long_body)
	check('a long package length', (encoded[0] >> 6, (encoded[0] & 0x0F) | (encoded[1] << 4)), (1, 102))
	check('a dual name', namestring('_SB.PCI0'), b'\x2e_SB_PCI0')
	check('a root multi name', namestring('\\_SB.PCI0.S08'), b'\\\x2f\x03_SB_PCI0S08_')
	check('EISAID', eisaid('PNP0A08'), integer(0x080AD041))
	check('ToUUID', to_uuid(DEVICE_PROPERTIES)[3:3 + 1], b'\x10')
	check('a UUID laid out little-endian', bytes(to_uuid(DEVICE_PROPERTIES))[-16:][:4], bytes([0x14, 0xd8, 0xff, 0xda]))
	sample = table('SSDT', sample_body())
	check('the checksum', sum(sample) & 0xFF, 0)
	check('the length', struct.unpack('<I', sample[4:8])[0], len(sample))
	# THE CONNECTION DESCRIPTORS, FIELD BY FIELD AT THE SPECIFICATION'S OFFSETS (ACPI 6.5, 6.4.3.8.1 and 6.4.3.8.2.1).
	source = '\\_SB.PCI0.SA8_'
	i2c = i2c_serial_bus_v2(0x2C, source, speed=400000)
	check('I2cSerialBusV2: the tag', i2c[0], 0x8E)
	check('I2cSerialBusV2: the length after the header', struct.unpack('<H', i2c[1:3])[0], len(i2c) - 3)
	check('I2cSerialBusV2: revision 2', i2c[3], 2)
	check('I2cSerialBusV2: resource source index', i2c[4], 0)
	check('I2cSerialBusV2: serial bus type I2C', i2c[5], 1)
	check('I2cSerialBusV2: general flags - controller-initiated, consumer, exclusive', i2c[6], 0x02)
	check('I2cSerialBusV2: type flags - seven-bit', struct.unpack('<H', i2c[7:9])[0], 0)
	check('I2cSerialBusV2: ten-bit when asked', struct.unpack('<H', i2c_serial_bus_v2(0x2C, source, ten_bit=True)[7:9])[0], 1)
	check('I2cSerialBusV2: type revision', i2c[9], 1)
	check('I2cSerialBusV2: type data length', struct.unpack('<H', i2c[10:12])[0], 6)
	check('I2cSerialBusV2: connection speed', struct.unpack('<I', i2c[12:16])[0], 400000)
	check('I2cSerialBusV2: the address', struct.unpack('<H', i2c[16:18])[0], 0x2C)
	check('I2cSerialBusV2: the controller, NUL-terminated', i2c[18:], source.encode() + b'\x00')
	controller = '\\_SB.PCI0.SB0_'
	line = gpio_int([1], controller, edge=False, active_low=True)
	check('GpioInt: the tag', line[0], 0x8C)
	check('GpioInt: the length after the header', struct.unpack('<H', line[1:3])[0], len(line) - 3)
	check('GpioInt: revision 1', line[3], 1)
	check('GpioInt: an interrupt connection', line[4], 0)
	check('GpioInt: general flags - consumer', struct.unpack('<H', line[5:7])[0], 1)
	check('GpioInt: level, active low, exclusive, not wake', struct.unpack('<H', line[7:9])[0], 0x02)
	check('GpioInt: edge, active high, shared and wake', struct.unpack('<H', gpio_int([1], controller, edge=True, shared=True, wake=True)[7:9])[0], 0x19)
	check('GpioInt: default pull', line[9], 0)
	check('GpioInt: the pin table offset', struct.unpack('<H', line[14:16])[0], 23)
	check('GpioInt: resource source index', line[16], 0)
	check('GpioInt: the source name offset', struct.unpack('<H', line[17:19])[0], 25)
	check('GpioInt: the vendor data offset, past the name', struct.unpack('<H', line[19:21])[0], 25 + len(controller) + 1)
	check('GpioInt: no vendor data', struct.unpack('<H', line[21:23])[0], 0)
	check('GpioInt: the pin', struct.unpack('<H', line[23:25])[0], 1)
	check('GpioInt: the controller, NUL-terminated', line[25:], controller.encode() + b'\x00')
	check('GpioIo: an I/O connection', gpio_io([7], controller)[4], 1)
	check('GpioIo: input-only restriction', struct.unpack('<H', gpio_io([7], controller)[7:9])[0], 1)
	check('GpioIo: the pin table', struct.unpack('<H', gpio_io([7], controller)[23:25])[0], 7)
	# A `_DSM` ANSWERING AN INTEGER - HID over I2C's descriptor register: function 0's bitmap, function 1 a Return of it.
	hid = dsm('3cdff6f7-4267-4555-ad05-b30a3d8938de', {1: [ret(0x20)]})
	check('a _DSM returning an integer', bytes(hid).endswith(bytes(ret(buffer([0])))) and bytes(if_(lequal(arg(2), 1), [ret(0x20)])) in bytes(hid), True)
	check('its function 0 bitmap names function 1', bytes(if_(lequal(arg(2), 0), [ret(buffer([0b11]))])) in bytes(hid), True)
	check('a ThermalZone', thermal_zone('TZ00', [ret(0)]), b'\x5b\x85' + pkg_length(b'TZ00' + bytes(ret(0))))
	check('a QWordMemory length at its offset', struct.unpack('<Q', qword_memory(0x800000000, 0x1000)[38:46])[0], 0x1000)
	check('a QWordMemory minimum', struct.unpack('<Q', qword_memory(0x800000000, 0x1000)[14:22])[0], 0x800000000)
	# THE INTERPRETER'S SUITE LOADS THE SAMPLE FROM ITS OWN COPY: any byte this emitter now makes differently is drift.
	import os
	committed = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'aml', 'src', 'tests', 'fixtures', 'emitter-sample.aml')
	try:
		with open(committed, 'rb') as handle:
			check('the sample the interpreter suite loads', handle.read() == sample, True)
	except OSError as error:
		failures.append(f'the committed sample: {error}')
	if failures:
		for failure in failures:
			print(f'aml_emitter: {failure}', file=sys.stderr)
		return 1
	print('aml_emitter: every encoding checked')
	return 0


def sample_body():
	"""A table exercising most of the emitter - the interpreter's suite loads the same bytes (`--sample`)."""
	return [
		scope('\\_SB', [
			device('GPI0', [name('_HID', string('LSFXGPIO'))]),
			device('I2C0', [name('_HID', string('LSFXI2C0'))]),
			device('FIX0', [
				name('_HID', string('LSFX0001')),
				name('_CID', string('PRP0001')),
				name('_UID', 0),
				method('_STA', 0, [ret(0x0F)]),
				operation_region('FRAM', 'SystemMemory', 0x2000, 0x10),
				field('FRAM', [unit('VAL0', 32), unit('VAL1', 32)], access='DWordAcc'),
				mutex('FMTX'),
				method('BUMP', 1, [acquire('FMTX'), add('VAL0', arg(0), 'VAL0'), release('FMTX'), ret('VAL0')]),
				method('SIGN', 1, [store(arg(0), 'VAL1'), notify('FIX0', 0x80)]),
				operation_region('GSB0', 'GenericSerialBus', 0, 0x100),
				field('GSB0', [connection(i2c_serial_bus_v2(0x50, '\\_SB.I2C0')), access_as('BufferAcc', 0x06), unit('REG0', 8)], access='BufferAcc'),
				operation_region('GPO0', 'GeneralPurposeIo', 0, 1),
				field('GPO0', [connection(gpio_io([4], '\\_SB.GPI0')), unit('LIN4', 1)]),
				name('_CRS', buffer(resource_template(memory32_fixed(0xFE000000, 0x1000), gpio_int([5], '\\_SB.GPI0'), i2c_serial_bus_v2(0x51, '\\_SB.I2C0')))),
				name('_DSD', dsd({'compatible': string('liber,fixture'), 'reg': 0x50}, [('led@0', 'LED0')])),
				name('LED0', data_node({'label': string('status')})),
				dsm('4a5e8f45-1d6b-4a2c-9c35-5c8a6d2e7f10', {1: [ret(add(deref(index(arg(3), 0)), 1))]}),
			]),
		]),
	]


def main(argv):
	if argv[1:] == ['--self-test']:
		return self_test()
	if argv[1:2] == ['--sample'] and len(argv) == 3:
		with open(argv[2], 'wb') as out:
			out.write(table('SSDT', sample_body()))
		return 0
	print('usage: aml_emitter.py --self-test | --sample OUTPUT', file=sys.stderr)
	return 2


if __name__ == '__main__':
	sys.exit(main(sys.argv))
