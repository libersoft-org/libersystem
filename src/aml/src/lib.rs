//! A BOUNDED ACPI MACHINE LANGUAGE INTERPRETER.
//!
//! ACPI firmware describes most of a PC's board as a PROGRAM: the DSDT and the SSDTs are AML bytecode that declares
//! a namespace of devices and the methods that answer for them - "to read the battery, run `_BST`". This crate is
//! the interpreter that runs it, in the userspace ACPI service, never in the kernel: firmware code can be wrong, and
//! a defect in it must cost a restart of that service rather than the machine.
//!
//! WHAT IT IS:
//!   - the namespace: every table loaded into one tree, `Load` and `LoadTable` included (a loaded table checked like
//!     any other), `Unload` refused;
//!   - method evaluation, BOUNDED - opcodes, time, call depth, expression nesting, memory, loop iterations and
//!     package depth - a method past a bound aborted and the bound named, never retried;
//!   - operation regions through a HOST (`host::Host`) that performs every access under the kernel's policy:
//!     system memory, system I/O, PCI configuration (the function found from `_SEG`, `_BBN` and `_ADR`), CMOS, the
//!     embedded controller, `GeneralPurposeIo` for input reads only and `GenericSerialBus` - any other space refused;
//!   - what the OS says it is: `_OSI` true for the Windows strings and the features implemented (`interp::OSI_TRUE`),
//!     `\_OS` "Microsoft Windows NT", `\_REV` 2;
//!   - the device walk (`_STA`, `_INI`), identities, `_CRS` (`resource`), `_DSD` (`dsd`), `_OSC` and `_DSM`.
//!
//! One evaluation runs at a time: the interpreter has one thread, so a `Mutex` is always its own to take and an
//! `Event` nothing else can signal.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod convert;
pub mod devices;
pub mod dsd;
pub mod ec;
pub mod error;
mod field;
pub mod host;
pub mod interp;
pub mod name;
pub mod namespace;
pub mod object;
pub mod resource;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod wire;

pub use devices::{Found, Identity, Kind, Walk};
pub use error::{Bound, Error, Limits};
pub use host::{Access, Host, HostError, PciAddress, TableBytes};
pub use interp::Aml;
pub use name::{NameString, Path, Seg};
pub use namespace::{NodeId, ROOT};
pub use object::{Object, Space};

#[cfg(test)]
mod tests;
