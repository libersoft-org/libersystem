#!/usr/bin/env python3
# THE HARNESS'S FLATTENED-DEVICE-TREE EDITOR, shared: a tree read into nodes and properties, nodes and properties added,
# and the tree written back - the memory reservation block and every node it did not touch copied as they were. Pure
# Python rather than `dtc`, which the machines this runs on do not all have.
#
# Three users: `dma-mode-record.py`, which adds the boot-policy node; the device-tree fixture the ports' firmware
# gates boot with (`tree-fixture`): the virtio-gpio function's node at its slot under the PCI host, with its
# `virtio,device29` child - a GPIO controller and an interrupt controller - and a fixture node whose
# `interrupts-extended` names a line of that child; and the HID-over-I2C fixture (`hid-fixture`): the virtio-i2c
# function's node with its `virtio,device22` child, the two `hid-over-i2c` devices on it, and their interrupts, level
# and active low, on lines of the virtio-gpio function's child - the bindings Linux documents.
#
#   fdt_edit.py tree-fixture IN OUT --slot N [--line L]             write the firmware fixture tree
#   fdt_edit.py hid-fixture IN OUT --i2c-slot N --gpio-slot M       write the HID-over-I2C fixture tree
#   fdt_edit.py --self-test                                          build, edit and re-read a tree, writing nothing

import struct
import sys

FDT_MAGIC = 0xD00DFEED
FDT_BEGIN_NODE = 1
FDT_END_NODE = 2
FDT_PROP = 3
FDT_NOP = 4
FDT_END = 9


class TreeError(Exception):
	pass


def align4(n):
	return (n + 3) & ~3


class Node:
	def __init__(self, name, props=None, children=None):
		self.name = name
		self.props = props if props is not None else []
		self.children = children if children is not None else []

	def prop(self, name):
		for key, value in self.props:
			if key == name:
				return value
		return None

	def set(self, name, value):
		for at, (key, _) in enumerate(self.props):
			if key == name:
				self.props[at] = (name, value)
				return self
		self.props.append((name, value))
		return self

	def child(self, name):
		for child in self.children:
			if child.name == name:
				return child
		return None

	def add(self, node):
		if self.child(node.name) is not None:
			raise TreeError(f'{self.name or "/"} already has a node named {node.name}')
		self.children.append(node)
		return node


class Tree:
	def __init__(self, reserved, root, version=17, last_comp_version=16, boot_cpuid_phys=0):
		self.reserved = reserved
		self.root = root
		self.version = version
		self.last_comp_version = last_comp_version
		self.boot_cpuid_phys = boot_cpuid_phys

	def find(self, path):
		node = self.root
		for part in [p for p in path.split('/') if p]:
			node = node.child(part)
			if node is None:
				return None
		return node

	def walk(self):
		stack = [('', self.root)]
		while stack:
			path, node = stack.pop()
			yield (path or '/'), node
			for child in reversed(node.children):
				stack.append((f'{path}/{child.name}', child))

	def phandles(self):
		out = set()
		for _, node in self.walk():
			value = node.prop('phandle')
			if value is not None and len(value) == 4:
				out.add(struct.unpack('>I', value)[0])
		return out

	def new_phandle(self):
		used = self.phandles()
		value = max(used, default=0) + 1
		return value


def read_header(blob):
	if len(blob) < 40:
		raise TreeError('not a flattened device tree: shorter than its header')
	fields = struct.unpack('>10I', blob[:40])
	if fields[0] != FDT_MAGIC:
		raise TreeError('not a flattened device tree: bad magic')
	names = ['magic', 'totalsize', 'off_dt_struct', 'off_dt_strings', 'off_mem_rsvmap', 'version', 'last_comp_version', 'boot_cpuid_phys', 'size_dt_strings', 'size_dt_struct']
	return dict(zip(names, fields))


def reserved_block(blob, header):
	# Every (address, size) pair through the (0, 0) terminator, wherever the header put the block.
	at = header['off_mem_rsvmap']
	out = b''
	while at + 16 <= len(blob):
		pair = blob[at:at + 16]
		out += pair
		at += 16
		if pair == b'\0' * 16:
			return out
	raise TreeError('the memory reservation block has no terminator')


def parse(blob):
	h = read_header(blob)
	struct_block = blob[h['off_dt_struct']:h['off_dt_struct'] + h['size_dt_struct']]
	strings = blob[h['off_dt_strings']:h['off_dt_strings'] + h['size_dt_strings']]
	stack = []
	root = None
	at = 0
	while at < len(struct_block):
		token = struct.unpack('>I', struct_block[at:at + 4])[0]
		at += 4
		if token == FDT_BEGIN_NODE:
			end = struct_block.index(b'\0', at)
			node = Node(struct_block[at:end].decode())
			at = align4(end + 1)
			if stack:
				stack[-1].children.append(node)
			elif root is None:
				root = node
			else:
				raise TreeError('a second root node')
			stack.append(node)
		elif token == FDT_END_NODE:
			if not stack:
				raise TreeError('an end of a node that was never begun')
			stack.pop()
		elif token == FDT_PROP:
			length, nameoff = struct.unpack('>II', struct_block[at:at + 8])
			at += 8
			value = struct_block[at:at + length]
			at += align4(length)
			name = strings[nameoff:strings.index(b'\0', nameoff)].decode()
			if not stack:
				raise TreeError('a property outside every node')
			stack[-1].props.append((name, value))
		elif token == FDT_NOP:
			pass
		elif token == FDT_END:
			break
		else:
			raise TreeError(f'unknown structure token {token:#x}')
	if root is None or stack:
		raise TreeError('the tree does not close')
	return Tree(reserved_block(blob, h), root, h['version'], h['last_comp_version'], h['boot_cpuid_phys'])


def serialize(tree):
	strings = bytearray()
	offsets = {}

	def string_offset(name):
		if name not in offsets:
			offsets[name] = len(strings)
			strings.extend(name.encode() + b'\0')
		return offsets[name]

	out = bytearray()

	def emit(node):
		name = node.name.encode()
		out.extend(struct.pack('>I', FDT_BEGIN_NODE) + name + b'\0' + b'\0' * (align4(len(name) + 1) - len(name) - 1))
		for key, value in node.props:
			out.extend(struct.pack('>III', FDT_PROP, len(value), string_offset(key)) + value + b'\0' * (align4(len(value)) - len(value)))
		for child in node.children:
			emit(child)
		out.extend(struct.pack('>I', FDT_END_NODE))

	emit(tree.root)
	out.extend(struct.pack('>I', FDT_END))
	# Layout: header, the reservation block, the structure block, the strings block - every offset computed from the
	# blocks, which is what keeps this correct for a tree whose blocks were not in this order.
	off_rsv = 40
	off_struct = align4(off_rsv + len(tree.reserved))
	off_strings = off_struct + len(out)
	total = off_strings + len(strings)
	header = struct.pack('>10I', FDT_MAGIC, total, off_struct, off_strings, off_rsv, tree.version, tree.last_comp_version, tree.boot_cpuid_phys, len(strings), len(out))
	return bytes(header + tree.reserved + b'\0' * (off_struct - off_rsv - len(tree.reserved)) + out + strings)


def cells(*values):
	return b''.join(struct.pack('>I', value & 0xFFFFFFFF) for value in values)


def text(value):
	return value.encode() + b'\0'


def pci_host(tree):
	for path, node in tree.walk():
		if node.prop('device_type') == b'pci\0':
			return path, node
	raise TreeError('the tree describes no PCI host')


# A MODERN VIRTIO FUNCTION'S PCI DEVICE ID is 0x1040 plus its virtio device id.
VIRTIO_GPIO = 0x29
VIRTIO_I2C = 0x22
# The tree's interrupt-type flags, whose values virtio-gpio's trigger types share.
IRQ_TYPE_EDGE_RISING = 1
IRQ_TYPE_LEVEL_LOW = 8


def virtio_function(tree, slot, device, name):
	"""The node of the virtio function at `slot` of bus 0 under the PCI host, and its one child - the virtio device's
	node Linux's binding reads, named `name` and compatible `virtio,device<id>` - made once and found again after."""
	_, host = pci_host(tree)
	function = host.child(f'{name}@{slot:x},0')
	if function is None:
		function = host.add(Node(f'{name}@{slot:x},0', [('compatible', text(f'pci1af4,{0x1040 + device:x}')), ('reg', cells(slot << 11, 0, 0, 0, 0))]))
	child = function.child(name)
	if child is None:
		child = function.add(Node(name, [('compatible', text(f'virtio,device{device:x}'))]))
	return child


def gpio_controller(tree, slot):
	"""The virtio-gpio function's child as a GPIO controller and an interrupt controller: its phandle."""
	child = virtio_function(tree, slot, VIRTIO_GPIO, 'gpio')
	if child.prop('phandle') is None:
		for key, value in [('gpio-controller', b''), ('interrupt-controller', b''), ('#gpio-cells', cells(2)), ('#interrupt-cells', cells(2)), ('phandle', cells(tree.new_phandle()))]:
			child.set(key, value)
	return struct.unpack('>I', child.prop('phandle'))[0]


def tree_fixture(tree, slot, line=2):
	"""THE PORTS' FIRMWARE FIXTURE: the virtio-gpio function's node at `slot` under the PCI host, its `virtio,device29`
	child a GPIO and interrupt controller, and `/liber-fixture` - matched by the development image alone - whose
	`interrupts-extended` names line `line` of that child, edge-triggered."""
	phandle = gpio_controller(tree, slot)
	tree.root.add(Node('liber-fixture', [('compatible', text('liber,tree-fixture')), ('interrupts-extended', cells(phandle, line, IRQ_TYPE_EDGE_RISING))]))
	return tree


# THE HID-OVER-I2C DEVICES the backend models: (node name, address, descriptor register, GPIO line).
HID_DEVICES = [('touchpad', 0x2C, 0x20, 0), ('touchscreen', 0x10, 0x01, 1)]


def hid_fixture(tree, i2c_slot, gpio_slot):
	"""THE HID-OVER-I2C FIXTURE: the virtio-i2c function's `virtio,device22` child, a bus of seven-bit addresses, and on
	it each of `HID_DEVICES` as a `hid-over-i2c` node - `reg` its address, `hid-descr-addr` its descriptor register,
	its interrupt a line of the virtio-gpio function's child, level and active low."""
	phandle = gpio_controller(tree, gpio_slot)
	bus = virtio_function(tree, i2c_slot, VIRTIO_I2C, 'i2c')
	bus.set('#address-cells', cells(1)).set('#size-cells', cells(0))
	for name, address, register, line in HID_DEVICES:
		bus.add(Node(f'{name}@{address:x}', [('compatible', text('hid-over-i2c')), ('reg', cells(address)), ('hid-descr-addr', cells(register)), ('interrupts-extended', cells(phandle, line, IRQ_TYPE_LEVEL_LOW))]))
	return tree


def self_test():
	failures = []

	def check(what, got, want):
		if got != want:
			failures.append(f'{what}: {got!r} != {want!r}')

	root = Node('', [('#address-cells', cells(2)), ('#size-cells', cells(2))])
	pcie = root.add(Node('pcie@10000000', [('compatible', text('pci-host-ecam-generic')), ('device_type', text('pci')), ('#address-cells', cells(3)), ('#size-cells', cells(2)), ('phandle', cells(5))]))
	pcie.add(Node('dev@1,0', [('reg', cells(0x800, 0, 0, 0, 0))]))
	blob = serialize(Tree(struct.pack('>QQ', 0x4000_0000, 0x1000) + b'\0' * 16, root))
	tree = parse(blob)
	check('a tree reads back', serialize(tree), blob)
	check('the reservation block is kept', tree.reserved[:16], struct.pack('>QQ', 0x4000_0000, 0x1000))
	tree_fixture(tree, 3)
	edited = parse(serialize(tree))
	function = edited.find('/pcie@10000000/gpio@3,0')
	check('the function node is added under the host', function is not None, True)
	check('its reg names device 3', function.prop('reg')[:4] if function else None, cells(3 << 11))
	check('its compatible is the modern virtio-gpio function', function.prop('compatible') if function else None, text('pci1af4,1069'))
	child = edited.find('/pcie@10000000/gpio@3,0/gpio')
	check('its child is a GPIO controller', child.prop('gpio-controller') if child else None, b'')
	check('the child is the virtio device', child.prop('compatible') if child else None, text('virtio,device29'))
	check('a fresh phandle', child.prop('phandle') if child else None, cells(6))
	fixture = edited.find('/liber-fixture')
	check('the fixture names line 2 of the child', fixture.prop('interrupts-extended') if fixture else None, cells(6, 2, 1))
	check('an existing node is untouched', edited.find('/pcie@10000000/dev@1,0').prop('reg'), cells(0x800, 0, 0, 0, 0))
	# THE HID FIXTURE ON THE SAME TREE: the GPIO controller is found again, not added twice.
	hid_fixture(edited, 4, 3)
	both = parse(serialize(edited))
	bus = both.find('/pcie@10000000/i2c@4,0/i2c')
	check('the I2C function node and its virtio child', (both.find('/pcie@10000000/i2c@4,0').prop('compatible'), bus.prop('compatible') if bus else None), (text('pci1af4,1062'), text('virtio,device22')))
	check('a bus of seven-bit addresses', (bus.prop('#address-cells'), bus.prop('#size-cells')) if bus else None, (cells(1), cells(0)))
	pad = both.find('/pcie@10000000/i2c@4,0/i2c/touchpad@2c')
	check('the touchpad', (pad.prop('compatible'), pad.prop('reg'), pad.prop('hid-descr-addr')) if pad else None, (text('hid-over-i2c'), cells(0x2C), cells(0x20)))
	check('its line 0 of the one GPIO controller, level and active low', pad.prop('interrupts-extended') if pad else None, cells(6, 0, 8))
	screen = both.find('/pcie@10000000/i2c@4,0/i2c/touchscreen@10')
	check('the touchscreen', (screen.prop('reg'), screen.prop('hid-descr-addr'), screen.prop('interrupts-extended')) if screen else None, (cells(0x10), cells(0x01), cells(6, 1, 8)))
	check('one GPIO function', len([c for c in both.find('/pcie@10000000').children if c.name.startswith('gpio@')]), 1)
	try:
		tree.root.add(Node('liber-fixture'))
		failures.append('a second node of one name was added')
	except TreeError:
		pass
	if failures:
		for failure in failures:
			print(f'fdt_edit: {failure}', file=sys.stderr)
		return 1
	print('fdt_edit: every edit checked')
	return 0


def main(argv):
	if argv == ['--self-test']:
		return self_test()
	if len(argv) >= 3 and argv[0] == 'hid-fixture':
		i2c_slot = int(argv[argv.index('--i2c-slot') + 1], 0) if '--i2c-slot' in argv else 0x15
		gpio_slot = int(argv[argv.index('--gpio-slot') + 1], 0) if '--gpio-slot' in argv else 0x16
		with open(argv[1], 'rb') as source:
			tree = parse(source.read())
		with open(argv[2], 'wb') as destination:
			destination.write(serialize(hid_fixture(tree, i2c_slot, gpio_slot)))
		return 0
	if len(argv) >= 3 and argv[0] == 'tree-fixture':
		slot = 3
		line = 2
		if '--slot' in argv:
			slot = int(argv[argv.index('--slot') + 1], 0)
		if '--line' in argv:
			line = int(argv[argv.index('--line') + 1], 0)
		with open(argv[1], 'rb') as source:
			tree = parse(source.read())
		with open(argv[2], 'wb') as destination:
			destination.write(serialize(tree_fixture(tree, slot, line)))
		return 0
	print('usage: fdt_edit.py tree-fixture IN OUT [--slot N] [--line L] | hid-fixture IN OUT [--i2c-slot N] [--gpio-slot M] | --self-test', file=sys.stderr)
	return 2


if __name__ == '__main__':
	sys.exit(main(sys.argv[1:]))
