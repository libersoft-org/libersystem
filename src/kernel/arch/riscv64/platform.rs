// THE DEVICES THIS MACHINE'S DEVICE TREE DESCRIBES, as the descriptions `device::init` publishes: every node
// naming a `compatible` and resources, the kernel's own among them held - the APLIC, the IMSIC, the ACLINT,
// the goldfish clock, fw_cfg, the PCIe host, the console UART and the `sifive,test` reset device, whose
// authority is the system's power role and never a driver's.

use alloc::vec::Vec;

use crate::device::Described;

#[cfg(not(test))]
const KERNEL_HELD: &[&[u8]] = &[
	b"riscv,aplic",
	b"riscv,imsics",
	b"qemu,imsics",
	b"riscv,plic0",
	b"sifive,plic-1.0.0",
	b"riscv,clint0",
	b"sifive,clint0",
	b"riscv,aclint-mswi",
	b"riscv,aclint-mtimer",
	b"riscv,aclint-sswi",
	b"google,goldfish-rtc",
	b"qemu,fw-cfg-mmio",
	b"pci-host-ecam-generic",
	b"sifive,test0",
	b"sifive,test1",
	b"syscon",
];

// An APLIC specifier as a wired line: a source and its trigger (1 and 2 edges, 4 and 8 levels, 2 and 8 low). A
// PLIC's one cell is a controller this kernel does not drive, and is not a line.
#[cfg(not(test))]
fn aplic_line(route: &fdt::IntxRoute) -> Option<abi::WiredLine> {
	if route.cells != 2 || route.spec[0] == 0 {
		return None;
	}
	let flags = route.spec[1];
	Some(abi::WiredLine { number: route.spec[0], trigger: if flags & 0xc != 0 { abi::LINE_TRIGGER_LEVEL } else { abi::LINE_TRIGGER_EDGE }, polarity: if flags & 0xa != 0 { abi::LINE_POLARITY_LOW } else { abi::LINE_POLARITY_HIGH }, controller: abi::LINE_CONTROLLER_APLIC, _pad: 0 })
}

// Every description this machine's tree gives, and the system its SMBIOS tables name. A test kernel reads no
// tree - its rows are the suite's own.
pub fn describe() -> Vec<Described> {
	#[cfg(not(test))]
	{
		let out = super::device_tree().map(|tree| crate::arch::common::platform::from_tree(&tree, KERNEL_HELD, super::serial::UART_BASE, aplic_line)).unwrap_or_default();
		crate::arch::common::platform::report_smbios(super::boot::loader_smbios(super::boot::BOOT_ARG.load(core::sync::atomic::Ordering::SeqCst)), super::paging::phys_to_virt);
		out
	}
	#[cfg(test)]
	Vec::new()
}
