//! The twenty-one ambient symbols the pinned configuration names, answered WITHOUT an ambient
//! anything.
//!
//! WHAT THE UPSTREAM LOADER DOES, AND WHY NONE OF IT CAN HAPPEN HERE. Its Unix platform layer finds
//! drivers by reading environment variables for search paths, scanning directories under them for
//! JSON manifests, opening whichever it finds with `dlopen`, and resolving entry points with
//! `dlsym`. Every step of that is authority this system does not hand out: there is no ambient file
//! namespace, no environment search path, no current-directory plugin load and no arbitrary
//! `dlopen`.
//!
//! SO THE FUNCTIONS STAY AND THEIR ANSWERS COME FROM A RECORD. The loader still calls `opendir`,
//! `fopen` and `getenv`; what it gets back is decided by a bounded, versioned record the substrate
//! installs before any foreign code runs. `getenv` answers nothing, always. `opendir` enumerates
//! exactly the manifests the record names and nothing else. `loader_platform_open_library` hands
//! back a reference to a provider that is ALREADY in the verified closure - it opens nothing, and
//! can fail only by naming something that is not there.
//!
//! WHY NOT DELETE THE CALLS INSTEAD. Because the calls are the pinned upstream's, and patching them
//! out is a fork: every one removed is a line this tree then owns and has to re-remove at the next
//! revision. Answering them is the smaller change and it keeps the audit honest - what the inventory
//! measured is what the configuration asks for, and this is what the system chooses to say back.
//!
//! SYMBOL RESOLUTION IS TWO WELL-KNOWN EXPORTS AND THERE IS NO `dlsym`. A provider-scoped export
//! query would need module provenance in the kernel's export table and a new ring-3 syscall, which
//! is a kernel redesign this milestone has no business doing for a substrate audit.

#![no_std]

pub mod record;
pub mod version;

pub mod directory;
pub mod library;
pub mod stream;

#[cfg(test)]
mod tests;
