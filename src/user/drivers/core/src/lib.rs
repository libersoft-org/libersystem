#![cfg_attr(not(test), no_std)]

extern crate alloc;

// WHAT EVERY DRIVER BINARY SHARES, as a library, because that is what it already was.
//
// These three were `mod` declarations repeated in each binary: the transport compiled seven times,
// and every binary reported the parts it does not call as dead code - which is a true fact about
// that binary and a false one about the module. The only way to say so in a binary crate was to
// switch the lint off at the top of the file, for all seven at once, which is how a genuinely dead
// duplicate of `read_isr` sat here unnoticed.
//
// A library says it properly: this is the surface a driver may use, and no binary owes it a caller.
// Nothing here is exempt from having ONE - the items with no caller anywhere in the tree were
// deleted before the move, not carried across by it.
// THE AHCI DECISIONS, with no controller behind them: the port bitmap, the port state rules, the
// scatter-gather arithmetic and the capacity parse, which is where an AHCI driver is actually wrong.
pub mod ahci;
pub mod blk;
pub mod common;
// VIRTIO-SERIAL MULTIPORT AS PURE DECISIONS: which queues a port owns, what a control message means,
// and what this driver refuses. Every input here is bytes the DEVICE chose, which is why it is a
// module with fixtures rather than a branch inside a binary nobody can run on the host.
pub mod console;
pub mod descriptor;
pub mod gpu;
// THE HIGH DEFINITION AUDIO DECISIONS: the verb packing, the two rings, the widget graph walk and
// the format word. Most of an HDA driver is not register access, and this is the part that is not.
pub mod hda;
// THE HID REPORT-DESCRIPTOR PARSER AND REPORT DECODER. It was a module INSIDE the xHCI binary,
// where it could not be tested on the host at all - which is how a descriptor whose bit cursor
// overflows the admission check, and a short report that leaves the previous one's tail standing,
// both survived. Nothing about it is transport-specific: a USB HID device, an I2C one and a
// Bluetooth one all speak the same descriptors.
pub mod hid;
pub mod input;
pub mod keys;
pub mod net;
// THE NVM EXPRESS DECISIONS, with no controller behind them: the register layouts, the completion
// rules and the PRP arithmetic, which is where an NVMe driver is actually wrong and all of which a
// host test can watch failing.
pub mod nvme;
pub mod port;
// THE SD PROTOCOL DECISIONS, kept independent of how the controller is attached exactly as the item
// that owns them asks: no PCI, no ACPI, no device tree, so board glue stays outside the driver.
// THE SCSI COMMAND AND SENSE CORE, owned by the first of its three consumers to be written and
// consumed by the rest: one command set, several transports, one place that knows what its bytes
// mean.
pub mod scsi;
pub mod sdhci;
pub mod serial_port;
pub mod snd;
pub mod usb;
// The in-controller class-module execution model: what a USB class driver IS in this system, and the
// per-class budget that stops two of them inside one Domain from starving each other.
pub mod usb_class;
pub mod virtio;
