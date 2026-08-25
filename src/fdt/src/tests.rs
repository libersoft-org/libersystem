// The parser, run against the two machines this system actually boots on, and against trees built
// to be awkward.
//
// THE FIXTURES ARE REAL. `qemu-virt-aarch64.dtb` and `qemu-virt-riscv64.dtb` are what
// `qemu-system-<arch> -machine virt,dumpdtb=...` produces - the same bytes the firmware hands the
// loader at boot. That matters more here than usual: this crate exists so the loader can stop
// hard-coding `0x0900_0000` and `0x1000_0000`, and the only convincing evidence that the discovery
// works is that it finds those two addresses in the trees the machine really provides.
//
// The synthesised trees cover what QEMU never produces: a console the tree does not name, a chip
// this system has no driver for, a console under a bus whose `#address-cells` differ from the
// root's, and an alias pointing somewhere that is not there.

use super::*;

// The kernel reaches the blob through its direct map and the loader reads it where it lies; a host
// test reads a `&'static [u8]` at its own address, which is the same shape as the loader's.
fn identity(address: u64) -> u64 {
	address
}

// THE SAFETY ARGUMENT FOR THIS WHOLE FILE, made once (FDT-007).
//
// `Fdt::new` is `unsafe` because it takes a physical address it will dereference, and nothing in
// `base: u64` says the number is a device tree. Here it always is: `blob` is a `&'static [u8]` this
// process owns - either an `include_bytes!` fixture or a leaked `Vec` from the builder below - and
// `identity` is the correct translation for an address that is already virtual. Forty-eight call
// sites would have carried forty-eight `unsafe` blocks stating none of that.
fn at(blob: &'static [u8]) -> Fdt {
	unsafe { Fdt::new(blob.as_ptr() as u64, identity) }
}

const AARCH64: &[u8] = include_bytes!("../tests/qemu-virt-aarch64.dtb");
const RISCV64: &[u8] = include_bytes!("../tests/qemu-virt-riscv64.dtb");
// The machine the riscv64 harness actually runs: `virt,aia=aplic-imsic`, whose tree describes two
// IMSICs - the firmware's and this kernel's.
const RISCV64_AIA: &[u8] = include_bytes!("../tests/qemu-virt-riscv64-aia.dtb");

// ---------------------------------------------------------------------------------------------
// A tree builder, so the awkward cases can be written rather than found.
// ---------------------------------------------------------------------------------------------

const FDT_HEADER_LEN: usize = 40;

struct Builder {
	structure: Vec<u8>,
	strings: Vec<u8>,
	// The memory reservation block's entries, which the fixtures could not express until the parser
	// could read them.
	reserved: Vec<(u64, u64)>,
}

impl Builder {
	fn new() -> Self {
		Self { structure: Vec::new(), strings: Vec::new(), reserved: Vec::new() }
	}

	fn reserve(&mut self, base: u64, size: u64) -> &mut Self {
		self.reserved.push((base, size));
		self
	}

	fn token(&mut self, token: u32) {
		self.structure.extend_from_slice(&token.to_be_bytes());
	}

	fn pad(&mut self) {
		while self.structure.len() % 4 != 0 {
			self.structure.push(0);
		}
	}

	fn begin(&mut self, name: &str) -> &mut Self {
		self.token(FDT_BEGIN_NODE);
		self.structure.extend_from_slice(name.as_bytes());
		self.structure.push(0);
		self.pad();
		self
	}

	fn end(&mut self) -> &mut Self {
		self.token(FDT_END_NODE);
		self
	}

	// Strings are deduplicated the way a real writer does, so a tree with two `reg` properties has
	// one `reg` in its strings block - which is the case a naive reader gets right by accident.
	fn string(&mut self, name: &str) -> u32 {
		let want = name.as_bytes();
		let mut at = 0usize;
		while at < self.strings.len() {
			let end = self.strings[at..].iter().position(|byte| *byte == 0).expect("terminated") + at;
			if &self.strings[at..end] == want {
				return at as u32;
			}
			at = end + 1;
		}
		let offset = self.strings.len() as u32;
		self.strings.extend_from_slice(want);
		self.strings.push(0);
		offset
	}

	fn prop(&mut self, name: &str, value: &[u8]) -> &mut Self {
		let offset = self.string(name);
		self.token(FDT_PROP);
		self.structure.extend_from_slice(&(value.len() as u32).to_be_bytes());
		self.structure.extend_from_slice(&offset.to_be_bytes());
		self.structure.extend_from_slice(value);
		self.pad();
		self
	}

	fn prop_u32(&mut self, name: &str, value: u32) -> &mut Self {
		self.prop(name, &value.to_be_bytes())
	}

	fn prop_str(&mut self, name: &str, value: &str) -> &mut Self {
		let mut bytes = value.as_bytes().to_vec();
		bytes.push(0);
		self.prop(name, &bytes)
	}

	// Two 64-bit cells: the `reg` shape of every node in both QEMU trees.
	fn prop_reg64(&mut self, base: u64, size: u64) -> &mut Self {
		let mut bytes = base.to_be_bytes().to_vec();
		bytes.extend_from_slice(&size.to_be_bytes());
		self.prop("reg", &bytes)
	}

	fn finish(&mut self) -> &'static [u8] {
		self.token(FDT_END);
		let structure = core::mem::take(&mut self.structure);
		let strings = core::mem::take(&mut self.strings);
		// Header, the memory-reservation block (its entries and the zero terminator), then the two
		// blocks.
		let reserved = core::mem::take(&mut self.reserved);
		let rsvmap_len = (reserved.len() + 1) * 16;
		let off_rsvmap = FDT_HEADER_LEN;
		let off_struct = off_rsvmap + rsvmap_len;
		let off_strings = off_struct + structure.len();
		let total = off_strings + strings.len();
		let mut out = Vec::with_capacity(total);
		for word in [FDT_MAGIC, total as u32, off_struct as u32, off_strings as u32, off_rsvmap as u32, 17, 16, 0, strings.len() as u32, structure.len() as u32] {
			out.extend_from_slice(&word.to_be_bytes());
		}
		for (base, size) in &reserved {
			out.extend_from_slice(&base.to_be_bytes());
			out.extend_from_slice(&size.to_be_bytes());
		}
		out.extend_from_slice(&[0u8; 16]);
		out.extend_from_slice(&structure);
		out.extend_from_slice(&strings);
		// Leaked on purpose: `Fdt` holds a physical address rather than a borrow, so the bytes have
		// to outlive the reader the way the firmware's blob does.
		Vec::leak(out)
	}
}

// A minimal machine: a root, a memory node, one cpu, and whatever `body` adds.
fn machine(body: impl FnOnce(&mut Builder)) -> &'static [u8] {
	let mut builder = Builder::new();
	builder.begin("");
	builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	builder.begin("memory@40000000").prop("device_type", b"memory\0").prop_reg64(0x4000_0000, 0x2000_0000).end();
	builder.begin("cpus").prop_u32("#address-cells", 1).prop_u32("#size-cells", 0).begin("cpu@0").prop_u32("reg", 0).end().end();
	body(&mut builder);
	builder.end();
	builder.finish()
}

#[test]
fn a_hole_in_the_memory_map_is_not_reported_as_memory() {
	// THE ARITHMETIC USED TO CLAIM THE HOLE. `ram_base` was the first bank's base and `ram_size` the
	// SUM of every bank's size, so two 256 MiB banks either side of a gap produced base 0x4000_0000
	// and size 512 MiB - a range whose second half is the hole, and whose end stops short of the
	// second bank. The kernel builds its usable frame region from exactly `ram_base + ram_size`, so
	// it would have handed out frames addressing nothing.
	//
	// That failure is unattributable at the point it bites: a store into a hole goes nowhere and the
	// data is simply gone, with no fault, in whatever subsystem happened to be handed that frame.
	let mut builder = Builder::new();
	builder.begin("");
	builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	// Two banks with a gap between them, in ONE `reg` - which is how a device tree usually says it.
	let mut reg = 0x4000_0000u64.to_be_bytes().to_vec();
	reg.extend_from_slice(&0x1000_0000u64.to_be_bytes());
	reg.extend_from_slice(&0x8000_0000u64.to_be_bytes());
	reg.extend_from_slice(&0x1000_0000u64.to_be_bytes());
	builder.begin("memory@40000000").prop("device_type", b"memory\0").prop("reg", &reg).end();
	builder.begin("cpus").prop_u32("#address-cells", 1).prop_u32("#size-cells", 0).begin("cpu@0").prop_u32("reg", 0).end().end();
	builder.end();
	let info = at(builder.finish()).parse().expect("the machine describes its memory");
	assert_eq!(info.ram_base, 0x4000_0000, "the run starts at the first bank");
	assert_eq!(info.ram_size, 0x1000_0000, "and ends where the first bank does - the hole is not memory");

	// AND BOTH BANKS ARE CARRIED, which is the half the run cannot express. Reporting only the run
	// was safe and lossy - the second bank's 256 MiB were simply never used - and the frame
	// allocator has taken a LIST of regions since it existed; the reader was the side collapsing it.
	assert_eq!(info.ram_region_count, 2, "two banks, two regions");
	assert_eq!(info.ram_regions[0], (0x4000_0000, 0x1000_0000));
	assert_eq!(info.ram_regions[1], (0x8000_0000, 0x1000_0000), "the memory past the hole is not lost any more");

	// AND CONTIGUOUS BANKS STILL JOIN, which is the case this must not break: a tree that describes
	// one range in two pieces is describing one range.
	let mut builder = Builder::new();
	builder.begin("");
	builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	let mut reg = 0x4000_0000u64.to_be_bytes().to_vec();
	reg.extend_from_slice(&0x1000_0000u64.to_be_bytes());
	reg.extend_from_slice(&0x5000_0000u64.to_be_bytes());
	reg.extend_from_slice(&0x1000_0000u64.to_be_bytes());
	builder.begin("memory@40000000").prop("device_type", b"memory\0").prop("reg", &reg).end();
	builder.begin("cpus").prop_u32("#address-cells", 1).prop_u32("#size-cells", 0).begin("cpu@0").prop_u32("reg", 0).end().end();
	builder.end();
	let info = at(builder.finish()).parse().expect("parses");
	assert_eq!(info.ram_base, 0x4000_0000);
	assert_eq!(info.ram_region_count, 1, "one range described in two pieces is one region");
	assert_eq!(info.ram_size, 0x2000_0000, "two touching banks are one range");

	// AND A BANK THAT EXTENDS THE RUN DOWNWARDS joins too, because a device tree does not promise
	// its banks are in address order.
	let mut builder = Builder::new();
	builder.begin("");
	builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	let mut reg = 0x5000_0000u64.to_be_bytes().to_vec();
	reg.extend_from_slice(&0x1000_0000u64.to_be_bytes());
	reg.extend_from_slice(&0x4000_0000u64.to_be_bytes());
	reg.extend_from_slice(&0x1000_0000u64.to_be_bytes());
	builder.begin("memory@50000000").prop("device_type", b"memory\0").prop("reg", &reg).end();
	builder.begin("cpus").prop_u32("#address-cells", 1).prop_u32("#size-cells", 0).begin("cpu@0").prop_u32("reg", 0).end().end();
	builder.end();
	let info = at(builder.finish()).parse().expect("parses");
	assert_eq!(info.ram_base, 0x4000_0000, "the run grew downwards");
	assert_eq!(info.ram_size, 0x2000_0000);
}

#[test]
fn a_property_shorter_than_the_reader_assumes_is_not_read_past_its_end() {
	// `prop_in` proves a property's declared value lies inside the structure block. It does not
	// prove the value is as long as a particular reader assumes - and three readers assumed.
	//
	// A one-byte `#address-cells` had `be32` taking three bytes of whatever followed it. The value
	// stayed inside the blob, so no bounds check fired; what it corrupted was the CELL COUNT, which
	// then decided how every `reg` in the tree was parsed.
	let fdt = at(machine(|b| {
		b.begin("chosen").prop("#address-cells", &[2u8]).end();
	}));
	// The tree still parses - a short property is refused, not the whole blob - and the geometry is
	// the root's, which is what a refused override means.
	let info = fdt.parse().expect("a short property refuses itself rather than the tree");
	assert_eq!(info.ram_base, 0x4000_0000, "the root's cells still describe the memory");

	// The initrd properties took a four-byte read for any length that was not eight, including
	// zero, one, two and three.
	for bytes in [&[][..], &[1u8], &[1, 2], &[1, 2, 3], &[1, 2, 3, 4, 5]] {
		let fdt = at(machine(|b| {
			b.begin("chosen").prop("linux,initrd-start", bytes).end();
		}));
		let info = fdt.parse().expect("parses");
		assert_eq!(info.modules_start, 0, "a property of {} bytes names no address", bytes.len());
	}
	// And the two widths a device tree really writes are still read.
	let fdt = at(machine(|b| {
		b.begin("chosen").prop("linux,initrd-start", &0x4800_0000u32.to_be_bytes()).prop("linux,initrd-end", &0x4900_0000u64.to_be_bytes()).end();
	}));
	let info = fdt.parse().expect("parses");
	assert_eq!(info.modules_start, 0x4800_0000, "four bytes is one cell");
	assert_eq!(info.modules_end, 0x4900_0000, "eight bytes is two");
}

// ---------------------------------------------------------------------------------------------
// The machines this system boots on.
// ---------------------------------------------------------------------------------------------

#[test]
fn the_real_aarch64_virt_tree_parses_into_the_geometry_the_kernel_boots_on() {
	let fdt = at(AARCH64);
	assert!(fdt.is_valid(), "a tree QEMU produced is a valid tree");
	let info = fdt.parse().expect("the machine describes its memory");
	assert_eq!(info.ram_base, 0x4000_0000, "virt puts RAM at 1 GiB");
	assert!(info.ram_size >= 0x800_0000, "and QEMU's default is 128 MiB or more, not zero");
	assert_eq!(info.cpu_count, 1, "one core unless -smp says otherwise");
	assert_eq!(info.pcie_ecam, 0x40_1000_0000, "the high ECAM window virt describes, in the root's two address cells");
}

#[test]
fn the_real_riscv64_virt_tree_parses_and_carries_the_timebase() {
	let fdt = at(RISCV64);
	assert!(fdt.is_valid());
	let info = fdt.parse().expect("the machine describes its memory");
	assert_eq!(info.ram_base, 0x8000_0000);
	assert!(info.plic_base != 0, "riscv64 virt has a PLIC and the kernel needs its address");
	// The constant this replaced was 10,000,000 with a comment naming QEMU. The tree says so
	// itself, which is the whole point of reading it - and until this test existed it did not:
	// the walk started its depth at zero, so `/cpus` was never recognised and every riscv64 boot
	// printed "no /cpus/timebase-frequency in the device tree" and used the constant anyway.
	assert_eq!(fdt.timebase_frequency(), Some(10_000_000), "QEMU virt ticks at 10 MHz and says so");
}

#[test]
fn the_console_of_each_real_machine_is_found_at_the_address_that_was_hard_coded() {
	// THE LOAD-BEARING TEST OF THIS CRATE. Both loaders carry a UART address written down from
	// QEMU - `0x0900_0000` for aarch64 and `0x1000_0000` for riscv64 - and the audit has recorded
	// twice that on a machine that is not `virt` those stores go somewhere nobody chose.
	//
	// Discovery replaces the constant, and this is the evidence that it replaces it with the SAME
	// answer on the machine the constant was right for. If these two lines ever disagree with the
	// backends' fallbacks, one of the two is describing a machine that no longer exists.
	let arm = at(AARCH64).console().expect("aarch64 virt names its console");
	assert_eq!(arm, Console { uart: Uart::Pl011, base: 0x0900_0000, reg_shift: 0, reg_io_width: 1 });

	let risc = at(RISCV64).console().expect("riscv64 virt names its console");
	assert_eq!(risc, Console { uart: Uart::Ns16550, base: 0x1000_0000, reg_shift: 0, reg_io_width: 1 });
}

#[test]
fn the_riscv64_console_is_reached_through_an_alias_and_a_bus() {
	// Worth its own test because it exercises the two things the aarch64 tree does not: the path
	// is an ALIAS (`stdout-path = "serial0"`), and the node is not at the root but under `/soc`,
	// whose `#address-cells` is what its children's `reg` is expressed in. A reader that used the
	// root's cells, or that only understood absolute paths, finds nothing here - and "nothing" is
	// indistinguishable from "this machine has no console" unless something tests it.
	let fdt = at(RISCV64);
	let console = fdt.console().expect("through the alias and the bus");
	assert_eq!(console.base, 0x1000_0000);
}

// ---------------------------------------------------------------------------------------------
// What QEMU never produces.
// ---------------------------------------------------------------------------------------------

#[test]
fn a_tree_that_names_no_console_answers_none() {
	// The case the whole change is for. A machine that does not say where its console is gets
	// silence rather than a store to an address this code invented.
	let blob = machine(|builder| {
		builder.begin("pl011@9000000").prop_str("compatible", "arm,pl011").prop_reg64(0x0900_0000, 0x1000).end();
	});
	assert!(at(blob).is_valid());
	assert_eq!(at(blob).console(), None, "the UART is in the tree and nothing points at it, so nothing is chosen");
}

#[test]
fn a_console_this_system_has_no_driver_for_is_not_guessed_at() {
	// A real machine with a real console that is neither a PL011 nor a 16550. Answering "PL011 at
	// that address" because it was the first branch would put bytes into a device whose registers
	// mean something else.
	let blob = machine(|builder| {
		builder.begin("chosen").prop_str("stdout-path", "/serial@10000000").end();
		builder.begin("serial@10000000").prop_str("compatible", "fsl,imx21-uart").prop_reg64(0x1000_0000, 0x1000).end();
	});
	assert_eq!(at(blob).console(), None);
}

#[test]
fn a_compatible_list_is_read_past_its_first_entry() {
	// `compatible` is most-specific-first, so a board that names its own part number followed by
	// the generic one is ordinary. Reading only the first entry answers None for a machine this
	// system can perfectly well drive.
	let mut value = b"acme,uart-v3\0ns16550a\0".to_vec();
	value.push(0);
	let blob = machine(|builder| {
		builder.begin("chosen").prop_str("stdout-path", "/serial@10000000").end();
		builder.begin("serial@10000000").prop("compatible", &value).prop_reg64(0x1000_0000, 0x1000).end();
	});
	assert_eq!(at(blob).console().map(|console| console.uart), Some(Uart::Ns16550));
}

#[test]
fn the_options_after_the_colon_are_not_part_of_the_path() {
	// `stdout-path = "/serial@10000000:115200n8"` is the specification's form and it is what
	// riscv64's tree writes through its alias. A reader that took the whole string as a path finds
	// no such node.
	let blob = machine(|builder| {
		builder.begin("chosen").prop_str("stdout-path", "/serial@10000000:115200n8").end();
		builder.begin("serial@10000000").prop_str("compatible", "ns16550a").prop_reg64(0x1000_0000, 0x1000).end();
	});
	assert_eq!(at(blob).console().map(|console| console.base), Some(0x1000_0000));
}

#[test]
fn a_bus_with_its_own_address_cells_decides_how_its_children_are_read() {
	// The defect a reader gets by assuming the root's cells everywhere: a bus declaring
	// `#address-cells = 1` writes its children's `reg` as ONE cell, and reading two takes the size
	// as the top half of the address. The answer is not merely wrong, it is enormous.
	let mut builder = Builder::new();
	builder.begin("");
	builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	builder.begin("memory@40000000").prop("device_type", b"memory\0").prop_reg64(0x4000_0000, 0x2000_0000).end();
	builder.begin("chosen").prop_str("stdout-path", "/soc/serial@10000000").end();
	// An empty `ranges` - the identity mapping - because a bus that declares none does not make its
	// children reachable from its parent at all (FDT-003), which the test below covers.
	builder.begin("soc").prop_u32("#address-cells", 1).prop_u32("#size-cells", 1).prop("ranges", &[]);
	let mut reg = 0x1000_0000u32.to_be_bytes().to_vec();
	reg.extend_from_slice(&0x1000u32.to_be_bytes());
	builder.begin("serial@10000000").prop_str("compatible", "ns16550a").prop("reg", &reg).end();
	builder.end().end();
	let blob = builder.finish();
	assert_eq!(at(blob).console().map(|console| console.base), Some(0x1000_0000), "one cell means one cell");
}

// A machine whose console sits behind `bus`, built by `bus_props`, at child address `child`.
fn console_behind_bus(bus_props: impl FnOnce(&mut Builder), child: u64) -> &'static [u8] {
	let mut builder = Builder::new();
	builder.begin("");
	builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	builder.begin("memory@40000000").prop("device_type", b"memory\0").prop_reg64(0x4000_0000, 0x2000_0000).end();
	builder.begin("chosen").prop_str("stdout-path", "/soc/serial@1000").end();
	builder.begin("soc");
	bus_props(&mut builder);
	let mut reg = child.to_be_bytes().to_vec();
	reg.extend_from_slice(&0x1000u64.to_be_bytes());
	builder.begin("serial@1000").prop_str("compatible", "ns16550a").prop("reg", &reg).end();
	builder.end().end();
	builder.finish()
}

#[test]
fn a_device_behind_a_bus_is_reached_at_the_address_the_bus_translates_it_to() {
	// FDT-003. A `reg` is an address on the PARENT BUS, and `ranges` is how that bus says where its
	// children land in ITS parent's space. This reader returned the raw child address - the right
	// offset on the wrong bus - so a UART at child 0x1000 behind a window based at 0x9000_0000 was
	// driven at physical 0x1000, which on most machines is RAM.
	let mapped = console_behind_bus(
		|builder| {
			builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
			// <child 0x0 -> parent 0x9000_0000, 0x1_0000 long>
			let mut ranges = 0u64.to_be_bytes().to_vec();
			ranges.extend_from_slice(&0x9000_0000u64.to_be_bytes());
			ranges.extend_from_slice(&0x1_0000u64.to_be_bytes());
			builder.prop("ranges", &ranges);
		},
		0x1000,
	);
	assert_eq!(at(mapped).console().map(|console| console.base), Some(0x9000_1000), "the child address is translated through the bus window");

	// An address outside every window the bus declares does not translate, and an untranslatable
	// address is not a console: answering the raw number would be answering with an address on a
	// different bus.
	let outside = console_behind_bus(
		|builder| {
			builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
			let mut ranges = 0u64.to_be_bytes().to_vec();
			ranges.extend_from_slice(&0x9000_0000u64.to_be_bytes());
			ranges.extend_from_slice(&0x800u64.to_be_bytes()); // the window ends before 0x1000
			builder.prop("ranges", &ranges);
		},
		0x1000,
	);
	assert_eq!(at(outside).console(), None, "an address in no window of its bus does not translate");

	// And a bus with NO `ranges` does not make its children reachable at all. This is the case the
	// old reader answered with the untranslated address.
	let unreachable = console_behind_bus(
		|builder| {
			builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
		},
		0x1000,
	);
	assert_eq!(at(unreachable).console(), None, "a bus that declares no ranges does not translate its children");
}

#[test]
fn a_dw_apb_uart_carries_its_register_spacing() {
	// `snps,dw-apb-uart` is a 16550 whose registers are four bytes apart, and `reg-shift` is how
	// the tree says so. A driver that ignored it would write the character into the interrupt-enable
	// register - which is the same class of mistake as the wrong base address, one register over.
	let blob = machine(|builder| {
		builder.begin("chosen").prop_str("stdout-path", "/serial@10000000").end();
		builder.begin("serial@10000000").prop_str("compatible", "snps,dw-apb-uart").prop_reg64(0x1000_0000, 0x1000).prop_u32("reg-shift", 2).end();
	});
	assert_eq!(at(blob).console(), Some(Console { uart: Uart::Ns16550, base: 0x1000_0000, reg_shift: 2, reg_io_width: 1 }));
}

#[test]
fn an_alias_that_names_nothing_answers_none() {
	let blob = machine(|builder| {
		builder.begin("chosen").prop_str("stdout-path", "serial0").end();
		builder.begin("aliases").prop_str("serial0", "/serial@10000000").end();
	});
	assert_eq!(at(blob).console(), None, "the alias resolves and the node it names is not in the tree");

	let blob = machine(|builder| {
		builder.begin("chosen").prop_str("stdout-path", "serial9").end();
		builder.begin("aliases").prop_str("serial0", "/serial@10000000").end();
	});
	assert_eq!(at(blob).console(), None, "and an alias that is not there is not a path either");
}

#[test]
fn a_node_without_a_reg_is_not_a_console_at_address_zero() {
	// Zero is an address, and it is the address a missing `reg` decodes to if nothing checks. A
	// loader writing to physical zero is a loader writing over the exception vectors on one of
	// these two architectures.
	let blob = machine(|builder| {
		builder.begin("chosen").prop_str("stdout-path", "/serial@10000000").end();
		builder.begin("serial@10000000").prop_str("compatible", "ns16550a").end();
	});
	assert_eq!(at(blob).console(), None);
}

#[test]
fn a_path_component_matches_a_node_with_a_unit_address_but_not_a_longer_name() {
	// `/soc/serial` must find `serial@10000000`, because a stdout path written by hand often omits
	// the unit address - and it must NOT find `serial0@...` or `serialport@...`, which is what a
	// plain prefix test would do.
	let blob = machine(|builder| {
		builder.begin("chosen").prop_str("stdout-path", "/serial").end();
		builder.begin("serialport@20000000").prop_str("compatible", "ns16550a").prop_reg64(0x2000_0000, 0x1000).end();
		builder.begin("serial@10000000").prop_str("compatible", "ns16550a").prop_reg64(0x1000_0000, 0x1000).end();
	});
	assert_eq!(at(blob).console().map(|console| console.base), Some(0x1000_0000), "the unit-address match, and not the longer name before it");
}

#[test]
fn a_blob_that_is_not_a_device_tree_answers_none_rather_than_walking_it() {
	// The loader hands this whatever the firmware put in its configuration table, and a table
	// entry that is not a DTB is a pointer into something else. Every entry point checks the
	// header first; this pins that they all do, because one that did not would walk arbitrary
	// memory as a token stream.
	let rubbish: &'static [u8] = Vec::leak(vec![0x5au8; 4096]);
	let fdt = at(rubbish);
	assert!(!fdt.is_valid());
	assert_eq!(fdt.console(), None);
	assert!(fdt.parse().is_none());
	assert_eq!(fdt.timebase_frequency(), None);
	assert!(!fdt.has_isa_extension(b"svpbmt"));
}

// A token stream that never closes what it opened is refused (FDT-008).
#[test]
fn a_tree_that_ends_with_a_node_still_open_is_not_a_tree() {
	let mut builder = Builder::new();
	builder.begin("");
	builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	builder.begin("memory@40000000").prop("device_type", b"memory\0").prop_reg64(0x4000_0000, 0x2000_0000).end();
	// The root's own `end()` is missing, so `FDT_END` arrives at depth 0.
	let blob = builder.finish();
	let fdt = at(blob);
	assert!(fdt.is_valid(), "the header is well formed - it is the structure that is not");
	assert!(fdt.parse().is_none(), "a stream with a node still open is refused rather than read");
}

// And the balanced version of the same tree parses, so the refusal is about the imbalance.
#[test]
fn the_same_tree_parses_once_it_closes_its_root() {
	let mut builder = Builder::new();
	builder.begin("");
	builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	builder.begin("memory@40000000").prop("device_type", b"memory\0").prop_reg64(0x4000_0000, 0x2000_0000).end();
	builder.end();
	let info = at(builder.finish()).parse().expect("balanced, so it parses");
	assert_eq!(info.ram_base, 0x4000_0000);
}

// ---------------------------------------------------------------------------------------------
// ISA extensions: whole names, and every hart (FDT-004).
// ---------------------------------------------------------------------------------------------

// A machine with `count` harts, each carrying the `riscv,isa` and `riscv,isa-extensions` given.
fn harts(entries: &[(&str, &[&str])]) -> &'static [u8] {
	let mut builder = Builder::new();
	builder.begin("");
	builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	builder.begin("memory@80000000").prop("device_type", b"memory\0").prop_reg64(0x8000_0000, 0x1000_0000).end();
	builder.begin("cpus").prop_u32("#address-cells", 1).prop_u32("#size-cells", 0);
	for (index, (isa, extensions)) in entries.iter().enumerate() {
		builder.begin(&alloc_name(index));
		builder.prop_u32("reg", index as u32);
		if !isa.is_empty() {
			builder.prop_str("riscv,isa", isa);
		}
		if !extensions.is_empty() {
			let mut bytes: Vec<u8> = Vec::new();
			for name in *extensions {
				bytes.extend_from_slice(name.as_bytes());
				bytes.push(0);
			}
			builder.prop("riscv,isa-extensions", &bytes);
		}
		builder.end();
	}
	builder.end();
	builder.end();
	builder.finish()
}

fn alloc_name(index: usize) -> String {
	format!("cpu@{index}")
}

#[test]
fn a_single_letter_extension_is_read_out_of_the_base_string_and_not_out_of_a_name() {
	// `c` IS in `rv64imafdc`. It is NOT in `rv64ima_zicsr`, where the only `c` is inside `zicsr` -
	// which a substring scan reported as present.
	assert!(at(harts(&[("rv64imafdc", &[])])).has_isa_extension(b"c"));
	assert!(!at(harts(&[("rv64ima_zicsr", &[])])).has_isa_extension(b"c"));
}

#[test]
fn a_multi_letter_extension_is_a_whole_underscore_separated_name() {
	assert!(at(harts(&[("rv64imafdc_svpbmt", &[])])).has_isa_extension(b"svpbmt"));
	// `sv` is a prefix of `svpbmt` and is not itself declared.
	assert!(!at(harts(&[("rv64imafdc_svpbmt", &[])])).has_isa_extension(b"sv"));
	// And `pbmt` is an infix of it.
	assert!(!at(harts(&[("rv64imafdc_svpbmt", &[])])).has_isa_extension(b"pbmt"));
}

#[test]
fn the_extensions_stringlist_is_compared_element_by_element() {
	assert!(at(harts(&[("", &["zicsr", "svpbmt"])])).has_isa_extension(b"svpbmt"));
	assert!(!at(harts(&[("", &["zicsr", "svpbmt"])])).has_isa_extension(b"csr"));
	assert!(!at(harts(&[("", &["zicsr", "svpbmt"])])).has_isa_extension(b"zic"));
}

// THE ONE THAT MATTERS ON A REAL MACHINE. This answered `true` on the first hart that had the
// extension, and the kernel then used it on every hart - an illegal instruction on the others.
#[test]
fn an_extension_one_hart_lacks_is_not_this_machines_extension() {
	assert!(at(harts(&[("rv64imafdc_svpbmt", &[]), ("rv64imafdc_svpbmt", &[])])).has_isa_extension(b"svpbmt"), "both harts have it");
	assert!(!at(harts(&[("rv64imafdc_svpbmt", &[]), ("rv64imafdc", &[])])).has_isa_extension(b"svpbmt"), "the second does not, so the machine does not");
}

#[test]
fn a_tree_with_no_cpu_nodes_answers_false_rather_than_vacuously_true() {
	let mut builder = Builder::new();
	builder.begin("");
	builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	builder.begin("cpus").prop_u32("#address-cells", 1).prop_u32("#size-cells", 0).end();
	builder.end();
	assert!(!at(builder.finish()).has_isa_extension(b"svpbmt"), "'every hart' over no harts is not 'yes'");
}

// ---------------------------------------------------------------------------------------------
// PSCI: which instruction reaches the platform's implementation.
// ---------------------------------------------------------------------------------------------

#[test]
fn the_psci_conduit_comes_from_the_tree_rather_than_the_exception_level() {
	// THE MACHINE THIS SYSTEM BOOTS ON SAYS IT. The loader used to answer `PSCI_HVC` for any boot
	// that did not start at EL2, which is true of QEMU's `virt` and false of most server-class
	// AArch64 - where EL2 belongs to a hypervisor and PSCI lives in EL3 firmware behind `smc`.
	assert_eq!(at(AARCH64).psci_conduit(), Some(PsciConduit::Hvc), "QEMU virt's own tree states hvc, which is what the guess happened to be");

	// And a tree that states the other one is read as the other one.
	let smc = machine(|builder| {
		builder.begin("psci").prop_str("compatible", "arm,psci-1.0").prop_str("method", "smc").end();
	});
	assert_eq!(at(smc).psci_conduit(), Some(PsciConduit::Smc), "a platform whose PSCI is in EL3 firmware");

	let hvc = machine(|builder| {
		builder.begin("psci").prop_str("compatible", "arm,psci-1.0").prop_str("method", "hvc").end();
	});
	assert_eq!(at(hvc).psci_conduit(), Some(PsciConduit::Hvc));

	// NO NODE IS NOT "HVC BY DEFAULT". riscv64's virt tree has no `/psci` at all, and the answer to
	// "which instruction" for a machine that describes none is that there is no instruction.
	assert_eq!(at(RISCV64).psci_conduit(), None, "a tree with no /psci node states nothing");
	let bare = machine(|_| {});
	assert_eq!(at(bare).psci_conduit(), None);

	// A node with no `method`, and a method this system has no instruction for, are both "nothing
	// stated" rather than a guess at the more likely one.
	let no_method = machine(|builder| {
		builder.begin("psci").prop_str("compatible", "arm,psci-1.0").end();
	});
	assert_eq!(at(no_method).psci_conduit(), None);
	let strange = machine(|builder| {
		builder.begin("psci").prop_str("method", "mailbox").end();
	});
	assert_eq!(at(strange).psci_conduit(), None);
}

// ---------------------------------------------------------------------------------------------
// Malformed trees: every walk is bounded by the blocks the header declares.
// ---------------------------------------------------------------------------------------------

// The bytes of a well-formed tree, mutable, so a test can break one field of it. `Builder::finish`
// leaks its output, which is what `Fdt` needs, so this hands back a `&'static mut`.
fn broken(body: impl FnOnce(&mut Builder), damage: impl FnOnce(&mut [u8])) -> &'static [u8] {
	let good = machine(body);
	let mut bytes = good.to_vec();
	damage(&mut bytes);
	Vec::leak(bytes)
}

fn put_be32(bytes: &mut [u8], at: usize, value: u32) {
	bytes[at..at + 4].copy_from_slice(&value.to_be_bytes());
}

#[test]
fn a_header_whose_blocks_do_not_fit_is_refused() {
	// `is_valid` checked that `off_struct` and `off_strings` were each BELOW `totalsize` and stopped
	// there, so a structure block that starts inside the tree and runs past its end passed - and
	// nothing below bounded the walk either. The header carries `size_dt_struct` and
	// `size_dt_strings` for exactly this and neither was read.
	let past_end = broken(|_| {}, |bytes| put_be32(bytes, 36, 0x0010_0000));
	assert!(!at(past_end).is_valid(), "a structure block declared past the end of the tree");
	assert!(at(past_end).parse().is_none(), "and nothing walks it");

	let strings_past_end = broken(|_| {}, |bytes| put_be32(bytes, 32, 0x0010_0000));
	assert!(!at(strings_past_end).is_valid(), "a strings block declared past the end of the tree");

	// And the well-formed tree the damage was applied to is accepted, so the refusals above are
	// about the damage rather than about the fixture.
	assert!(at(machine(|_| {})).is_valid());
}

#[test]
fn a_record_that_runs_past_its_block_is_refused_rather_than_read() {
	let good = machine(|builder| {
		builder.begin("serial@9000000").prop_str("compatible", "arm,pl011").prop_reg64(0x0900_0000, 0x1000).end();
		builder.begin("chosen").prop_str("stdout-path", "/serial@9000000").end();
	});
	assert!(at(good).console().is_some(), "the fixture is a tree with a console");

	// A property whose declared length reaches beyond the structure block. The walk used to advance
	// the cursor by that length and keep reading whatever followed.
	let off_struct = u32::from_be_bytes([good[8], good[9], good[10], good[11]]) as usize;
	let huge = broken(
		|builder| {
			builder.begin("serial@9000000").prop_str("compatible", "arm,pl011").prop_reg64(0x0900_0000, 0x1000).end();
			builder.begin("chosen").prop_str("stdout-path", "/serial@9000000").end();
		},
		|bytes| {
			// The first FDT_PROP in the stream: the root's `#address-cells`. Its length word sits
			// four bytes past the token, and the root's BEGIN_NODE is one token plus an empty
			// padded name.
			let prop = off_struct + 4 + 4;
			put_be32(bytes, prop + 4, 0x0000_ffff);
		},
	);
	assert!(at(huge).parse().is_none(), "a property whose value runs past the structure block stops the walk");
	assert_eq!(at(huge).console(), None);

	// A node name with no terminator inside the block: the scan used to run until it found a zero
	// byte, anywhere in memory.
	let unterminated = broken(
		|builder| {
			builder.begin("serial@9000000").prop_str("compatible", "arm,pl011").prop_reg64(0x0900_0000, 0x1000).end();
			builder.begin("chosen").prop_str("stdout-path", "/serial@9000000").end();
		},
		|bytes| {
			let total = bytes.len();
			let strings_len = u32::from_be_bytes([bytes[32], bytes[33], bytes[34], bytes[35]]) as usize;
			// Fill the whole structure block with a BEGIN_NODE followed by non-zero bytes.
			put_be32(bytes, off_struct, 1);
			for byte in bytes.iter_mut().take(total - strings_len).skip(off_struct + 4) {
				*byte = b'x';
			}
		},
	);
	assert!(at(unterminated).parse().is_none(), "a node name with no terminator inside the block is refused");

	// A property name offset outside the strings block.
	let bad_nameoff = broken(
		|builder| {
			builder.begin("serial@9000000").prop_str("compatible", "arm,pl011").prop_reg64(0x0900_0000, 0x1000).end();
			builder.begin("chosen").prop_str("stdout-path", "/serial@9000000").end();
		},
		|bytes| {
			let prop = off_struct + 4 + 4;
			put_be32(bytes, prop + 8, 0x0000_ffff);
		},
	);
	assert!(at(bad_nameoff).parse().is_none(), "a nameoff outside the strings block is refused");
	assert_eq!(at(bad_nameoff).console(), None);
}

// The sixth audit round.

#[test]
fn a_memory_node_is_named_and_typed_and_enabled() {
	// `str_starts(name, "memory")` matched `memory-controller@`, `memory-window@` and anything else
	// beginning with those six letters - and if such a node had a `reg`, its MMIO aperture was added
	// to the RAM list and handed to the frame allocator. The specification's rule is a unit name of
	// exactly `memory` or `memory@...` AND `device_type = "memory"`, and `device_type` was not read
	// anywhere in this parser.
	//
	// Confirmed against a real tree before the rule was imposed: QEMU's `virt` writes
	// `device_type = "memory"` on its `memory@40000000`.
	let mut builder = Builder::new();
	builder.begin("");
	builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	builder.begin("memory@40000000").prop("device_type", b"memory\0").prop_reg64(0x4000_0000, 0x2000_0000).end();
	// A memory CONTROLLER's aperture, which is not memory.
	builder.begin("memory-controller@10000000").prop_reg64(0x1000_0000, 0x1000).end();
	builder.begin("cpus").prop_u32("#address-cells", 1).prop_u32("#size-cells", 0).begin("cpu@0").prop_u32("reg", 0).end().end();
	builder.end();
	let info = at(builder.finish()).parse().expect("the machine describes its memory");
	assert_eq!(info.ram_region_count, 1, "the controller's aperture is not RAM");
	assert_eq!((info.ram_base, info.ram_size), (0x4000_0000, 0x2000_0000));

	// A node with the right name and no `device_type` is not memory either.
	let mut builder = Builder::new();
	builder.begin("");
	builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	builder.begin("memory@40000000").prop_reg64(0x4000_0000, 0x2000_0000).end();
	builder.begin("cpus").prop_u32("#address-cells", 1).prop_u32("#size-cells", 0).begin("cpu@0").prop_u32("reg", 0).end().end();
	builder.end();
	assert!(at(builder.finish()).parse().is_none(), "a node without `device_type = \"memory\"` is not memory whatever it is called");

	// And one this board has disabled is memory the kernel may not use.
	let mut builder = Builder::new();
	builder.begin("");
	builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	builder.begin("memory@40000000").prop("device_type", b"memory\0").prop("status", b"disabled\0").prop_reg64(0x4000_0000, 0x2000_0000).end();
	builder.begin("cpus").prop_u32("#address-cells", 1).prop_u32("#size-cells", 0).begin("cpu@0").prop_u32("reg", 0).end().end();
	builder.end();
	assert!(at(builder.finish()).parse().is_none(), "a disabled memory node is not memory this kernel may use");
}

#[test]
fn a_zero_width_memory_tuple_is_refused_rather_than_walked_forever() {
	// `while q + 4 * (addr_cells + size_cells) <= end` never becomes false at width zero,
	// `read_cells(.., 0)` does not advance the cursor, `s == 0` takes the `continue`, and the loader
	// HANGS at boot - before there is any way to say so.
	//
	// `#size-cells = 0` is legitimate on other node types (the `cpus` node in every fixture here
	// uses it), so the rule is contextual: it is about reading a `/memory/reg`.
	let mut builder = Builder::new();
	builder.begin("");
	builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 0);
	builder.begin("memory@40000000").prop("device_type", b"memory\0").prop("reg", &0x4000_0000u64.to_be_bytes()).end();
	builder.begin("cpus").prop_u32("#address-cells", 1).prop_u32("#size-cells", 0).begin("cpu@0").prop_u32("reg", 0).end().end();
	builder.end();
	assert!(at(builder.finish()).parse().is_none(), "a zero-width tuple is refused rather than walked forever");
}

#[test]
fn the_bank_list_is_sorted_and_merged_and_bounded() {
	// The list coalesced a bank only with the one IMMEDIATELY before it, did not sort, did not merge
	// overlaps, and computed the previous bank's end unchecked. `carve_banks` then used
	// `saturating_add`, so a bank declaring `size = u64::MAX` saturated into a usable range covering
	// nearly the whole address space - and the frame allocator's overlap refusal kept the system
	// safe by discarding legitimate memory.
	let mut reg: Vec<u8> = Vec::new();
	// Out of order, with the second touching the first and the third overlapping it.
	for (base, size) in [(0x8000_0000u64, 0x1000_0000u64), (0x4000_0000, 0x1000_0000), (0x5000_0000, 0x1000_0000), (0x8800_0000, 0x1000_0000)] {
		reg.extend_from_slice(&base.to_be_bytes());
		reg.extend_from_slice(&size.to_be_bytes());
	}
	let mut builder = Builder::new();
	builder.begin("");
	builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	builder.begin("memory@40000000").prop("device_type", b"memory\0").prop("reg", &reg).end();
	builder.begin("cpus").prop_u32("#address-cells", 1).prop_u32("#size-cells", 0).begin("cpu@0").prop_u32("reg", 0).end().end();
	builder.end();
	let info = at(builder.finish()).parse().expect("the machine describes its memory");
	assert_eq!(info.ram_region_count, 2, "the touching pair merged and the overlapping pair merged");
	assert_eq!(info.ram_regions[0], (0x4000_0000, 0x2000_0000), "sorted first, and 0x4000_0000 touches 0x5000_0000");
	assert_eq!(info.ram_regions[1], (0x8000_0000, 0x1800_0000), "and the overlapping pair is their union, not the sum of their sizes");

	// A bank whose end overflows is the tree contradicting itself and is dropped, rather than
	// saturating into a range covering everything.
	let mut reg = 0x4000_0000u64.to_be_bytes().to_vec();
	reg.extend_from_slice(&u64::MAX.to_be_bytes());
	let mut builder = Builder::new();
	builder.begin("");
	builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	builder.begin("memory@40000000").prop("device_type", b"memory\0").prop("reg", &reg).end();
	builder.begin("cpus").prop_u32("#address-cells", 1).prop_u32("#size-cells", 0).begin("cpu@0").prop_u32("reg", 0).end().end();
	builder.end();
	assert!(at(builder.finish()).parse().is_none(), "a bank that does not fit the address space is not a bank");
}

#[test]
fn the_memory_reservation_block_is_read() {
	// There was no `off_mem_rsvmap` handling anywhere in this parser. The specification requires a
	// client not to use the reservation block's regions, and nothing carved them out - so pages
	// holding firmware runtime data and whatever a board reserves could be allocated and zeroed
	// while still live.
	let mut builder = Builder::new();
	builder.reserve(0x4000_0000, 0x10_0000);
	builder.reserve(0x4100_0000, 0x2_0000);
	builder.begin("");
	builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	builder.begin("memory@40000000").prop("device_type", b"memory\0").prop_reg64(0x4000_0000, 0x2000_0000).end();
	builder.begin("cpus").prop_u32("#address-cells", 1).prop_u32("#size-cells", 0).begin("cpu@0").prop_u32("reg", 0).end().end();
	builder.end();
	let blob = builder.finish();
	let fdt = at(blob);
	let mut seen: Vec<(u64, u64)> = Vec::new();
	assert!(fdt.for_each_reserved_region(|base, size| seen.push((base, size))), "the list terminates");
	assert_eq!(seen, vec![(0x4000_0000u64, 0x10_0000u64), (0x4100_0000, 0x2_0000)], "both reservations are reported");

	// AND THE BLOB'S OWN PAGES, which nothing reserved either - and this kernel keeps reading the
	// tree after the allocator is up.
	let (base, len) = fdt.extent().expect("the blob knows how big it is");
	assert_eq!(base, blob.as_ptr() as u64);
	assert_eq!(len as usize, blob.len(), "the extent is the whole blob, which is what must not be overwritten");
}

// ---------------------------------------------------------------------------------------------
// `/reserved-memory` (FDT-001), the other place a tree says "do not use this".
// ---------------------------------------------------------------------------------------------

fn reserved_memory_of(blob: &'static [u8]) -> (Vec<(u64, u64)>, bool) {
	let mut seen: Vec<(u64, u64)> = Vec::new();
	let complete = at(blob).for_each_reserved_memory_node(|base, size| seen.push((base, size)));
	(seen, complete)
}

#[test]
fn the_reserved_memory_subtree_is_read() {
	// The shape a board actually writes: a firmware carve-out with `no-map`, a DMA pool that is
	// `reusable`, and a dynamic child with a `size` and no `reg`.
	let mut builder = Builder::new();
	builder.begin("");
	builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	builder.begin("memory@40000000").prop("device_type", b"memory\0").prop_reg64(0x4000_0000, 0x2000_0000).end();
	builder.begin("reserved-memory").prop_u32("#address-cells", 2).prop_u32("#size-cells", 2).prop("ranges", b"");
	builder.begin("secure@40000000").prop_reg64(0x4000_0000, 0x10_0000).prop("no-map", b"").end();
	builder.begin("dma-pool@41000000").prop_reg64(0x4100_0000, 0x40_0000).prop("reusable", b"").end();
	builder.begin("placed-by-the-client").prop("size", &0x10_0000u64.to_be_bytes()).end();
	builder.end();
	builder.end();
	let blob = builder.finish();
	let (seen, complete) = reserved_memory_of(blob);
	assert!(complete, "the subtree is walked to the end of the tree");
	assert_eq!(seen, vec![(0x4000_0000u64, 0x10_0000u64), (0x4100_0000, 0x40_0000)], "both fixed regions, and nothing for the one with no reg");
}

// `reusable` IS RESERVED. The specification lets the OS use such a region until the owner claims it
// back, and this kernel cannot give a frame back on demand - so the flag is not a licence here, and
// this is what says so.
#[test]
fn a_reusable_region_is_reserved_like_any_other() {
	let mut builder = Builder::new();
	builder.begin("");
	builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	builder.begin("reserved-memory").prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	builder.begin("pool@50000000").prop_reg64(0x5000_0000, 0x10_0000).prop("reusable", b"").end();
	builder.end();
	builder.end();
	let (seen, complete) = reserved_memory_of(builder.finish());
	assert!(complete);
	assert_eq!(seen, vec![(0x5000_0000u64, 0x10_0000u64)]);
}

// A CHILD MAY DECLARE SEVERAL RANGES, and a trailing partial pair is not one of them.
#[test]
fn one_child_may_carry_several_ranges_and_a_partial_pair_is_not_a_range() {
	let mut builder = Builder::new();
	builder.begin("");
	builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	builder.begin("reserved-memory").prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	let mut reg = Vec::new();
	for (base, size) in [(0x6000_0000u64, 0x1000u64), (0x6100_0000, 0x2000)] {
		reg.extend_from_slice(&base.to_be_bytes());
		reg.extend_from_slice(&size.to_be_bytes());
	}
	// Half of a third pair: an address with no size.
	reg.extend_from_slice(&0x6200_0000u64.to_be_bytes());
	builder.begin("two-and-a-half@60000000").prop("reg", &reg).end();
	builder.end();
	builder.end();
	let (seen, complete) = reserved_memory_of(builder.finish());
	assert!(complete);
	assert_eq!(seen, vec![(0x6000_0000u64, 0x1000u64), (0x6100_0000, 0x2000)], "the half pair carves nothing");
}

// The node's OWN cell counts, when it declares them, and the root's when it does not.
#[test]
fn the_subtrees_own_cell_counts_are_used() {
	let mut builder = Builder::new();
	builder.begin("");
	builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	// One cell each, which is a 32-bit machine's shape and not the root's.
	builder.begin("reserved-memory").prop_u32("#address-cells", 1).prop_u32("#size-cells", 1);
	let mut reg = Vec::new();
	reg.extend_from_slice(&0x8000_0000u32.to_be_bytes());
	reg.extend_from_slice(&0x10_0000u32.to_be_bytes());
	builder.begin("fb@80000000").prop("reg", &reg).end();
	builder.end();
	builder.end();
	let (seen, complete) = reserved_memory_of(builder.finish());
	assert!(complete);
	assert_eq!(seen, vec![(0x8000_0000u64, 0x10_0000u64)], "read with one cell each, not the root's two");
}

// A NAME THAT MERELY STARTS WITH IT IS NOT IT.
#[test]
fn a_node_named_like_reserved_memory_is_not_reserved_memory() {
	let mut builder = Builder::new();
	builder.begin("");
	builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	builder.begin("reserved-memory-pool").prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	builder.begin("thing@90000000").prop_reg64(0x9000_0000, 0x1000).end();
	builder.end();
	builder.end();
	let (seen, complete) = reserved_memory_of(builder.finish());
	assert!(complete);
	assert!(seen.is_empty(), "nothing under a node that is not /reserved-memory");
}

// A tree with no such subtree answers cleanly rather than refusing.
#[test]
fn a_tree_without_the_subtree_reports_nothing_and_completes() {
	let (seen, complete) = reserved_memory_of(AARCH64);
	assert!(complete, "the real QEMU tree walks to its end");
	assert!(seen.is_empty(), "QEMU virt declares no /reserved-memory");
	let (seen, complete) = reserved_memory_of(RISCV64);
	assert!(complete);
	assert!(seen.is_empty());
}

#[test]
fn the_cpu_nodes_yield_hardware_ids_rather_than_a_count_to_iterate() {
	// KERN-ARCH-008. The walk counted `cpu@` nodes and threw away the `reg` that says WHICH core
	// each one is, so both non-x86 backends started ids `0..cpu_count`: dense, zero-based and in
	// tree order, none of which a device tree promises. A board whose harts are 0, 1, 4, 5 had two
	// targets that are not cores, and two cores that never started.
	let mut builder = Builder::new();
	builder.begin("");
	builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	builder.begin("memory@40000000").prop("device_type", b"memory\0").prop_reg64(0x4000_0000, 0x2000_0000).end();
	builder.begin("cpus").prop_u32("#address-cells", 1).prop_u32("#size-cells", 0);
	for id in [0u32, 1, 4, 5] {
		builder.begin("cpu@x").prop("device_type", b"cpu\0").prop_u32("reg", id).end();
	}
	builder.end().end();
	let info = at(builder.finish()).parse().expect("the machine parses");
	assert_eq!(info.cpu_count, 4);
	assert_eq!(&info.cpu_ids[..4], &[0, 1, 4, 5], "the ids are the tree's, not the enumeration order");
	assert_eq!(info.cpu_nodes, 4);
}

#[test]
fn a_disabled_cpu_is_not_a_bring_up_target() {
	// The `/memory` reader has honoured `status` since FDT-004; the cpu reader did not read it at
	// all, so a core the firmware still owns - or one that does not physically exist on this SKU -
	// was sent a `CPU_ON`/`hart_start` like any other. `cpu_nodes` still counts it, so the
	// difference between what the tree declares and what the kernel may use stays visible.
	let mut builder = Builder::new();
	builder.begin("");
	builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	builder.begin("memory@40000000").prop("device_type", b"memory\0").prop_reg64(0x4000_0000, 0x2000_0000).end();
	builder.begin("cpus").prop_u32("#address-cells", 1).prop_u32("#size-cells", 0);
	builder.begin("cpu@0").prop_u32("reg", 0).prop_str("status", "okay").end();
	builder.begin("cpu@1").prop_u32("reg", 1).prop_str("status", "disabled").end();
	// STATUS AFTER REG, which is the order that catches a reader deciding too early.
	builder.begin("cpu@2").prop_u32("reg", 2).prop_str("status", "fail").end();
	builder.begin("cpu@3").prop_u32("reg", 3).end();
	builder.end().end();
	let info = at(builder.finish()).parse().expect("the machine parses");
	assert_eq!(info.cpu_count, 2, "two usable cores");
	assert_eq!(&info.cpu_ids[..2], &[0, 3], "and they are the ones the tree left enabled");
	assert_eq!(info.cpu_nodes, 4, "while the tree's own declaration is still reported");
}

#[test]
fn a_cpu_id_is_read_in_the_cells_the_cpus_node_declares() {
	// `/cpus` carries its own `#address-cells`; the root's describe the root's children. An MPIDR
	// wide enough to need two cells - a clustered aarch64 machine - is the case where reading the
	// root's count, or assuming one, produces the wrong core.
	let mut builder = Builder::new();
	builder.begin("");
	builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	builder.begin("memory@40000000").prop("device_type", b"memory\0").prop_reg64(0x4000_0000, 0x2000_0000).end();
	builder.begin("cpus").prop_u32("#address-cells", 2).prop_u32("#size-cells", 0);
	let mut reg = 0u32.to_be_bytes().to_vec();
	reg.extend_from_slice(&0x0000_0100u32.to_be_bytes());
	builder.begin("cpu@100").prop("reg", &reg).end();
	builder.end().end();
	let info = at(builder.finish()).parse().expect("the machine parses");
	assert_eq!(info.cpu_count, 1);
	assert_eq!(info.cpu_ids[0], 0x100, "both cells, folded in tree order");
}

#[test]
fn more_cores_than_the_table_holds_are_capped_and_counted() {
	// Capped rather than refused: the backends have a fixed per-CPU pool, and a machine with more
	// cores than it holds is still a machine. `cpu_nodes` is what makes the cap sayable - the
	// caller can report "N of M" instead of silently describing a sixteen-core board as an
	// eight-core one.
	let mut builder = Builder::new();
	builder.begin("");
	builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	builder.begin("memory@40000000").prop("device_type", b"memory\0").prop_reg64(0x4000_0000, 0x2000_0000).end();
	builder.begin("cpus").prop_u32("#address-cells", 1).prop_u32("#size-cells", 0);
	for id in 0..16u32 {
		builder.begin("cpu@x").prop_u32("reg", id).end();
	}
	builder.end().end();
	let info = at(builder.finish()).parse().expect("the machine parses");
	assert_eq!(info.cpu_count as usize, super::MAX_CPUS, "the table is full and not overrun");
	assert_eq!(info.cpu_nodes, 16, "and the machine's real width is still reported");
	assert_eq!(info.cpu_ids[super::MAX_CPUS - 1], (super::MAX_CPUS - 1) as u64);
}

#[test]
fn a_cpu_node_without_a_reg_names_no_core() {
	// The specification requires `reg` on a cpu node. Without one there is nothing to send a
	// `CPU_ON` to, and inventing an id from the enumeration order is the dense assumption this
	// whole finding is about - so the node is counted and not recorded.
	let mut builder = Builder::new();
	builder.begin("");
	builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	builder.begin("memory@40000000").prop("device_type", b"memory\0").prop_reg64(0x4000_0000, 0x2000_0000).end();
	builder.begin("cpus").prop_u32("#address-cells", 1).prop_u32("#size-cells", 0);
	builder.begin("cpu@0").prop_u32("reg", 0).end();
	builder.begin("cpu@1").prop("device_type", b"cpu\0").end();
	builder.end().end();
	let info = at(builder.finish()).parse().expect("the machine parses");
	assert_eq!(info.cpu_count, 1);
	assert_eq!(info.cpu_ids[0], 0);
	assert_eq!(info.cpu_nodes, 2);
}

#[test]
fn a_console_the_reader_cannot_represent_is_not_a_console() {
	// FDT-006. The console path accepted one through FOUR address cells while every reader in this
	// crate folds cells into a `u64`, so a bus declaring three produced the low 64 bits of a 96-bit
	// address - a number that looks like a valid physical address and names nothing. The root
	// parser had applied `MAX_CELLS` since it was written; this path had its own rule.
	for cells in [0u32, 1, 2, 3, 4] {
		let mut builder = Builder::new();
		builder.begin("");
		builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
		builder.begin("memory@40000000").prop("device_type", b"memory\0").prop_reg64(0x4000_0000, 0x2000_0000).end();
		builder.begin("chosen").prop_str("stdout-path", "/soc/serial@10000000").end();
		builder.begin("soc").prop_u32("#address-cells", cells).prop_u32("#size-cells", 1).prop("ranges", &[]);
		// The address written in `cells` cells, followed by a one-cell size.
		let mut reg: Vec<u8> = Vec::new();
		for index in (0..cells).rev() {
			// The address, big-endian, in as many cells as this bus declares; cells above the
			// value's own width are zero.
			let cell = if index == 0 { 0x1000_0000u32 } else { 0 };
			reg.extend_from_slice(&cell.to_be_bytes());
		}
		reg.extend_from_slice(&0x1000u32.to_be_bytes());
		builder.begin("serial@10000000").prop_str("compatible", "ns16550a").prop("reg", &reg).end();
		builder.end().end();
		let base = at(builder.finish()).console().map(|console| console.base);
		match cells {
			1 | 2 => assert_eq!(base, Some(0x1000_0000), "{cells} cells is a width this reader represents"),
			_ => assert_eq!(base, None, "{cells} cells is not, so there is no console rather than a truncated address"),
		}
	}
}

#[test]
fn the_register_spacing_and_access_width_are_bounded_by_what_the_loader_can_issue() {
	// FDT-006. `reg-shift` was taken as any `u32` and handed to the loader, which evaluates
	// `5 << reg_shift` for the line-status register: a panic under checked arithmetic, or a masked
	// offset into an unrelated register in release. And `reg-io-width` was not read at all, so a
	// part wired for 32-bit access was byte-poked - after `ExitBootServices`, where a lost console
	// is also a lost diagnosis.
	let with = |name: &'static str, value: u32| {
		machine(|builder| {
			builder.begin("chosen").prop_str("stdout-path", "/serial@10000000").end();
			builder.begin("serial@10000000").prop_str("compatible", "ns16550a").prop_reg64(0x1000_0000, 0x1000).prop_u32(name, value).end();
		})
	};
	for shift in [0u32, 1, 2, 3] {
		assert_eq!(at(with("reg-shift", shift)).console().map(|console| console.reg_shift), Some(shift), "a spacing of {shift} is one a 16550 has");
	}
	for shift in [4u32, 63, 64, u32::MAX] {
		assert_eq!(at(with("reg-shift", shift)).console(), None, "a spacing of {shift} is not a register file, so there is no console");
	}
	for width in [1u32, 2, 4] {
		assert_eq!(at(with("reg-io-width", width)).console().map(|console| console.reg_io_width), Some(width), "a width the access layer implements is carried");
	}
	for width in [0u32, 3, 8, u32::MAX] {
		assert_eq!(at(with("reg-io-width", width)).console(), None, "a width it cannot issue is a console it cannot drive");
	}
	// And the default, which is what every tree in this repository actually says.
	let plain = machine(|builder| {
		builder.begin("chosen").prop_str("stdout-path", "/serial@10000000").end();
		builder.begin("serial@10000000").prop_str("compatible", "ns16550a").prop_reg64(0x1000_0000, 0x1000).end();
	});
	assert_eq!(at(plain).console().map(|console| (console.reg_shift, console.reg_io_width)), Some((0, 1)));
}

#[test]
fn a_disabled_console_is_not_the_console() {
	// The node `/chosen` points at can be marked unusable like any other, and the reader did not
	// look. Driving a UART the firmware has disabled is writing to a device that may be powered
	// down or handed to something else.
	let disabled = machine(|builder| {
		builder.begin("chosen").prop_str("stdout-path", "/serial@10000000").end();
		builder.begin("serial@10000000").prop_str("compatible", "ns16550a").prop_reg64(0x1000_0000, 0x1000).prop_str("status", "disabled").end();
	});
	assert_eq!(at(disabled).console(), None, "a disabled node is not a console");
	let okay = machine(|builder| {
		builder.begin("chosen").prop_str("stdout-path", "/serial@10000000").end();
		builder.begin("serial@10000000").prop_str("compatible", "ns16550a").prop_reg64(0x1000_0000, 0x1000).prop_str("status", "okay").end();
	});
	assert_eq!(at(okay).console().map(|console| console.base), Some(0x1000_0000), "and an enabled one is");
}

// A machine with a `/soc` bus carrying whatever `body` adds, so the platform-device cases below
// differ by one node each.
fn machine_with_soc(bus_props: impl FnOnce(&mut Builder), body: impl FnOnce(&mut Builder)) -> &'static [u8] {
	let mut builder = Builder::new();
	builder.begin("");
	builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	builder.begin("memory@40000000").prop("device_type", b"memory\0").prop_reg64(0x4000_0000, 0x2000_0000).end();
	builder.begin("cpus").prop_u32("#address-cells", 1).prop_u32("#size-cells", 0).begin("cpu@0").prop_u32("reg", 0).end().end();
	builder.begin("soc");
	bus_props(&mut builder);
	body(&mut builder);
	builder.end().end();
	builder.finish()
}

#[test]
fn a_platform_device_is_the_one_its_compatible_names_and_not_the_one_its_name_starts_like() {
	// FDT-003. The walk entered these nodes on a unit-name PREFIX at any depth, read the `reg`
	// there and then, and let the last one win. So `plic-sw` - a real node on several boards -
	// overwrote the interrupt controller's base with its own, and the kernel then programmed a
	// different device. `compatible` is what a binding is; the name only says where to look.
	let blob = machine_with_soc(
		|builder| {
			builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2).prop("ranges", &[]);
		},
		|builder| {
			// The decoy FIRST, which is the ordering that catches a reader taking the name for the
			// binding: last-one-wins and first-one-wins both give the right answer otherwise.
			builder.begin("plic-sw@d000000").prop_str("compatible", "sifive,plic-sw").prop_reg64(0x0d00_0000, 0x1000).end();
			builder.begin("plic@c000000").prop_str("compatible", "sifive,plic-1.0.0").prop_reg64(0x0c00_0000, 0x60_0000).end();
		},
	);
	let info = at(blob).parse().expect("the machine parses");
	assert_eq!(info.plic_base, 0x0c00_0000, "the controller, not the node whose name starts the same way");

	// And a node the firmware disabled is not the controller either.
	let disabled = machine_with_soc(
		|builder| {
			builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2).prop("ranges", &[]);
		},
		|builder| {
			builder.begin("plic@c000000").prop_str("compatible", "sifive,plic-1.0.0").prop_reg64(0x0c00_0000, 0x60_0000).prop_str("status", "disabled").end();
		},
	);
	assert_eq!(at(disabled).parse().expect("the machine parses").plic_base, 0, "a disabled controller is not a controller");
}

#[test]
fn a_platform_device_behind_a_bus_is_read_in_that_buss_cells_and_translated_through_it() {
	// FDT-003, the other half: these `reg`s were decoded with the ROOT's cell counts even under a
	// bus that declares its own, and the address was never translated through the bus's `ranges`.
	// One reads the wrong number of cells; the other returns an address on the wrong bus. Both
	// point PCI enumeration or an interrupt controller at whatever happens to live there.
	let blob = machine_with_soc(
		|builder| {
			// A one-cell bus whose window is based a long way up.
			builder.prop_u32("#address-cells", 1).prop_u32("#size-cells", 1);
			let mut ranges = 0x1000u32.to_be_bytes().to_vec(); // child 0x1000
			ranges.extend_from_slice(&0x3000_0000u64.to_be_bytes()); // -> parent 0x3000_0000
			ranges.extend_from_slice(&0x1_0000u32.to_be_bytes()); // 64 kB long
			builder.prop("ranges", &ranges);
		},
		|builder| {
			let mut reg = 0x2000u32.to_be_bytes().to_vec();
			reg.extend_from_slice(&0x1000u32.to_be_bytes());
			builder.begin("pcie@2000").prop_str("compatible", "pci-host-ecam-generic").prop("reg", &reg).end();
		},
	);
	let info = at(blob).parse().expect("the machine parses");
	assert_eq!(info.pcie_ecam, 0x3000_1000, "one cell read as one cell, then translated through the bus window");
}

// The reservation block of `blob`, and whether the reader accepted it whole.
fn reservations_of(blob: &'static [u8]) -> (Vec<(u64, u64)>, bool) {
	let mut seen: Vec<(u64, u64)> = Vec::new();
	let complete = at(blob).for_each_reserved_region(|base, size| seen.push((base, size)));
	(seen, complete)
}

// A machine whose reservation block holds exactly `entries`.
fn machine_reserving(entries: &[(u64, u64)]) -> &'static [u8] {
	let mut builder = Builder::new();
	for &(base, size) in entries {
		builder.reserve(base, size);
	}
	builder.begin("");
	builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	builder.begin("memory@40000000").prop("device_type", b"memory\0").prop_reg64(0x4000_0000, 0x2000_0000).end();
	builder.end();
	builder.finish()
}

#[test]
fn a_reservation_list_is_read_whole_or_not_at_all() {
	// FDT-005. Entries were handed to the caller as they were read, and the "could this list be
	// read?" answer came afterwards - so a caller that treated it as a diagnostic, which the one in
	// this tree did, had already carved a PREFIX and then used the rest of RAM as though the list
	// had been empty. Firmware memory, in the allocator, on a machine whose tree is malformed.
	//
	// A good list is still read.
	let (seen, complete) = reservations_of(machine_reserving(&[(0x4000_0000, 0x1000), (0x8000_0000, 0x2000)]));
	assert!(complete);
	assert_eq!(seen, vec![(0x4000_0000, 0x1000), (0x8000_0000, 0x2000)]);

	// A zero-size entry is the tree contradicting itself. It used to be skipped without even
	// marking the list incomplete, so the entries AFTER it were carved and the list read as good.
	let (seen, complete) = reservations_of(machine_reserving(&[(0x4000_0000, 0x1000), (0x5000_0000, 0), (0x8000_0000, 0x2000)]));
	assert!(!complete, "a malformed entry is not a list that was read");
	assert!(seen.is_empty(), "and nothing before it was committed");

	// An entry whose end overflows: same treatment, for the same reason.
	let (seen, complete) = reservations_of(machine_reserving(&[(0x4000_0000, 0x1000), (u64::MAX - 4, 64)]));
	assert!(!complete);
	assert!(seen.is_empty(), "nothing is committed from a list that does not validate");
}

#[test]
fn a_reservation_list_that_does_not_terminate_reserves_nothing() {
	// The terminator comes off the same untrusted bytes as everything else. A list that runs to the
	// cap without one, or past it, is a list this reader cannot say it has read - and the entries it
	// did read are not a safe subset, because the ones it did not are exactly the unknown.
	let full: Vec<(u64, u64)> = (0..super::MAX_RESERVED_REGIONS as u64).map(|i| (0x4000_0000 + i * 0x2000, 0x1000)).collect();
	let (seen, complete) = reservations_of(machine_reserving(&full));
	assert!(complete, "a full list followed by its terminator is a list that terminated");
	assert_eq!(seen.len(), super::MAX_RESERVED_REGIONS);

	let over: Vec<(u64, u64)> = (0..super::MAX_RESERVED_REGIONS as u64 + 1).map(|i| (0x4000_0000 + i * 0x2000, 0x1000)).collect();
	let (seen, complete) = reservations_of(machine_reserving(&over));
	assert!(!complete, "one more than this reader carries is a list it cannot carry");
	assert!(seen.is_empty(), "and it commits none of it");
}

// ---------------------------------------------------------------------------------------------
// The ARM interrupt controller, read from the machine rather than from a constant.
// ---------------------------------------------------------------------------------------------

// A GICv2 node the way QEMU's virt machine writes it: two ranges in one `reg`, distributor first.
fn gic_reg(dist: u64, dist_size: u64, cpu: u64, cpu_size: u64) -> Vec<u8> {
	let mut reg = dist.to_be_bytes().to_vec();
	reg.extend_from_slice(&dist_size.to_be_bytes());
	reg.extend_from_slice(&cpu.to_be_bytes());
	reg.extend_from_slice(&cpu_size.to_be_bytes());
	reg
}

fn with_gic(reg: &[u8], compatible: &str) -> &'static [u8] {
	let reg = reg.to_vec();
	let compatible = compatible.to_string();
	machine(move |builder| {
		builder.begin("intc@8000000").prop_str("compatible", &compatible).prop("reg", &reg).end();
	})
}

#[test]
fn the_controller_addresses_come_from_the_tree() {
	// THE POINT OF THE ITEM. Both addresses used to be constants naming one QEMU machine, so passing
	// on that machine was not evidence that anything had been discovered. A tree that says something
	// else must move them.
	let blob = with_gic(&gic_reg(0x0800_0000, 0x1_0000, 0x0801_0000, 0x1_0000), "arm,cortex-a15-gic");
	let info = at(blob).parse().expect("the tree parses");
	assert_eq!(info.gic_dist, 0x0800_0000, "the distributor is the first range");
	assert_eq!(info.gic_cpu, 0x0801_0000, "the CPU interface is the second");
	assert_eq!(info.gic_version, 2, "`arm,cortex-a15-gic` is a GICv2");

	let moved = with_gic(&gic_reg(0x1_0000_0000, 0x1_0000, 0x1_0001_0000, 0x2_0000), "arm,gic-400");
	let info = at(moved).parse().expect("the tree parses");
	assert_eq!(info.gic_dist, 0x1_0000_0000, "a machine that maps it elsewhere is followed");
	assert_eq!(info.gic_cpu, 0x1_0001_0000);
	assert_eq!(info.gic_cpu_size, 0x2_0000, "the size is the tree's, not a fixed window");
}

#[test]
fn a_gicv3_says_it_is_one() {
	// The two versions describe two ranges each and are driven differently, so the version cannot be
	// inferred from the shape of `reg` - it comes from `compatible`.
	let blob = with_gic(&gic_reg(0x0800_0000, 0x1_0000, 0x080a_0000, 0xf6_0000), "arm,gic-v3");
	let info = at(blob).parse().expect("the tree parses");
	assert_eq!(info.gic_version, 3);
	assert_eq!(info.gic_cpu, 0x080a_0000, "a GICv3's second range is the redistributor region");
}

#[test]
fn a_controller_this_reader_does_not_know_is_not_used() {
	// `compatible` is the whole of the decision. A node named `intc` that says it is something else
	// leaves the addresses at zero, which the caller reads as "no controller" and refuses on.
	let blob = with_gic(&gic_reg(0x0800_0000, 0x1_0000, 0x0801_0000, 0x1_0000), "acme,not-a-gic");
	let info = at(blob).parse().expect("the tree parses");
	assert_eq!(info.gic_dist, 0, "an unknown controller is not an address to write to");
	assert_eq!(info.gic_version, 0);
}

#[test]
fn a_disabled_controller_is_not_used() {
	let blob = machine(|builder| {
		builder.begin("intc@8000000").prop_str("compatible", "arm,cortex-a15-gic").prop("status", b"disabled\0").prop("reg", &gic_reg(0x0800_0000, 0x1_0000, 0x0801_0000, 0x1_0000)).end();
	});
	assert_eq!(at(blob).parse().expect("parses").gic_dist, 0);
}

#[test]
fn a_reg_too_short_for_two_ranges_is_refused() {
	// A CONTROLLER IS BOTH REGIONS OR IT IS NOTHING. Taking the first range alone would leave the
	// CPU interface at whatever the previous constant said, which is the failure this item exists to
	// remove - and reading past the property takes the padding after it as an address.
	let mut short = 0x0800_0000u64.to_be_bytes().to_vec();
	short.extend_from_slice(&0x1_0000u64.to_be_bytes());
	let blob = with_gic(&short, "arm,cortex-a15-gic");
	let info = at(blob).parse().expect("the tree parses");
	assert_eq!(info.gic_dist, 0, "one range is not a controller");
	assert_eq!(info.gic_cpu, 0);
}

#[test]
fn a_zero_sized_or_overflowing_range_is_refused() {
	let zero = with_gic(&gic_reg(0x0800_0000, 0, 0x0801_0000, 0x1_0000), "arm,cortex-a15-gic");
	assert_eq!(at(zero).parse().expect("parses").gic_dist, 0, "a region of no bytes cannot be mapped");

	let overflow = with_gic(&gic_reg(0x0800_0000, 0x1_0000, u64::MAX - 8, 0x1_0000), "arm,cortex-a15-gic");
	assert_eq!(at(overflow).parse().expect("parses").gic_dist, 0, "an end past the address space is refused, not wrapped");
}

#[test]
fn two_ranges_that_share_a_byte_are_refused() {
	// Writing the distributor would write the CPU interface. The check is here, where both are
	// known, rather than at the first MMIO access - which is after `ExitBootServices`.
	let blob = with_gic(&gic_reg(0x0800_0000, 0x2_0000, 0x0801_0000, 0x1_0000), "arm,cortex-a15-gic");
	assert_eq!(at(blob).parse().expect("parses").gic_dist, 0);
}

#[test]
fn the_first_usable_controller_wins() {
	// The same rule the other platform devices follow: a later node cannot overwrite an address
	// already taken from a usable one.
	let blob = machine(|builder| {
		builder.begin("intc@8000000").prop_str("compatible", "arm,cortex-a15-gic").prop("reg", &gic_reg(0x0800_0000, 0x1_0000, 0x0801_0000, 0x1_0000)).end();
		builder.begin("intc@9000000").prop_str("compatible", "arm,cortex-a15-gic").prop("reg", &gic_reg(0x0900_0000, 0x1_0000, 0x0901_0000, 0x1_0000)).end();
	});
	assert_eq!(at(blob).parse().expect("parses").gic_dist, 0x0800_0000);
}

#[test]
fn the_msi_frame_is_read_from_the_controller_s_child() {
	// The GICv2m frame is a child of the controller node, and a machine without one is a machine
	// without message-signalled interrupts rather than a broken tree.
	let mut with_frame = Builder::new();
	with_frame.begin("");
	with_frame.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	with_frame.begin("memory@40000000").prop("device_type", b"memory\0").prop_reg64(0x4000_0000, 0x2000_0000).end();
	with_frame.begin("cpus").prop_u32("#address-cells", 1).prop_u32("#size-cells", 0).begin("cpu@0").prop_u32("reg", 0).end().end();
	// The cell widths for the frame BELOW it, which is what a controller with a child declares -
	// QEMU's virt machine writes exactly this. A child's `reg` is read with its parent's widths, so
	// a node that has children and declares none describes nothing about them.
	with_frame
		.begin("intc@8000000")
		.prop_str("compatible", "arm,cortex-a15-gic")
		.prop_u32("#address-cells", 2)
		.prop_u32("#size-cells", 2)
		// AND AN EMPTY `ranges`, which is what QEMU writes and what the specification requires of a
		// node whose children have addresses: no entries means the identity mapping. Without it the
		// frame's address is not translatable to the root bus at all, and the reader refuses it -
		// which is the right answer for a tree that declares a child address under a bus that maps
		// nothing.
		.prop("ranges", b"")
		.prop("reg", &gic_reg(0x0800_0000, 0x1_0000, 0x0801_0000, 0x1_0000));
	with_frame.begin("v2m@8020000").prop_str("compatible", "arm,gic-v2m-frame").prop_reg64(0x0802_0000, 0x1000).end();
	with_frame.end().end();
	let blob = with_frame.finish();
	let info = at(blob).parse().expect("the tree parses");
	assert_eq!(info.gic_msi, 0x0802_0000);
	assert_eq!(info.gic_msi_size, 0x1000);

	let without = with_gic(&gic_reg(0x0800_0000, 0x1_0000, 0x0801_0000, 0x1_0000), "arm,cortex-a15-gic");
	assert_eq!(at(without).parse().expect("parses").gic_msi, 0, "no frame is no MSI, not a refusal");
}

#[test]
fn the_real_qemu_tree_describes_its_own_controller() {
	// The fixture is the machine this port actually boots on, so the addresses the constants used to
	// carry must be the ones the tree names.
	let info = at(AARCH64).parse().expect("the QEMU tree parses");
	assert_eq!(info.gic_dist, 0x0800_0000, "QEMU virt maps the distributor here");
	assert_eq!(info.gic_cpu, 0x0801_0000, "and the CPU interface here");
	assert_eq!(info.gic_version, 2, "the harness runs `virt,gic-version=2`");
	assert_eq!(info.gic_msi, 0x0802_0000, "and the v2m frame just above them");
}

#[test]
fn a_frame_under_a_controller_that_maps_nothing_is_refused() {
	// A child address is only meaningful through its parent's `ranges`. A controller that declares
	// none maps nothing, so the frame address below it names nothing at the root - and taking it
	// anyway would write MSI configuration at an address the machine never described.
	let mut builder = Builder::new();
	builder.begin("");
	builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	builder.begin("memory@40000000").prop("device_type", b"memory\0").prop_reg64(0x4000_0000, 0x2000_0000).end();
	builder.begin("cpus").prop_u32("#address-cells", 1).prop_u32("#size-cells", 0).begin("cpu@0").prop_u32("reg", 0).end().end();
	builder.begin("intc@8000000").prop_str("compatible", "arm,cortex-a15-gic").prop_u32("#address-cells", 2).prop_u32("#size-cells", 2).prop("reg", &gic_reg(0x0800_0000, 0x1_0000, 0x0801_0000, 0x1_0000));
	builder.begin("v2m@8020000").prop_str("compatible", "arm,gic-v2m-frame").prop_reg64(0x0802_0000, 0x1000).end();
	builder.end().end();
	let info = at(builder.finish()).parse().expect("the tree parses");
	assert_eq!(info.gic_msi, 0, "an untranslatable frame address is not an address");
	assert_eq!(info.gic_dist, 0x0800_0000, "the controller itself is at the root and still readable");
}

#[test]
fn the_supervisor_imsic_is_the_one_taken() {
	// TWO CONTROLLERS, AND THE FIRST IS THE FIRMWARE'S. QEMU lists the M-mode IMSIC at 0x2400_0000
	// before the S-mode one at 0x2800_0000, so a reader that took the first node would point every
	// device MSI at the interrupt files OpenSBI owns - which this kernel cannot read and would never
	// see fire. `interrupts-extended` names IRQ 9 for the supervisor file and 11 for the machine one.
	let info = at(RISCV64_AIA).parse().expect("the AIA tree parses");
	assert_eq!(info.imsic_base, 0x2800_0000, "the S-mode interrupt files");
	assert_ne!(info.imsic_size, 0, "and the extent the tree gives them");
}

#[test]
fn a_machine_without_imsics_reports_none() {
	// The plain `virt` machine routes through a PLIC and has no IMSIC at all. Zero is the answer,
	// and the port reads it as "this machine does not deliver MSI that way" rather than as a
	// controller at address zero.
	let info = at(RISCV64).parse().expect("the PLIC tree parses");
	assert_eq!(info.imsic_base, 0);
	assert_ne!(info.plic_base, 0, "that machine's controller is still found");
}

#[test]
fn an_imsic_wired_to_the_machine_interrupt_is_not_taken() {
	// The rule, written as its own case: the same node, the same `compatible`, one cell different.
	let mut m_mode = 0u32.to_be_bytes().to_vec();
	m_mode.extend_from_slice(&11u32.to_be_bytes());
	let blob = machine(move |builder| {
		builder.begin("interrupt-controller@28000000").prop_str("compatible", "riscv,imsics").prop("interrupts-extended", &m_mode).prop_reg64(0x2800_0000, 0x4000).end();
	});
	assert_eq!(at(blob).parse().expect("parses").imsic_base, 0, "the machine-mode file belongs to the firmware");
}

// A RISC-V AIA machine, built so the interrupt files' association with harts can be varied.
// `harts` gives each cpu node's `reg` and the phandle of its interrupt-controller child; `files`
// gives the phandles the S-mode IMSIC names, IN FILE ORDER.
fn aia(addr_cells: u32, harts: &[(u64, u32)], files: &[u32], imsic_base: u64, props: impl FnOnce(&mut Builder)) -> &'static [u8] {
	let mut builder = Builder::new();
	builder.begin("");
	builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	builder.begin("memory@40000000").prop("device_type", b"memory\0").prop_reg64(0x4000_0000, 0x2000_0000).end();
	builder.begin("cpus").prop_u32("#address-cells", addr_cells).prop_u32("#size-cells", 0);
	for &(reg, phandle) in harts {
		builder.begin("cpu@x").prop("device_type", b"cpu\0");
		if addr_cells == 2 {
			let mut cells = ((reg >> 32) as u32).to_be_bytes().to_vec();
			cells.extend_from_slice(&(reg as u32).to_be_bytes());
			builder.prop("reg", &cells);
		} else {
			builder.prop_u32("reg", reg as u32);
		}
		builder.begin("interrupt-controller").prop_str("compatible", "riscv,cpu-intc").prop_u32("phandle", phandle).end();
		builder.end();
	}
	builder.end();
	let mut ext = Vec::new();
	for &phandle in files {
		ext.extend_from_slice(&phandle.to_be_bytes());
		ext.extend_from_slice(&9u32.to_be_bytes()); // the supervisor external interrupt
	}
	builder.begin("imsics@x").prop_str("compatible", "riscv,imsics").prop("interrupts-extended", &ext).prop_reg64(imsic_base, 0x1000 * files.len() as u64);
	props(&mut builder);
	builder.end();
	builder.end();
	builder.finish()
}

#[test]
fn the_real_aia_machine_ties_every_file_to_its_hart() {
	// The identity the kernel's `base + hart * stride` addressing assumes, read rather than assumed:
	// on this machine file N does belong to hart N, and now that is a fact the tree stated.
	let info = at(RISCV64_AIA).parse().expect("the AIA machine parses");
	assert_eq!(info.imsic_hart_count, 8, "one interrupt file per hart");
	for (index, &hart) in info.imsic_harts[..8].iter().enumerate() {
		assert_eq!(hart, index as u64, "file {index} belongs to hart {index}");
	}
	assert_eq!(info.imsic_guest_index_bits, 0, "no guest indexing");
	assert_eq!(info.imsic_group_index_bits, 0, "no group indexing");
	assert!(info.imsic_num_ids >= 63, "the controller carries at least the identities this kernel uses");
}

#[test]
fn a_file_is_matched_to_its_hart_by_phandle_rather_than_by_position() {
	// THE ORDER OF THE CPU NODES IS NOT THE ORDER OF THE FILES, and nothing says it must be. Here
	// the tree lists its harts backwards while the IMSIC lists its files forwards; a reader that
	// paired them off by position would have every file pointing at the wrong hart.
	let harts = [(3u64, 0x30u32), (2, 0x20), (1, 0x10), (0, 0x100)];
	let info = at(aia(1, &harts, &[0x100, 0x10, 0x20, 0x30], 0x2800_0000, |_| {})).parse().expect("parses");
	assert_eq!(&info.cpu_ids[..4], &[3, 2, 1, 0], "the cpu ids are in tree order");
	assert_eq!(&info.imsic_harts[..4], &[0, 1, 2, 3], "the files are in their own order, resolved by phandle");
}

#[test]
fn a_file_whose_entry_names_no_cpu_belongs_to_no_hart() {
	// A dangling phandle is not a hart to address. It is reported as unknown rather than silently
	// becoming file index 0's hart.
	let info = at(aia(1, &[(0, 0x10)], &[0x10, 0xdead], 0x2800_0000, |_| {})).parse().expect("parses");
	assert_eq!(info.imsic_hart_count, 2, "the tree declares two files");
	assert_eq!(info.imsic_harts[0], 0);
	assert_eq!(info.imsic_harts[1], u64::MAX, "the second names nothing this tree describes");
}

#[test]
fn a_hart_id_wider_than_thirty_two_bits_is_read_whole() {
	// KERN-ARCH-008 again, at the other end: `#address-cells = 2` under `/cpus` is a 64-bit hart id,
	// which the SBI ABI carries as an `unsigned long`. Truncating it to 32 bits gives two harts one
	// id - and the id is what `hart_start` and every IPI target.
	let harts = [(0x1_0000_0000u64, 0x10u32), (0x1_0000_0001, 0x20)];
	let info = at(aia(2, &harts, &[0x10, 0x20], 0x2800_0000, |_| {})).parse().expect("parses");
	assert_eq!(&info.cpu_ids[..2], &[0x1_0000_0000, 0x1_0000_0001], "both halves of the id survive");
	assert_eq!(&info.imsic_harts[..2], &[0x1_0000_0000, 0x1_0000_0001], "and the files name them whole");
}

#[test]
fn the_interrupt_files_are_wherever_the_tree_puts_them() {
	// The address is read, not defaulted: a machine that relocates its files is followed.
	let info = at(aia(1, &[(0, 0x10)], &[0x10], 0x9_8765_0000, |_| {})).parse().expect("parses");
	assert_eq!(info.imsic_base, 0x9_8765_0000);
	assert_eq!(info.imsic_size, 0x1000, "one file, one page");
}

#[test]
fn a_guest_or_group_indexed_layout_is_reported_rather_than_flattened() {
	// The AIA binding lets a machine index its files by guest and by group, which puts a hart's file
	// somewhere `base + hart * 4096` does not. The parser states what the machine said; refusing it
	// is the kernel's call, and it cannot make it from an address alone.
	let guest = at(aia(1, &[(0, 0x10)], &[0x10], 0x2800_0000, |b| {
		b.prop_u32("riscv,guest-index-bits", 3);
	}))
	.parse()
	.expect("parses");
	assert_eq!(guest.imsic_guest_index_bits, 3);
	let group = at(aia(1, &[(0, 0x10)], &[0x10], 0x2800_0000, |b| {
		b.prop_u32("riscv,group-index-bits", 2).prop_u32("riscv,group-index-shift", 24);
	}))
	.parse()
	.expect("parses");
	assert_eq!(group.imsic_group_index_bits, 2);
	// And a controller that says how many identities it carries is taken at its word.
	let ids = at(aia(1, &[(0, 0x10)], &[0x10], 0x2800_0000, |b| {
		b.prop_u32("riscv,num-ids", 31);
	}))
	.parse()
	.expect("parses");
	assert_eq!(ids.imsic_num_ids, 31);
}

const AARCH64_GICV3: &[u8] = include_bytes!("../tests/qemu-virt-aarch64-gicv3.dtb");

#[test]
fn a_gicv3_machine_describes_a_distributor_and_a_redistributor_region() {
	// THE SAME TWO RANGES MEAN DIFFERENT THINGS. A GICv2's second `reg` range is the memory-mapped
	// CPU interface; a GICv3 has no such thing - its CPU interface is system registers - and the
	// second range is the REDISTRIBUTOR region, one frame pair per core. A driver that read the
	// version out of the shape of `reg` would drive one controller's registers on the other.
	let info = at(AARCH64_GICV3).parse().expect("the GICv3 machine parses");
	assert_eq!(info.gic_version, 3, "the version comes from `compatible`");
	assert_eq!(info.gic_dist, 0x0800_0000, "the distributor");
	assert_eq!(info.gic_dist_size, 0x1_0000);
	assert_eq!(info.gic_cpu, 0x080a_0000, "the redistributor region");
	// One 128 KiB frame pair per core, and QEMU sizes the region for the machine's maximum.
	assert!(info.gic_cpu_size >= 0x2_0000 * 8, "the region holds a frame pair for every core");
	assert_eq!(info.gic_msi, 0, "this machine signals MSIs through an ITS, which is not a v2m frame");
	assert_eq!(info.cpu_count, 8, "and the same tree still describes the cores");
}

const AARCH64_GICV3_ITS: &[u8] = include_bytes!("../tests/qemu-virt-aarch64-gicv3-its.dtb");

#[test]
fn a_gicv3_its_machine_names_its_translator_and_how_a_device_is_identified_to_it() {
	// An ITS is the OTHER kind of ARM MSI controller and nothing like a v2m frame: a device writes
	// an EVENT id to one register, and the ITS turns (device, event) into an LPI aimed at a core.
	// Which device it thinks wrote is the RequesterID mapped through the host bridge's `msi-map`, so
	// a kernel that cannot read that mapping cannot name its own devices to the controller.
	let info = at(AARCH64_GICV3_ITS).parse().expect("the GICv3/ITS machine parses");
	assert_eq!(info.gic_version, 3);
	assert_ne!(info.gic_its, 0, "the ITS is a child of the controller and is found there");
	assert!(info.gic_its_size >= 0x2_0000, "its two 64 KiB frames - the control page and the translator");
	assert_eq!(info.pci_msi_length, 0x10000, "the mapping covers the whole bus");
	assert_eq!(info.pci_msi_rid_base, 0, "and it is the identity on this machine");
	assert_eq!(info.pci_msi_devid_base, 0);
}

#[test]
fn a_gicv3_without_an_its_declares_none() {
	// The same controller, one child node fewer. `its=off` is a machine with no MSI controller at
	// all, which is a boot that schedules and has no device interrupts - not one to guess an
	// address for.
	let info = at(AARCH64_GICV3).parse().expect("parses");
	assert_eq!(info.gic_its, 0);
	assert_eq!(info.gic_msi, 0, "and no v2m frame either");
}

// ---------------------------------------------------------------------------------------------
// NUMA: which bank and which hart belong to which node, and how far apart the nodes are.
// ---------------------------------------------------------------------------------------------

// A two-node machine of the shape QEMU emits for `-numa node,...` on `virt`: a memory node and two
// cpus per NUMA node, and a `/distance-map` giving the non-local distance.
fn two_node_machine(distances: bool) -> &'static [u8] {
	let mut builder = Builder::new();
	builder.begin("");
	builder.prop_u32("#address-cells", 2).prop_u32("#size-cells", 2);
	builder.begin("memory@40000000").prop("device_type", b"memory\0").prop_reg64(0x4000_0000, 0x1000_0000).prop_u32("numa-node-id", 0).end();
	builder.begin("memory@50000000").prop("device_type", b"memory\0").prop_reg64(0x5000_0000, 0x1000_0000).prop_u32("numa-node-id", 1).end();
	builder.begin("cpus").prop_u32("#address-cells", 1).prop_u32("#size-cells", 0);
	builder.begin("cpu@0").prop_u32("reg", 0).prop_u32("numa-node-id", 0).end();
	builder.begin("cpu@1").prop_u32("reg", 1).prop_u32("numa-node-id", 0).end();
	builder.begin("cpu@2").prop_u32("reg", 2).prop_u32("numa-node-id", 1).end();
	builder.begin("cpu@3").prop_u32("reg", 3).prop_u32("numa-node-id", 1).end();
	builder.end();
	if distances {
		let mut matrix: Vec<u8> = Vec::new();
		for (from, to, distance) in [(0u32, 0u32, 10u32), (0, 1, 21), (1, 0, 31), (1, 1, 10)] {
			matrix.extend_from_slice(&from.to_be_bytes());
			matrix.extend_from_slice(&to.to_be_bytes());
			matrix.extend_from_slice(&distance.to_be_bytes());
		}
		builder.begin("distance-map").prop_str("compatible", "numa-distance-map-v1").prop("distance-matrix", &matrix).end();
	}
	builder.end();
	builder.finish()
}

#[test]
fn every_bank_and_every_hart_carries_the_node_the_tree_gave_it() {
	let info = at(two_node_machine(true)).parse().expect("a two-node tree parses");
	assert_eq!(info.ram_region_count, 2);
	assert_eq!(info.ram_regions[0], (0x4000_0000, 0x1000_0000));
	assert_eq!(info.ram_region_nodes[0], 0);
	assert_eq!(info.ram_regions[1], (0x5000_0000, 0x1000_0000));
	assert_eq!(info.ram_region_nodes[1], 1);
	assert_eq!(info.cpu_count, 4);
	assert_eq!(&info.cpu_node_ids[..4], &[0, 0, 1, 1]);
}

#[test]
fn a_distance_map_is_read_as_directed_triples() {
	let info = at(two_node_machine(true)).parse().expect("parses");
	assert_eq!(info.numa_distance_count, 4);
	assert_eq!(info.numa_distances[0], (0, 0, 10));
	assert_eq!(info.numa_distances[1], (0, 1, 21));
	// ASYMMETRIC, AND KEPT SO. The way back is a different cell in a real machine's map, and a
	// reader that stored one number per pair would quietly halve it.
	assert_eq!(info.numa_distances[2], (1, 0, 31));
	assert_eq!(info.numa_distances[3], (1, 1, 10));
}

#[test]
fn a_tree_with_no_numa_properties_says_unknown_rather_than_node_zero() {
	// THE DEFAULT MATTERS AS MUCH AS THE PARSE. A single-node machine's banks have no
	// `numa-node-id`, and reporting them as node zero would make an allocator believe in a locality
	// the firmware never claimed - which is indistinguishable, later, from a real one-node machine.
	let info = at(machine(|_| {})).parse().expect("parses");
	assert_eq!(info.ram_region_count, 1);
	assert_eq!(info.ram_region_nodes[0], NUMA_NODE_UNKNOWN);
	assert_eq!(info.cpu_node_ids[0], NUMA_NODE_UNKNOWN);
	assert_eq!(info.numa_distance_count, 0, "and no distance map is no distances, not a fabricated one");
}

#[test]
fn a_two_node_tree_without_a_distance_map_still_places_its_memory() {
	let info = at(two_node_machine(false)).parse().expect("parses");
	assert_eq!(info.ram_region_nodes[0], 0);
	assert_eq!(info.ram_region_nodes[1], 1);
	assert_eq!(info.numa_distance_count, 0);
}
