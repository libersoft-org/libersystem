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
# and active low, on lines of the virtio-gpio function's child - the bindings Linux documents; and the TCPCI fixture
# (`tcpc-fixture`): the same bus with a `tcpci` port controller on it, its alert a line of the same child, and its
# `usb-c-connector` child describing the gate's sink; and the idle states (`idle-fixture`) QEMU's trees do not carry:
# `/cpus/idle-states` with two retention states the firmware runs and one that loses the core's context, and every cpu
# node's `cpu-idle-states` naming them - PSCI's binding on aarch64, the SBI's on riscv64.
#
#   fdt_edit.py tree-fixture IN OUT --slot N [--line L]             write the firmware fixture tree
#   fdt_edit.py hid-fixture IN OUT --i2c-slot N --gpio-slot M       write the HID-over-I2C fixture tree
#   fdt_edit.py tcpc-fixture IN OUT --i2c-slot N --gpio-slot M      write the TCPCI fixture tree
#   fdt_edit.py idle-fixture IN OUT --binding arm|riscv              write the idle-state fixture tree
#   fdt_edit.py system-suspend-fixture IN OUT                       enable OpenSBI's system-suspend test backend
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


# THE PORT CONTROLLER the backend models (`vhost-i2c-gpio.py --tcpc`): its address and its alert line; and THE BOARD, as
# the TCPCI gate's SSDT describes it - Fixed 5 V at 3 A and 15 V at 2 A, operating at 15 W.
TCPC_ADDRESS = 0x52
TCPC_LINE = 5
TCPC_SINK_PDOS = [(5000 // 50) << 10 | 3000 // 10, (15000 // 50) << 10 | 2000 // 10]
TCPC_OPERATING_MICROWATTS = 15_000_000


def tcpc_fixture(tree, i2c_slot, gpio_slot):
	"""THE TCPCI FIXTURE: the virtio-i2c function's `virtio,device22` child, and on it a `tcpci` node - `reg` its address,
	its alert a line of the virtio-gpio function's child, level and active low - whose `connector` child is the
	`usb-c-connector` Linux's binding describes: a sink, its sink PDOs and its operating power."""
	phandle = gpio_controller(tree, gpio_slot)
	bus = virtio_function(tree, i2c_slot, VIRTIO_I2C, 'i2c')
	bus.set('#address-cells', cells(1)).set('#size-cells', cells(0))
	controller = bus.add(Node(f'tcpc@{TCPC_ADDRESS:x}', [('compatible', text('tcpci')), ('reg', cells(TCPC_ADDRESS)), ('interrupts-extended', cells(phandle, TCPC_LINE, IRQ_TYPE_LEVEL_LOW))]))
	controller.add(Node('connector', [('compatible', text('usb-c-connector')), ('power-role', text('sink')), ('sink-pdos', cells(*TCPC_SINK_PDOS)), ('op-sink-microwatt', cells(TCPC_OPERATING_MICROWATTS))]))
	return tree


# THE IDLE STATES, per binding: (name, parameter, entry, exit, min-residency, local-timer-stop). Two the firmware takes as
# a wait - PSCI's StateIDs 1 and 2 in the original power-state format (StateType 0, bit 16 clear), the SBI's default
# retentive suspend - a shallow one and a deep one whose wake is past a 1000 us latency request; and one that loses the
# core's context (StateType 1, a non-retentive type), which the kernel installs and holds out of its choices.
IDLE_STATES = {
	'arm': ('arm,idle-state', 'arm,psci-suspend-param', [('cpu-retention', 0x0000_0001, 20, 40, 80, False), ('cpu-sleep', 0x0000_0002, 500, 1500, 5000, False), ('cpu-power-down', 0x0001_0003, 100, 250, 1000, True)]),
	'riscv': ('riscv,idle-state', 'riscv,sbi-suspend-param', [('cpu-retentive', 0x0000_0000, 20, 40, 80, False), ('cpu-sleep', 0x0000_0000, 500, 1500, 5000, False), ('cpu-non-retentive', 0x8000_0000, 100, 250, 1000, True)]),
}


def idle_fixture(tree, binding):
	"""THE IDLE STATES QEMU'S TREES DO NOT CARRY: `/cpus/idle-states` with the binding's three states, and every cpu
	node's `cpu-idle-states` naming them in that order."""
	compatible, parameter, states = IDLE_STATES[binding]
	cpus = tree.find('/cpus')
	if cpus is None:
		raise TreeError('the tree describes no /cpus')
	idle = cpus.child('idle-states') or cpus.add(Node('idle-states', [('entry-method', text('psci'))] if binding == 'arm' else []))
	phandles = []
	for name, value, entry, exit_, residency, timer_stop in states:
		node = idle.child(name)
		if node is None:
			props = [('compatible', text(compatible)), (parameter, cells(value)), ('entry-latency-us', cells(entry)), ('exit-latency-us', cells(exit_)), ('min-residency-us', cells(residency))]
			if timer_stop:
				props.append(('local-timer-stop', b''))
			props.append(('phandle', cells(tree.new_phandle())))
			node = idle.add(Node(name, props))
		phandles.append(struct.unpack('>I', node.prop('phandle'))[0])
	named = 0
	for child in cpus.children:
		if child.prop('device_type') == text('cpu'):
			child.set('cpu-idle-states', cells(*phandles))
			named += 1
	if named == 0:
		raise TreeError('no cpu node to name the idle states')
	return tree


def system_suspend_fixture(tree):
	"""Enable OpenSBI's documented test backend and declare the test board's retained RTC wake.
	OpenSBI consumes and removes this node before the guest boots. The actual SBI SUSP call still checks
	that other harts stopped and resumes through the firmware's warm entry; this does not model power loss.
	See OpenSBI v1.6 docs/opensbi_config.md and lib/sbi/sbi_system.c."""
	rtcs = [node for _, node in tree.walk() if b'google,goldfish-rtc' in (node.prop('compatible') or b'').split(b'\0')]
	if len(rtcs) != 1:
		raise TreeError('the system-suspend test board needs exactly one Goldfish RTC')
	# The OpenSBI test backend leaves this device and its interrupt route powered. This board declaration
	# is intentionally not inferred in the kernel merely from the RTC's register-compatible string.
	rtcs[0].set('wakeup-source', b'')
	chosen = tree.root.child('chosen') or tree.root.add(Node('chosen'))
	config = chosen.child('opensbi-config') or chosen.add(Node('opensbi-config', [('compatible', text('opensbi,config'))]))
	if config.prop('compatible') != text('opensbi,config'):
		raise TreeError('/chosen/opensbi-config has a different binding')
	config.set('system-suspend-test', b'')
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
	# THE TCPCI FIXTURE ON THE SAME TREE: the controller on the one bus, its connector below it.
	tcpc_fixture(both, 4, 3)
	three = parse(serialize(both))
	controller = three.find('/pcie@10000000/i2c@4,0/i2c/tcpc@52')
	check('the port controller', (controller.prop('compatible'), controller.prop('reg'), controller.prop('interrupts-extended')) if controller else None, (text('tcpci'), cells(0x52), cells(6, 5, 8)))
	connector = three.find('/pcie@10000000/i2c@4,0/i2c/tcpc@52/connector')
	check('its connector', (connector.prop('compatible'), connector.prop('power-role'), connector.prop('sink-pdos'), connector.prop('op-sink-microwatt')) if connector else None, (text('usb-c-connector'), text('sink'), cells(0x1912C, 0x4B0C8), cells(15_000_000)))
	try:
		tree.root.add(Node('liber-fixture'))
		failures.append('a second node of one name was added')
	except TreeError:
		pass
	# THE IDLE FIXTURE, on a tree with two cpus: both name every state, and a second run adds nothing.
	for binding, compatible, parameter in (('arm', 'arm,idle-state', 'arm,psci-suspend-param'), ('riscv', 'riscv,idle-state', 'riscv,sbi-suspend-param')):
		cpu_root = Node('', [('#address-cells', cells(2)), ('#size-cells', cells(2))])
		cpu_node = cpu_root.add(Node('cpus', [('#address-cells', cells(1)), ('#size-cells', cells(0))]))
		for index in range(2):
			cpu_node.add(Node(f'cpu@{index}', [('device_type', text('cpu')), ('reg', cells(index))]))
		cpu_tree = parse(serialize(Tree(b'\0' * 16, cpu_root)))
		idle_fixture(cpu_tree, binding)
		idle_fixture(cpu_tree, binding)
		again = parse(serialize(cpu_tree))
		states = again.find('/cpus/idle-states')
		check(f'{binding}: three states', len(states.children) if states else None, 3)
		first, second = (states.children[0], states.children[2]) if states and len(states.children) == 3 else (Node(''), Node(''))
		check(f'{binding}: the binding', (first.prop('compatible'), second.prop('compatible')), (text(compatible), text(compatible)))
		check(f'{binding}: the retention state keeps its timer, the other stops it', (first.prop('local-timer-stop'), second.prop('local-timer-stop')), (None, b''))
		check(f'{binding}: each a parameter', (first.prop(parameter) is not None, second.prop(parameter) is not None), (True, True))
		names = cells(*[struct.unpack('>I', child.prop('phandle'))[0] for child in states.children]) if states else None
		check(f'{binding}: every cpu names all three, in order', [again.find(f'/cpus/cpu@{index}').prop('cpu-idle-states') for index in range(2)], [names, names])
	# Preserve the RTC/interrupt description and unrelated chosen data; declare retention only on the RTC.
	suspend_tree = parse(blob)
	suspend_tree.root.add(Node('chosen', [('bootargs', text('unchanged'))]))
	suspend_tree.root.add(Node('rtc@101000', [('compatible', text('google,goldfish-rtc')), ('reg', cells(0, 0x101000, 0, 0x1000)), ('interrupts', cells(11, 4))]))
	system_suspend_fixture(suspend_tree)
	system_suspend_fixture(suspend_tree)
	suspend_tree = parse(serialize(suspend_tree))
	check('OpenSBI test backend enabled', suspend_tree.find('/chosen/opensbi-config').prop('system-suspend-test'), b'')
	check('OpenSBI config binding', suspend_tree.find('/chosen/opensbi-config').prop('compatible'), text('opensbi,config'))
	check('chosen data preserved', suspend_tree.find('/chosen').prop('bootargs'), text('unchanged'))
	check('one firmware config node', len(suspend_tree.find('/chosen').children), 1)
	check('RTC retained wake declared', suspend_tree.find('/rtc@101000').prop('wakeup-source'), b'')
	check('RTC registers preserved', suspend_tree.find('/rtc@101000').prop('reg'), cells(0, 0x101000, 0, 0x1000))
	check('RTC interrupt preserved', suspend_tree.find('/rtc@101000').prop('interrupts'), cells(11, 4))
	check('suspend fixture preserves hardware', serialize(Tree(suspend_tree.reserved, suspend_tree.find('/pcie@10000000'))), serialize(Tree(tree.reserved, parse(blob).find('/pcie@10000000'))))
	if failures:
		for failure in failures:
			print(f'fdt_edit: {failure}', file=sys.stderr)
		return 1
	print('fdt_edit: every edit checked')
	return 0


def main(argv):
	if argv == ['--self-test']:
		return self_test()
	if len(argv) == 3 and argv[0] == 'system-suspend-fixture':
		with open(argv[1], 'rb') as source:
			tree = parse(source.read())
		with open(argv[2], 'wb') as destination:
			destination.write(serialize(system_suspend_fixture(tree)))
		return 0
	if len(argv) >= 3 and argv[0] in ('hid-fixture', 'tcpc-fixture'):
		i2c_slot = int(argv[argv.index('--i2c-slot') + 1], 0) if '--i2c-slot' in argv else 0x15
		gpio_slot = int(argv[argv.index('--gpio-slot') + 1], 0) if '--gpio-slot' in argv else 0x16
		edit = hid_fixture if argv[0] == 'hid-fixture' else tcpc_fixture
		with open(argv[1], 'rb') as source:
			tree = parse(source.read())
		with open(argv[2], 'wb') as destination:
			destination.write(serialize(edit(tree, i2c_slot, gpio_slot)))
		return 0
	if len(argv) >= 3 and argv[0] == 'idle-fixture':
		binding = argv[argv.index('--binding') + 1] if '--binding' in argv else ''
		if binding not in IDLE_STATES:
			print('fdt_edit: idle-fixture needs --binding arm or --binding riscv', file=sys.stderr)
			return 2
		with open(argv[1], 'rb') as source:
			tree = parse(source.read())
		with open(argv[2], 'wb') as destination:
			destination.write(serialize(idle_fixture(tree, binding)))
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
	print('usage: fdt_edit.py tree-fixture IN OUT [--slot N] [--line L] | hid-fixture|tcpc-fixture IN OUT [--i2c-slot N] [--gpio-slot M] | idle-fixture IN OUT --binding arm|riscv | system-suspend-fixture IN OUT | --self-test', file=sys.stderr)
	return 2


if __name__ == '__main__':
	sys.exit(main(sys.argv[1:]))
