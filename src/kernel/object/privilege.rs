// Privilege: a named authority over one part of the machine that has no object of its own.
//
// Most of what a syscall lets a caller do is reached through something it holds - a Channel to
// send on, a MemoryObject to map, a DeviceMemory to touch. Three syscalls had nothing to hold:
// `SYS_FRAMEBUFFER_MAP` took the display, `SYS_CONSOLE_ATTACH` redirected the console's input
// sink, and `SYS_CONSOLE_FEED` injected keystrokes into a privileged console - each of them
// available to any process that knew the number. Taking the display before DisplayService did,
// or silencing the kernel console at will, needed no authority at all.
//
// A Privilege is that missing object: a bare capability whose whole content is WHICH authority
// it is. Three are minted once at boot and handed down the boot chain like the power capability
// beside them, and the three syscalls check for the matching kind. There is no way to make one
// from userspace - `create` is not reachable through any syscall - so the set that exists at the
// end of boot is the set that will ever exist.
//
// They are deliberately three rather than one. Feeding the console and owning the display are
// different authorities held by different components, and a single "console capability" would
// mean whoever may type into the console may also take the screen.

#![allow(dead_code)]

use alloc::sync::Arc;
use core::any::Any;

use super::{KernelObject, ObjectHeader, ObjectType, impl_kernel_object};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PrivilegeKind {
	// Owns the display: may take the framebuffer from the kernel console. DisplayService.
	DisplayController,
	// May inject keyboard and serial bytes into the console input path. The input driver,
	// and the development agent's serial relay.
	ConsoleInputSource,
	// May register the channel the kernel feeds console input to, which also silences the
	// kernel's own framebuffer console. ConsoleService.
	ConsoleSink,
	// May take a device's memory, its MSI-X vectors and its interrupt lines out of the kernel and
	// hand them to a driver. DeviceManager, inside the core service.
	//
	// Ungated, this was the widest hole in the syscall surface: `SYS_DEVICE_ACQUIRE(index)` minted a
	// `DeviceMemory` capability to any caller that named an index, so any ring-3 process could take
	// the BAR of any PCI device - which contradicts `DeviceMemory`'s own documentation that a driver
	// is handed only its device. On a DMA-capable device it is worse than an MMIO takeover: with no
	// IOMMU, a process holding both DMA buffers and physical addresses reaches memory the page
	// tables were meant to isolate.
	DeviceManager,
}

impl PrivilegeKind {
	pub fn name(self) -> &'static str {
		match self {
			PrivilegeKind::DisplayController => "DisplayController",
			PrivilegeKind::ConsoleInputSource => "ConsoleInputSource",
			PrivilegeKind::ConsoleSink => "ConsoleSink",
			PrivilegeKind::DeviceManager => "DeviceManager",
		}
	}
}

pub struct Privilege {
	header: ObjectHeader,
	kind: PrivilegeKind,
}

impl Privilege {
	pub fn create(kind: PrivilegeKind) -> Arc<Self> {
		Arc::new(Self { header: ObjectHeader::new(), kind })
	}

	pub fn kind(&self) -> PrivilegeKind {
		self.kind
	}
}

impl_kernel_object!(Privilege, Privilege);
