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

fn at(blob: &'static [u8]) -> Fdt {
	Fdt::new(blob.as_ptr() as u64, identity)
}

const AARCH64: &[u8] = include_bytes!("../tests/qemu-virt-aarch64.dtb");
const RISCV64: &[u8] = include_bytes!("../tests/qemu-virt-riscv64.dtb");

// ---------------------------------------------------------------------------------------------
// A tree builder, so the awkward cases can be written rather than found.
// ---------------------------------------------------------------------------------------------

const FDT_HEADER_LEN: usize = 40;

struct Builder {
	structure: Vec<u8>,
	strings: Vec<u8>,
}

impl Builder {
	fn new() -> Self {
		Self { structure: Vec::new(), strings: Vec::new() }
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
		// Header, an empty memory-reservation block, then the two blocks.
		let off_rsvmap = FDT_HEADER_LEN;
		let off_struct = off_rsvmap + 16;
		let off_strings = off_struct + structure.len();
		let total = off_strings + strings.len();
		let mut out = Vec::with_capacity(total);
		for word in [FDT_MAGIC, total as u32, off_struct as u32, off_strings as u32, off_rsvmap as u32, 17, 16, 0, strings.len() as u32, structure.len() as u32] {
			out.extend_from_slice(&word.to_be_bytes());
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
	builder.begin("memory@40000000").prop_reg64(0x4000_0000, 0x2000_0000).end();
	builder.begin("cpus").prop_u32("#address-cells", 1).prop_u32("#size-cells", 0).begin("cpu@0").prop_u32("reg", 0).end().end();
	body(&mut builder);
	builder.end();
	builder.finish()
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
	// QEMU - `0x0900_0000` for aarch64 and `0x1000_0000` for riscv64 - and P02M0129 has recorded
	// twice that on a machine that is not `virt` those stores go somewhere nobody chose.
	//
	// Discovery replaces the constant, and this is the evidence that it replaces it with the SAME
	// answer on the machine the constant was right for. If these two lines ever disagree with the
	// backends' fallbacks, one of the two is describing a machine that no longer exists.
	let arm = at(AARCH64).console().expect("aarch64 virt names its console");
	assert_eq!(arm, Console { uart: Uart::Pl011, base: 0x0900_0000, reg_shift: 0 });

	let risc = at(RISCV64).console().expect("riscv64 virt names its console");
	assert_eq!(risc, Console { uart: Uart::Ns16550, base: 0x1000_0000, reg_shift: 0 });
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
	builder.begin("memory@40000000").prop_reg64(0x4000_0000, 0x2000_0000).end();
	builder.begin("chosen").prop_str("stdout-path", "/soc/serial@10000000").end();
	builder.begin("soc").prop_u32("#address-cells", 1).prop_u32("#size-cells", 1);
	let mut reg = 0x1000_0000u32.to_be_bytes().to_vec();
	reg.extend_from_slice(&0x1000u32.to_be_bytes());
	builder.begin("serial@10000000").prop_str("compatible", "ns16550a").prop("reg", &reg).end();
	builder.end().end();
	let blob = builder.finish();
	assert_eq!(at(blob).console().map(|console| console.base), Some(0x1000_0000), "one cell means one cell");
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
	assert_eq!(at(blob).console(), Some(Console { uart: Uart::Ns16550, base: 0x1000_0000, reg_shift: 2 }));
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
