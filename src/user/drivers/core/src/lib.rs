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
// THE ACPI FAN, the driver's parts a host can test: `_FIF`, `_FPS` and `_FST` read as the specification lays them out.
pub mod acpi_fan;
// ACPI BATTERY, AC AND THERMAL ZONE, the driver's parts a host can test: the class a node is, its methods' results read
// at the specification's offsets, and the bound on a storm of notifications.
pub mod acpi_power;
// THE ACPI BUTTONS AND THE LID, the driver's parts a host can test: the class a node is, what its `Notify` values mean and
// what `_LID` answers.
pub mod acpi_button;
// THE ACPI TIME AND ALARM DEVICE, the driver's parts a host can test: `_GCP`'s capabilities, `_GRT`'s time and what a
// sleep's timed wake programs into a timer.
pub mod acpi_tad;
// THE AHCI DECISIONS, with no controller behind them: the port bitmap, the port state rules, the
// scatter-gather arithmetic and the capacity parse, which is where an AHCI driver is actually wrong.
pub mod ahci;
// THE BMC AS AN `ipmi` BINDING READS AND DRIVES IT: identity, the SDR repository, readings, the SEL, FRU, LAN, and the
// two administrative executors' rules - every sequence of transactions, over one transaction at a time.
pub mod ipmi_bmc;
// A UCSI CONNECTOR AS `ucsi-acpi` READS AND DRIVES IT: its record for TypeCService and its `usb-c` source for
// PowerService, built from what the policy manager answered, and the rules a request is refused by before any command.
pub mod typec_ucsi;
// A PORT CONTROLLER'S CONNECTOR AS `tcpci` DESCRIBES AND REPORTS IT: the board's `usb-c-connector` read from the
// controller's property block, and its record and `usb-c` source built from what the sink engine reports.
pub mod typec_tcpci;
// AN ADMINISTRATIVE EXECUTOR'S PREPARED OPERATIONS: the payload copied and digested at preparation, the live
// target generation frozen with it, and the start guard that admits at most one attempt. The probe fixture
// uses it now and the DFU class module will.
pub mod admin_operation;
pub mod blk;
// THE EMULATED BLUETOOTH PEER the in-guest fixture plays: an LE boot mouse's responder side of
// pairing, its GATT table and its scripted reports. DEVELOPMENT-ONLY in what uses it - only the
// fixture driver links it - and its cryptography is a separate implementation from the host stack's,
// held to the same published vectors, so the two agreeing in the guest means something.
pub mod bt_peer;
// THE EMULATED BR/EDR WORLD the same fixture plays beside the mouse: the classic half of its controller and five
// classic devices, their L2CAP, SDP and RFCOMM written apart from the host stack's. DEVELOPMENT-ONLY in what uses it.
pub mod bt_world;
// THE EMULATED LE WORLD beyond the mouse: three LE peripherals pairing in every model, with private addresses, key
// distribution and attribute servers of their own. DEVELOPMENT-ONLY in what uses it.
pub mod bt_le_world;
// THE FIXTURE'S SBC: what the emulated headset reads from a stream and the emulated phone writes into one - its own
// frame reader, CRC, allocation and dequantization, apart from the host's codec. DEVELOPMENT-ONLY in what uses it.
pub mod bt_sbc;
// THE USB BLUETOOTH HCI TRANSPORT'S DECISIONS: the controller's pipes, and where one HCI packet ends in transfers
// that do not say.
pub mod bt_usb;
// THE COMMUNICATIONS-CLASS DECISIONS, shared by the two network models that are the same descriptors
// and different framing: where the interfaces and the bulk pair are, and - for NCM - what a transfer
// block's datagram table may say before it is believed, which is where such a driver reads past a
// buffer.
pub mod cdc;
// THE USB CCID CLASS'S DECISIONS: a reader's class descriptor, its messages, and its slot-change notifications.
pub mod ccid;
pub mod common;
// A DEVICE'S POWER STATE THROUGH ITS NODE CHANNEL: what a driver bound to a firmware node asks the ACPI service for, since
// the power resources behind a state are shared and counted there.
pub mod node_power;
// VIRTIO-SERIAL MULTIPORT AS PURE DECISIONS: which queues a port owns, what a control message means,
// and what this driver refuses. Every input here is bytes the DEVICE chose, which is why it is a
// module with fixtures rather than a branch inside a binary nobody can run on the host.
pub mod console;
pub mod descriptor;
// THE USB DFU CLASS'S DECISIONS: which interface is a target and in which mode, what a status says, and what an
// image's DFU suffix claims.
pub mod dfu;
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
// THE HID POWER DEVICE CLASS OVER THE COMMON HID FIELD TABLE: which fields carry a UPS's values, what the latest
// reports say, and how a control is written back.
pub mod hid_power;
// THE USB MONITOR CONTROL CLASS AND THE HID AMBIENT-LIGHT SENSOR OVER THE SAME TABLE: a monitor's brightness and EDID,
// and a sensor's illuminance.
pub mod hid_display;
// ACPI'S VIDEO EXTENSION AND ITS LIGHT SENSOR, the backlight and sensor drivers' parts a host can test: `_BCL`, `_BQC`,
// the output's notification map, the firmware that steps itself, and `_ALI`, `_ALR` and `_ALP`.
pub mod acpi_video;
// HID OVER I2C, the driver's parts a host can test: which collections publish what, which report goes where, the
// reset handshake's bound and the interrupt-storm rule.
pub mod i2c_hid;
pub mod input;
pub mod keys;
pub mod mbim;
// MBIM ABOVE THE FRAMING: the function's interfaces, its control messages and the BASIC_CONNECT information
// buffers a modem provider reads and writes.
pub mod mbim_cid;
pub mod net;
// THE NVM EXPRESS DECISIONS, with no controller behind them: the register layouts, the completion
// rules and the PRP arithmetic, which is where an NVMe driver is actually wrong and all of which a
// host test can watch failing.
pub mod nvme;
pub mod piv_card;
pub mod port;
// THE USB PRINTER CLASS'S DECISIONS: which interface setting to drive, the class requests, and what a port
// status byte and a device ID may say.
pub mod printer;
// THE USB STILL IMAGE CLASS AS A TRANSPORT: its setting, its class requests and the device status a cancel reads.
pub mod ptp;
// THE SD PROTOCOL DECISIONS, kept independent of how the controller is attached exactly as the item
// that owns them asks: no PCI, no ACPI, no device tree, so board glue stays outside the driver.
// THE SCSI COMMAND AND SENSE CORE, owned by the first of its three consumers to be written and
// consumed by the rest: one command set, several transports, one place that knows what its bytes
// mean.
pub mod scsi;
// SMBus transactions composed from I2C messages, and their packet error code - for a controller that moves
// plain I2C messages, as virtio-i2c does.
pub mod smbus;
// Virtio-i2c's requests (device 34) and virtio-gpio's per-line event state (device 41): the parts of the two
// drivers a host can test.
pub mod gpio;
pub mod i2c;
pub mod sdhci;
pub mod serial_port;
// THE 16550 REGISTER ENGINE, apart from where the UART is and what clocks it - the console UART's driver is the
// platform glue around it.
pub mod snd;
pub mod uart;
// THE WATCHDOG DRIVERS' ARITHMETIC: one timeout divided across each device's stages and units.
pub mod watchdog;
// THE WATCHDOG ACTION TABLE, run as the register reads and writes it lists against what the kernel minted.
pub mod wdat;
// THE USB ATTACHED SCSI INFORMATION UNITS: the same SCSI command set as the Bulk-Only path, carried
// over four pipes joined by a tag instead of two strictly in order. The tag is where a UAS driver is
// wrong, and it is big-endian in a transport that is little-endian everywhere else.
pub mod uac;
pub mod uas;
pub mod usb;
// The in-controller class-module execution model: what a USB class driver IS in this system, and the
// per-class budget that stops two of them inside one Domain from starving each other.
pub mod usb_class;
// ONE CONFIGURATION DESCRIPTOR READ ONCE INTO ITS INTERFACE SETTINGS, which every service-backed class binder
// reads instead of walking the records itself.
pub mod usb_function;
pub mod usb_midi;
// UNIVERSAL MIDI PACKETS: message lengths and groups, SysEx7 and SysEx8 counted, MIDI 1.0 packets to and from UMP, MIDI
// 2.0 channel voice down to MIDI 1.0, and the Group Terminal Blocks a USB MIDI 2.0 device lists.
pub mod ump;
pub mod uvc;
pub mod virtio;
// THE VIRTIO-VSOCK DECISIONS: the packet header, the credit window and the connection state
// machine. Credit is the part a vsock driver gets wrong, in both directions and silently, and it is
// modular arithmetic on two counters the PEER moves - which is exactly what a host test can watch.
pub mod vsock;
