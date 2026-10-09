// EVERY DEVICE THE TREE DESCRIBES, as the kernel publishes them: each node that names a `compatible` and
// is not disabled, its `reg` translated up the buses it sits behind (or kept as an address on the I2C or
// SPI bus it sits on), its interrupts resolved against the controllers they name, and the span of its
// tokens - which is what its property block is read from.
//
// NOTHING HERE DECIDES WHAT A NODE IS. Whether a node is one the kernel drives itself, whether its
// interrupt is a wired line of the controller the kernel drives or a line of a GPIO controller, and what
// its identity is called are the kernel's; this reader answers what the tree SAYS, uninterpreted, the way
// `pci_intx_route` answers a specifier.

use super::*;

// The most `reg` ranges and interrupts one published node carries; a node with more is published with
// what fits and the rest counted as refused, which the kernel reports.
pub const MAX_NODE_REGS: usize = 6;
pub const MAX_NODE_INTERRUPTS: usize = 4;
// A node's `compatible` stringlist, copied whole or refused: a list cut short would name a different chip.
pub const MAX_COMPATIBLE: usize = 128;

// What a node's `reg` addresses, which is its PARENT's business: an I2C or SPI controller numbers its
// children on its own bus, and a parent with no size cells numbers them in no address space at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeBus {
	Memory,
	I2c,
	Spi,
	// A FUNCTION ON A PCI BUS: a child of a node whose `device_type` is `pci`, its `reg` a configuration-space address
	// - the first cell's bus, device and function (`pci_function`) - never MMIO. The node describes the function the
	// bus scan finds there: its companion, not a device of its own.
	Pci,
	Other,
}

// One interrupt a node names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeInterrupt {
	// On an interrupt controller: its phandle and its own specifier, uninterpreted.
	Controller(IntxRoute),
	// A LINE OF A GPIO CONTROLLER that is also an interrupt controller: never a wired line of the machine,
	// but a connection its driver reaches through the GPIO controller's.
	GpioLine { controller: u32, line: u32, flags: u32 },
	// A specifier this reader could not resolve: a parent the tree does not describe, one that is neither
	// kind of controller, a specifier shorter than its parent's binding, or one routed through an
	// `interrupt-map` nexus.
	Unresolved,
}

// One device node.
#[derive(Clone, Copy)]
pub struct DeviceNode {
	path: [u8; MAX_PATH],
	path_len: usize,
	compatible: [u8; MAX_COMPATIBLE],
	compatible_len: usize,
	pub phandle: u32,
	pub bus: NodeBus,
	regs: [(u64, u64); MAX_NODE_REGS],
	reg_count: usize,
	// A `reg` this reader could not read or translate, or more ranges than fit: the kernel refuses such a
	// node rather than publishing part of what it owns.
	pub reg_refused: bool,
	interrupts: [NodeInterrupt; MAX_NODE_INTERRUPTS],
	interrupt_count: usize,
	pub interrupt_controller: bool,
	pub gpio_controller: bool,
	// The board declares this device usable as a system wake source. Merely having an interrupt does not.
	pub wakeup_source: bool,
	// THE DMA STREAM ITS `iommus` NAMES: the first specifier's first cell after the IOMMU's phandle - the stream id under
	// a one-cell binding, which every IOMMU binding a `virt` machine carries uses. None for a node with no `iommus`,
	// which masters nothing an IOMMU translates.
	pub dma_stream: Option<u32>,
	span: (u64, u64),
}

impl DeviceNode {
	pub fn path(&self) -> &[u8] {
		&self.path[..self.path_len]
	}

	// Each string of the node's `compatible`, most specific first.
	pub fn compatible(&self) -> impl Iterator<Item = &[u8]> {
		self.compatible[..self.compatible_len].split(|byte| *byte == 0).filter(|text| !text.is_empty())
	}

	pub fn is_compatible(&self, want: &[u8]) -> bool {
		self.compatible().any(|text| text == want)
	}

	// The node's ranges: physical (base, size) under a memory-mapped bus, (address, 0) on an I2C or SPI one.
	pub fn regs(&self) -> &[(u64, u64)] {
		&self.regs[..self.reg_count]
	}

	pub fn interrupts(&self) -> &[NodeInterrupt] {
		&self.interrupts[..self.interrupt_count]
	}

	// A PCI child's function - (bus, device, function) from its `reg`'s first cell (`phys.hi`: bus in bits 23..16,
	// device in 15..11, function in 10..8) - or `None` for a node on any other bus.
	pub fn pci_function(&self) -> Option<(u8, u8, u8)> {
		if self.bus != NodeBus::Pci || self.reg_count == 0 {
			return None;
		}
		let hi = self.regs[0].0 as u32;
		Some(((hi >> 16) as u8, ((hi >> 11) & 0x1F) as u8, ((hi >> 8) & 0x07) as u8))
	}
}

// What one depth of the walk knows about the node open there.
#[derive(Clone, Copy)]
struct Open {
	path_len: usize,
	name: u64,
	compatible: Option<(u64, u32)>,
	enabled: bool,
	reg: Option<(u64, u32)>,
	interrupts: Option<(u64, u32)>,
	extended: Option<(u64, u32)>,
	iommus: Option<(u64, u32)>,
	phandle: u32,
	interrupt_controller: bool,
	gpio_controller: bool,
	wakeup_source: bool,
	// `device_type = "pci"`: its children are functions on its bus.
	pci: bool,
	start: u64,
}

impl Open {
	const fn empty() -> Self {
		Open { path_len: 0, name: 0, compatible: None, enabled: true, reg: None, interrupts: None, extended: None, iommus: None, phandle: 0, interrupt_controller: false, gpio_controller: false, wakeup_source: false, pci: false, start: 0 }
	}
}

// What an interrupt parent is, found by its phandle.
#[derive(Clone, Copy, Default)]
struct Controller {
	phandle: u32,
	interrupt_cells: u32,
	interrupt_controller: bool,
	gpio_controller: bool,
}

impl Fdt {
	// Walk every device node, visiting each as it closes - so a node's children come before it. False when
	// the walk was refused: a token that does not fit, a depth past this reader's, a tree that is not one.
	pub fn devices(&self, mut visit: impl FnMut(&DeviceNode)) -> bool {
		// SAFETY: every read below goes through `bounds`, as every walk in this file does.
		unsafe { self.devices_inner(&mut visit).is_some() }
	}

	unsafe fn devices_inner(&self, visit: &mut impl FnMut(&DeviceNode)) -> Option<()> {
		let b = self.bounds()?;
		unsafe {
			let mut p = b.struct_start;
			let mut depth: i32 = -1;
			let mut cells = [(2u32, 2u32); MAX_DEPTH + 1];
			let mut buses = [Bus::root(); MAX_DEPTH + 1];
			let mut parent = [0u32; MAX_DEPTH + 1];
			let mut open = [Open::empty(); MAX_DEPTH + 1];
			let mut path = [0u8; MAX_PATH];
			let mut path_len = 0usize;
			let mut controllers = [Controller::default(); 16];
			loop {
				let token_at = p;
				let token = self.be32_in(p, b.struct_end)?;
				p += 4;
				match token {
					FDT_BEGIN_NODE => {
						depth += 1;
						let (name, next) = self.node_name_in(p, &b)?;
						p = next;
						let at = depth as usize;
						if at > MAX_DEPTH {
							return None;
						}
						// THE PATH, built as the walk descends: `/` for the root and `/name` per level below it.
						// A path that does not fit is a node this reader cannot name, so the walk is refused.
						if at > 0 {
							let len = self.str_len_in(name, b.struct_end)? as usize;
							if path_len + 1 + len > MAX_PATH {
								return None;
							}
							path[path_len] = b'/';
							for i in 0..len {
								path[path_len + 1 + i] = self.u8_at(name + i as u64);
							}
							path_len += 1 + len;
							cells[at] = cells[at - 1];
							buses[at] = Bus { cells: cells[at], ranges: None };
							parent[at] = parent[at - 1];
						} else {
							buses[0] = Bus::root();
						}
						open[at] = Open { path_len, name, start: token_at, ..Open::empty() };
					}
					FDT_END_NODE => {
						if depth < 0 {
							return None;
						}
						let at = depth as usize;
						let node = open[at];
						if at > 0
							&& node.enabled && let Some((value, len)) = node.compatible
						{
							let mut device = DeviceNode { path: [0; MAX_PATH], path_len: 0, compatible: [0; MAX_COMPATIBLE], compatible_len: 0, phandle: node.phandle, bus: NodeBus::Memory, regs: [(0, 0); MAX_NODE_REGS], reg_count: 0, reg_refused: false, interrupts: [NodeInterrupt::Unresolved; MAX_NODE_INTERRUPTS], interrupt_count: 0, interrupt_controller: node.interrupt_controller, gpio_controller: node.gpio_controller, wakeup_source: node.wakeup_source, dma_stream: None, span: (node.start, p) };
							let shown = if at == 0 { 1 } else { node.path_len };
							device.path[..shown].copy_from_slice(if at == 0 { b"/" } else { &path[..shown] });
							device.path_len = shown;
							// A compatible that does not fit is refused whole rather than cut.
							if (len as usize) <= MAX_COMPATIBLE {
								for i in 0..len as usize {
									device.compatible[i] = self.u8_at(value + i as u64);
								}
								device.compatible_len = len as usize;
							}
							device.bus = self.bus_of(&open[at - 1], cells[at - 1]);
							if let Some((value, len)) = node.reg {
								self.read_regs(&mut device, value, len, cells[at - 1], &buses, at);
							}
							self.read_interrupts(&mut device, node, parent[at], &mut controllers);
							// `iommus = <&iommu id ...>`: the stream id after the first phandle.
							if let Some((value, len)) = node.iommus
								&& len >= 8
							{
								device.dma_stream = Some(self.be32(value + 4));
							}
							visit(&device);
						}
						if at > 0 {
							path_len = open[at - 1].path_len;
						}
						depth -= 1;
						if depth < -1 {
							return None;
						}
					}
					FDT_PROP => {
						let (pname, len, value, next) = self.prop_in(p, &b)?;
						p = next;
						if depth < 0 {
							return None;
						}
						let at = depth as usize;
						if len == 4 && self.str_eq(pname, "#address-cells") {
							cells[at].0 = self.be32(value);
							buses[at].cells.0 = cells[at].0;
						} else if len == 4 && self.str_eq(pname, "#size-cells") {
							cells[at].1 = self.be32(value);
							buses[at].cells.1 = cells[at].1;
						} else if self.str_eq(pname, "ranges") {
							buses[at].ranges = Some((value, len));
						} else if self.str_eq(pname, "compatible") && len > 0 {
							open[at].compatible = Some((value, len));
						} else if self.str_eq(pname, "status") {
							open[at].enabled = (len >= 5 && self.str_eq(value, "okay")) || (len >= 3 && self.str_eq(value, "ok"));
						} else if self.str_eq(pname, "reg") {
							open[at].reg = Some((value, len));
						} else if self.str_eq(pname, "interrupts") {
							open[at].interrupts = Some((value, len));
						} else if self.str_eq(pname, "interrupts-extended") {
							open[at].extended = Some((value, len));
						} else if self.str_eq(pname, "iommus") {
							open[at].iommus = Some((value, len));
						} else if len == 4 && self.str_eq(pname, "interrupt-parent") {
							parent[at] = self.be32(value);
						} else if len == 4 && (self.str_eq(pname, "phandle") || self.str_eq(pname, "linux,phandle")) {
							open[at].phandle = self.be32(value);
						} else if self.str_eq(pname, "interrupt-controller") {
							open[at].interrupt_controller = true;
						} else if self.str_eq(pname, "gpio-controller") {
							open[at].gpio_controller = true;
						} else if self.str_eq(pname, "wakeup-source") && len == 0 {
							open[at].wakeup_source = true;
						} else if len >= 4 && self.str_eq(pname, "device_type") && self.str_eq(value, "pci") {
							open[at].pci = true;
						}
					}
					FDT_NOP => {}
					FDT_END => return (depth == -1).then_some(()),
					_ => return None,
				}
			}
		}
	}

	// What the parent's children are addressed on. A memory-mapped bus has size cells; a parent with none
	// numbers its children in its own space - an I2C or SPI controller by its node name, anything else not
	// at all as far as this reader can say.
	unsafe fn bus_of(&self, parent: &Open, cells: (u32, u32)) -> NodeBus {
		if parent.pci {
			return NodeBus::Pci;
		}
		if cells.1 != 0 {
			return NodeBus::Memory;
		}
		unsafe {
			if self.str_starts(parent.name, "i2c") {
				NodeBus::I2c
			} else if self.str_starts(parent.name, "spi") {
				NodeBus::Spi
			} else {
				NodeBus::Other
			}
		}
	}

	unsafe fn read_regs(&self, device: &mut DeviceNode, value: u64, len: u32, cells: (u32, u32), buses: &[Bus; MAX_DEPTH + 1], at: usize) {
		match device.bus {
			NodeBus::Memory => {
				let pair = (cells.0 + cells.1) * 4;
				if pair == 0 || len % pair != 0 {
					device.reg_refused = true;
					return;
				}
				for index in 0..len / pair {
					if device.reg_count == MAX_NODE_REGS {
						device.reg_refused = true;
						return;
					}
					// SAFETY: inside the property the walk read out of the struct block.
					let Some((child, size)) = (unsafe { self.read_reg_range(value, len, cells.0, cells.1, index) }) else {
						device.reg_refused = true;
						return;
					};
					// AND TRANSLATED UP THE BUSES THE NODE SITS BEHIND - QEMU's platform bus puts a TPM node
					// under a `simple-bus` whose `ranges` move it - or refused: an untranslated address is an
					// address on a different bus.
					let Some(physical) = (unsafe { self.translate_to_root(buses, at - 1, child) }) else {
						device.reg_refused = true;
						return;
					};
					device.regs[device.reg_count] = (physical, size);
					device.reg_count += 1;
				}
			}
			NodeBus::I2c | NodeBus::Spi => {
				// AN ADDRESS ON THE BUS, never MMIO: one cell per child on both.
				if cells.0 == 1 && len >= 4 {
					device.regs[0] = (u64::from(unsafe { self.be32(value) }), 0);
					device.reg_count = 1;
				} else {
					device.reg_refused = true;
				}
			}
			NodeBus::Pci => {
				// THE CONFIGURATION ADDRESS, kept as the first cell: three address cells per child on a PCI bus.
				if cells.0 == 3 && len >= 12 {
					device.regs[0] = (u64::from(unsafe { self.be32(value) }), 0);
					device.reg_count = 1;
				} else {
					device.reg_refused = true;
				}
			}
			NodeBus::Other => {}
		}
	}

	unsafe fn read_interrupts(&self, device: &mut DeviceNode, node: Open, inherited: u32, controllers: &mut [Controller; 16]) {
		unsafe {
			if let Some((value, len)) = node.extended {
				// `interrupts-extended`: each entry names its own controller, and its length is that controller's.
				let mut at = 0u32;
				while at + 4 <= len && device.interrupt_count < MAX_NODE_INTERRUPTS {
					let phandle = self.be32(value + at as u64);
					let Some(controller) = self.controller(phandle, controllers) else {
						device.interrupts[device.interrupt_count] = NodeInterrupt::Unresolved;
						device.interrupt_count += 1;
						return;
					};
					let cells = controller.interrupt_cells;
					if cells == 0 || cells as usize > MAX_INTERRUPT_CELLS || at + 4 + cells * 4 > len {
						device.interrupts[device.interrupt_count] = NodeInterrupt::Unresolved;
						device.interrupt_count += 1;
						return;
					}
					device.interrupts[device.interrupt_count] = self.specifier(controller, value + at as u64 + 4);
					device.interrupt_count += 1;
					at += 4 + cells * 4;
				}
				return;
			}
			let Some((value, len)) = node.interrupts else { return };
			let Some(controller) = self.controller(inherited, controllers) else {
				device.interrupts[0] = NodeInterrupt::Unresolved;
				device.interrupt_count = 1;
				return;
			};
			let cells = controller.interrupt_cells;
			if cells == 0 || cells as usize > MAX_INTERRUPT_CELLS {
				device.interrupts[0] = NodeInterrupt::Unresolved;
				device.interrupt_count = 1;
				return;
			}
			let mut at = 0u32;
			while at + cells * 4 <= len && device.interrupt_count < MAX_NODE_INTERRUPTS {
				device.interrupts[device.interrupt_count] = self.specifier(controller, value + at as u64);
				device.interrupt_count += 1;
				at += cells * 4;
			}
		}
	}

	// One specifier at `at`, as the controller it names makes it.
	unsafe fn specifier(&self, controller: Controller, at: u64) -> NodeInterrupt {
		let mut spec = [0u32; MAX_INTERRUPT_CELLS];
		for (index, cell) in spec.iter_mut().enumerate().take(controller.interrupt_cells as usize) {
			*cell = unsafe { self.be32(at + index as u64 * 4) };
		}
		if controller.gpio_controller && controller.interrupt_controller {
			return NodeInterrupt::GpioLine { controller: controller.phandle, line: spec[0], flags: if controller.interrupt_cells > 1 { spec[1] } else { 0 } };
		}
		if controller.interrupt_controller {
			return NodeInterrupt::Controller(IntxRoute { controller: controller.phandle, cells: controller.interrupt_cells, spec });
		}
		// A NEXUS - an `interrupt-map` with no `interrupt-controller` of its own - or a parent that is neither.
		NodeInterrupt::Unresolved
	}

	// The controller a phandle names, remembered for the rest of the walk.
	unsafe fn controller(&self, phandle: u32, cache: &mut [Controller; 16]) -> Option<Controller> {
		if phandle == 0 || phandle == u32::MAX {
			return None;
		}
		if let Some(known) = cache.iter().find(|known| known.phandle == phandle) {
			return Some(*known);
		}
		let found = unsafe { self.controller_by_phandle(phandle)? };
		if let Some(slot) = cache.iter_mut().find(|slot| slot.phandle == 0) {
			*slot = found;
		}
		Some(found)
	}

	unsafe fn controller_by_phandle(&self, phandle: u32) -> Option<Controller> {
		let b = self.bounds()?;
		unsafe {
			let mut p = b.struct_start;
			let mut depth: i32 = -1;
			let mut found = [Controller::default(); MAX_DEPTH + 1];
			loop {
				let token = self.be32_in(p, b.struct_end)?;
				p += 4;
				match token {
					FDT_BEGIN_NODE => {
						depth += 1;
						let (_, next) = self.node_name_in(p, &b)?;
						p = next;
						if depth as usize > MAX_DEPTH {
							return None;
						}
						found[depth as usize] = Controller::default();
					}
					FDT_END_NODE => {
						if depth < 0 {
							return None;
						}
						if found[depth as usize].phandle == phandle {
							return Some(found[depth as usize]);
						}
						depth -= 1;
					}
					FDT_PROP => {
						let (pname, len, value, next) = self.prop_in(p, &b)?;
						p = next;
						if depth < 0 {
							return None;
						}
						let here = &mut found[depth as usize];
						if len == 4 && (self.str_eq(pname, "phandle") || self.str_eq(pname, "linux,phandle")) {
							here.phandle = self.be32(value);
						} else if len == 4 && self.str_eq(pname, "#interrupt-cells") {
							here.interrupt_cells = self.be32(value);
						} else if self.str_eq(pname, "interrupt-controller") {
							here.interrupt_controller = true;
						} else if self.str_eq(pname, "gpio-controller") {
							here.gpio_controller = true;
						}
					}
					FDT_NOP => {}
					_ => return None,
				}
			}
		}
	}

	// THE NODE'S PROPERTY BLOCK, in the records `abi::DEVICE_PROPERTY_*` describes, into `out`: its own
	// properties, then each child node and its properties, with a `clocks` reference to a `fixed-clock`
	// resolved to the frequency and every reference this reader cannot resolve - another clock, a reset, a
	// regulator supply, a pinctrl state, a power domain, a PHY - named instead of copied. Returns the bytes
	// written and whether anything was unresolved or cut at the bound.
	pub fn property_block(&self, node: &DeviceNode, out: &mut [u8]) -> PropertyBlock {
		// SAFETY: the span is the one the walk recorded inside the struct block; `bounds` is re-checked.
		unsafe { self.property_block_inner(node.span, out).unwrap_or(PropertyBlock { len: 0, unresolved: true }) }
	}

	unsafe fn property_block_inner(&self, span: (u64, u64), out: &mut [u8]) -> Option<PropertyBlock> {
		let b = self.bounds()?;
		if span.0 < b.struct_start || span.1 > b.struct_end || span.0 >= span.1 {
			return None;
		}
		let mut block = PropertyBlock { len: 0, unresolved: false };
		unsafe {
			let mut p = span.0;
			let mut depth: i32 = -1;
			while p < span.1 {
				let token = self.be32_in(p, span.1)?;
				p += 4;
				match token {
					FDT_BEGIN_NODE => {
						depth += 1;
						let (name, next) = self.node_name_in(p, &b)?;
						p = next;
						if depth > 0 {
							let len = self.str_len_in(name, b.struct_end)? as usize;
							let mut buffer = [0u8; MAX_PATH];
							if len > MAX_PATH {
								block.unresolved = true;
								continue;
							}
							for (i, slot) in buffer.iter_mut().enumerate().take(len) {
								*slot = self.u8_at(name + i as u64);
							}
							if !block.record(out, 1, depth as u8, &buffer[..len], &[]) {
								return Some(block);
							}
						}
					}
					FDT_END_NODE => {
						depth -= 1;
						if depth < 0 {
							return Some(block);
						}
					}
					FDT_PROP => {
						let (pname, len, value, next) = self.prop_in(p, &b)?;
						p = next;
						let name_len = self.str_len_in(pname, b.strings_end)? as usize;
						let mut name = [0u8; MAX_PATH];
						if name_len > MAX_PATH {
							block.unresolved = true;
							continue;
						}
						for (i, slot) in name.iter_mut().enumerate().take(name_len) {
							*slot = self.u8_at(pname + i as u64);
						}
						let name = &name[..name_len];
						let depth = depth.max(0) as u8;
						if name == b"clocks" {
							// Each entry is a phandle and the provider's `#clock-cells` more cells.
							let mut at = 0u32;
							while at + 4 <= len {
								let phandle = self.be32(value + at as u64);
								match self.clock_by_phandle(phandle) {
									Some((Some(frequency), clock_cells)) => {
										if !block.record(out, 3, depth, name, &frequency.to_le_bytes()) {
											return Some(block);
										}
										at += 4 + clock_cells * 4;
									}
									Some((None, clock_cells)) => {
										block.unresolved = true;
										if !block.record(out, 4, depth, name, &[]) {
											return Some(block);
										}
										at += 4 + clock_cells * 4;
									}
									None => {
										block.unresolved = true;
										if !block.record(out, 4, depth, name, &[]) {
											return Some(block);
										}
										break;
									}
								}
							}
							continue;
						}
						if name == b"resets" || name == b"power-domains" || name == b"phys" || name.ends_with(b"-supply") || (name.starts_with(b"pinctrl-") && name != b"pinctrl-names") {
							block.unresolved = true;
							if !block.record(out, 4, depth, name, &[]) {
								return Some(block);
							}
							continue;
						}
						let mut copied = [0u8; 256];
						if len as usize > copied.len() {
							// A value too long for one record is named as unresolved rather than cut.
							block.unresolved = true;
							if !block.record(out, 4, depth, name, &[]) {
								return Some(block);
							}
							continue;
						}
						for (i, slot) in copied.iter_mut().enumerate().take(len as usize) {
							*slot = self.u8_at(value + i as u64);
						}
						if !block.record(out, 2, depth, name, &copied[..len as usize]) {
							return Some(block);
						}
					}
					FDT_NOP => {}
					_ => return None,
				}
			}
		}
		Some(block)
	}

	// The clock a phandle names: its frequency when it is a `fixed-clock`, and its `#clock-cells`.
	unsafe fn clock_by_phandle(&self, phandle: u32) -> Option<(Option<u64>, u32)> {
		let b = self.bounds()?;
		unsafe {
			let mut p = b.struct_start;
			let mut depth: i32 = -1;
			let mut facts = [(0u32, false, None::<u64>, 0u32); MAX_DEPTH + 1];
			loop {
				let token = self.be32_in(p, b.struct_end)?;
				p += 4;
				match token {
					FDT_BEGIN_NODE => {
						depth += 1;
						let (_, next) = self.node_name_in(p, &b)?;
						p = next;
						if depth as usize > MAX_DEPTH {
							return None;
						}
						facts[depth as usize] = (0, false, None, 0);
					}
					FDT_END_NODE => {
						if depth < 0 {
							return None;
						}
						let (found, fixed, frequency, cells) = facts[depth as usize];
						if found == phandle && phandle != 0 {
							return Some((if fixed { frequency } else { None }, cells));
						}
						depth -= 1;
					}
					FDT_PROP => {
						let (pname, len, value, next) = self.prop_in(p, &b)?;
						p = next;
						if depth < 0 {
							return None;
						}
						let here = &mut facts[depth as usize];
						if len == 4 && (self.str_eq(pname, "phandle") || self.str_eq(pname, "linux,phandle")) {
							here.0 = self.be32(value);
						} else if self.str_eq(pname, "compatible") {
							here.1 = self.stringlist_contains(value, len, b"fixed-clock");
						} else if self.str_eq(pname, "clock-frequency") {
							here.2 = match len {
								4 => Some(u64::from(self.be32(value))),
								8 => Some((u64::from(self.be32(value)) << 32) | u64::from(self.be32(value + 4))),
								_ => None,
							};
						} else if len == 4 && self.str_eq(pname, "#clock-cells") {
							here.3 = self.be32(value).min(4);
						}
					}
					FDT_NOP => {}
					_ => return None,
				}
			}
		}
	}
}

// What `property_block` wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PropertyBlock {
	pub len: usize,
	// A reference the kernel could not resolve, or a block cut at its bound.
	pub unresolved: bool,
}

impl PropertyBlock {
	// One record, whole or not at all; false when it did not fit, which ends the block.
	fn record(&mut self, out: &mut [u8], kind: u8, depth: u8, name: &[u8], value: &[u8]) -> bool {
		let padded = (value.len() + 3) & !3;
		let need = 8 + name.len() + padded;
		if self.len + need > out.len() || name.len() > u16::MAX as usize {
			self.unresolved = true;
			return false;
		}
		let at = self.len;
		out[at] = kind;
		out[at + 1] = depth;
		out[at + 2..at + 4].copy_from_slice(&(name.len() as u16).to_le_bytes());
		out[at + 4..at + 8].copy_from_slice(&(value.len() as u32).to_le_bytes());
		out[at + 8..at + 8 + name.len()].copy_from_slice(name);
		out[at + 8 + name.len()..at + 8 + name.len() + value.len()].copy_from_slice(value);
		for byte in &mut out[at + 8 + name.len() + value.len()..at + need] {
			*byte = 0;
		}
		self.len += need;
		true
	}
}
