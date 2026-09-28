// THE DEVICES THIS MACHINE'S DEVICE TREE DESCRIBES, as the descriptions `device::init` publishes: every node
// naming a `compatible` and resources, the kernel's own among them held - the GIC with its ITS and v2m frames,
// the timer, the PL031 clock, fw_cfg, the PCIe host and the console UART.

use alloc::vec::Vec;

use crate::device::Described;

// THE NODES THIS KERNEL DRIVES ITSELF, by `compatible`. The clock stays the kernel's: it reads it for the
// wall clock and arms its alarm itself, so no driver is wanted for it.
#[cfg(not(test))]
const KERNEL_HELD: &[&[u8]] = &[
	b"arm,cortex-a15-gic",
	b"arm,cortex-a9-gic",
	b"arm,gic-400",
	b"arm,gic-v3",
	b"arm,gic-v3-its",
	b"arm,gic-v2m-frame",
	b"arm,armv8-timer",
	b"arm,armv7-timer",
	b"arm,pl031",
	b"qemu,fw-cfg-mmio",
	b"pci-host-ecam-generic",
];

// A GIC specifier as a wired line: a SHARED peripheral interrupt, by INTID, with the binding's flags - 1 and 2
// edges, 4 and 8 levels, 2 and 8 active low. A PPI is a core's own and never a device's line.
#[cfg(not(test))]
fn gic_line(route: &fdt::IntxRoute) -> Option<abi::WiredLine> {
	if route.cells != 3 || route.spec[0] != 0 {
		return None;
	}
	let intid = route.spec[1].checked_add(32)?;
	let flags = route.spec[2];
	Some(abi::WiredLine { number: intid, trigger: if flags & 0xc != 0 { abi::LINE_TRIGGER_LEVEL } else { abi::LINE_TRIGGER_EDGE }, polarity: if flags & 0xa != 0 { abi::LINE_POLARITY_LOW } else { abi::LINE_POLARITY_HIGH }, controller: abi::LINE_CONTROLLER_GIC, _pad: 0 })
}

// Every description this machine's tree gives, and the system its SMBIOS tables name. A test kernel reads no
// tree - its rows are the suite's own.
pub fn describe() -> Vec<Described> {
	#[cfg(not(test))]
	{
		let out = super::device_tree().map(|tree| crate::arch::common::platform::from_tree(&tree, KERNEL_HELD, super::serial::UART_BASE, gic_line)).unwrap_or_default();
		crate::arch::common::platform::report_smbios(super::boot::loader_smbios(super::boot::BOOT_ARG.load(core::sync::atomic::Ordering::SeqCst)), super::paging::phys_to_virt);
		out
	}
	#[cfg(test)]
	Vec::new()
}
