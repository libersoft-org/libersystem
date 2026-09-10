// The boot's DMA mode, latched over every manifest this loader verified and handed to the kernel.
//
// WHAT THE LOADER ADDS, AND WHAT IT DOES NOT. The mode is a kernel-owned admission decision; the
// loader is the one component that has checked a signature and read the medium, so it is the
// loader that establishes WHERE the value came from. Two provenances, and the loader writes each
// only for a value it can vouch for:
//
//   signed    every selected manifest is at the current version, every one carries a mode, and
//             every mode agrees. The value was verified against a signature this loader checked.
//   harness   every selected manifest carries NO mode - a tag-0 set, the test and development
//             builders' - and this entry path's harness input carried the frozen eight-byte record
//             with `harness` provenance. The loader relays it, validated, and the kernel on x86_64
//             revalidates the still-readable input against what was relayed.
//
// Everything else refuses BEFORE the hand-off, with the sentence that names it: a legacy manifest,
// a mixed set, disagreeing values, a signed set with a harness carrier beside it, a tag-0 set with
// no carrier or a malformed one, and an INDEPENDENT input on the path - the ESP file on x86_64,
// where only `fw_cfg` is the input, and the device-tree node on the two UEFI ports, where only the
// ESP file is. A boot-media-controlled value may never present itself as authenticated policy, and
// two producers agreeing is still two producers.

use bootproto::dma_mode::{Carrier, Handoff, Latch, LatchRefusal, Mode};

// THE LATCH, recorded by `trust::verify_for` for every manifest it accepts - the same set the
// release latch is taken over - and resolved once, before the hand-off.
static mut LATCH: Latch = Latch::new();

// What the hand-off carries, once resolved. `None` until `resolve` has run; the arch backends read
// it when they build `BootInfo`, which is after.
static mut HANDOFF: Option<Handoff> = None;

pub(crate) fn record(dma_mode: Option<u32>) {
	// SAFETY: the loader is single-threaded and this is reached only from its own boot path.
	unsafe {
		let latch: *mut Latch = &raw mut LATCH;
		(*latch).record(dma_mode);
	}
}

fn latch() -> Latch {
	// SAFETY: as above; a copy of a `Copy` value.
	unsafe { *(&raw const LATCH) }
}

// The words the arch backends write into `BootInfo`. Zero and zero - ABSENT - if `resolve` refused,
// which cannot reach a hand-off because `resolve` halts; stated rather than unwrapped so a backend
// that were ever called first would hand over absence, which the kernel refuses, rather than panic.
pub(crate) fn handoff_words() -> (u32, u32) {
	// SAFETY: as above.
	match unsafe { *(&raw const HANDOFF) } {
		Some(handoff) => handoff.words(),
		None => (bootproto::dma_mode::MODE_ABSENT, bootproto::dma_mode::PROVENANCE_ABSENT),
	}
}

// What this entry path's harness input looked like, and whether an INDEPENDENT input is present.
pub(crate) struct Inputs {
	// The one input this architecture's UEFI path reads: `fw_cfg` on x86_64, the ESP file on the
	// device-tree ports.
	pub carrier: Carrier,
	// An input that is not this path's, present anyway: the ESP file on x86_64, the device tree's
	// boot-policy node on the two UEFI ports. Named so the refusal can say which.
	pub independent: Option<&'static str>,
}

// Resolve the mode this boot runs under, or halt with the reason. Called once, after every source
// has been verified and before the hand-off.
pub(crate) fn resolve(inputs: Inputs) {
	if let Some(what) = inputs.independent {
		crate::arch::serial::write_str("loader: FATAL - ");
		crate::arch::serial::write_str(what);
		crate::arch::serial::write_str(" is present beside this entry path's DMA-mode input: an independent producer, refused whether or not it agrees\n");
		crate::arch::halt();
	}
	let latch = latch();
	match latch.resolve(inputs.carrier) {
		Ok(handoff) => {
			// SAFETY: single-threaded loader, own boot path.
			unsafe { *(&raw mut HANDOFF) = Some(handoff) };
			crate::arch::serial::write_str("loader: DMA mode ");
			crate::arch::serial::write_str(handoff.mode().name());
			crate::arch::serial::write_str(match handoff {
				Handoff::Signed(_) => " (signed - every selected manifest carries it and they agree)\n",
				Handoff::Harness(_) => " (harness - every selected manifest declares none and this path's carrier supplied it)\n",
			});
		}
		Err(refusal) => {
			crate::arch::serial::write_str("loader: FATAL - no DMA mode can be handed to the kernel: ");
			crate::arch::serial::write_str(refusal.message());
			match refusal {
				LatchRefusal::CarrierMalformed(reason) => {
					crate::arch::serial::write_str(" (");
					crate::arch::serial::write_str(malformed_name(reason));
					crate::arch::serial::write_str(")");
				}
				LatchRefusal::NothingSelected => {}
				_ => {}
			}
			crate::arch::serial::write_str(" - over ");
			crate::serial_write_usize(latch.manifests());
			crate::arch::serial::write_str(" verified manifest(s)\n");
			crate::arch::halt();
		}
	}
}

fn malformed_name(reason: bootproto::dma_mode::Malformed) -> &'static str {
	use bootproto::dma_mode::Malformed;
	match reason {
		Malformed::Length(_) => "not eight bytes",
		Malformed::Magic => "not the LSDM magic",
		Malformed::Version(_) => "an unknown record version",
		Malformed::Mode(_) => "a mode byte this format does not define",
		Malformed::Provenance(_) => "a provenance that is not `harness` - a replaceable medium may not claim authentication",
		Malformed::Reserved(_) => "a reserved byte that is not zero",
	}
}

// The mode a carrier names, for a backend that wants to say it. Unused where the sentence above is
// enough, kept so the type is spelled once.
#[allow(dead_code)]
pub(crate) fn mode_name(mode: Mode) -> &'static str {
	mode.name()
}

// The name the refusal above prints for the ports' independent producer, spelled once.
const BOOT_POLICY_NODE: &str = "the device tree's boot-policy node";

// Does the device tree the FIRMWARE PUBLISHED carry the boot-policy node? The two device-tree ports
// call this to fill `Inputs::independent`.
//
// THE COMPONENT THAT READS A TREE IS THE COMPONENT THAT CHECKS IT, and on these ports that is not
// always this one. Measured on the two-producer fixture: the UEFI firmware these ports run describes
// the machine with ACPI and publishes NO device-tree configuration table, so this check is handed
// nothing and the loader hands a mode over - while the kernel, which has to have a tree because this
// architecture describes its hardware with one, takes the machine's tree out of low DRAM, reads the
// boot-policy node and refuses every device claim. Both refusals are the same rule, and each belongs
// to whichever component actually reads the tree.
//
// AND THIS CHECK DOES NOT GO LOOKING FOR A TREE ITSELF. Making it walk low DRAM the way the kernel's
// reader does was tried and measured: the loader runs under the FIRMWARE's page tables, and a
// firmware is entitled to leave pages inside its own conventional memory unmapped - guard pages, on
// this one - so the walk took a synchronous exception 119 MB into the window and killed the boot on
// the ORDINARY row, where there is no second producer at all. A loader may read the addresses
// firmware handed it. It may not go fishing in memory firmware did not describe.
//
// # Safety
// `published` must be 0 or the address the firmware published for a device tree, and
// `phys_to_virt` must reach it - `Fdt`'s methods dereference what this is handed (FDT-007).
// Unused on x86_64, whose independent input is a file on the ESP rather than a node in a tree.
#[allow(dead_code)]
pub(crate) unsafe fn independent_tree(published: u64, phys_to_virt: fn(u64) -> u64) -> Option<&'static str> {
	if published == 0 {
		return None;
	}
	// SAFETY: the caller's contract covers this address.
	let tree = unsafe { fdt::Fdt::new(published, phys_to_virt) };
	// VALIDITY FIRST. Every other method on this type walks what the header declares, so asking one
	// of them about an address that carries no header reads wherever that garbage points.
	(tree.is_valid() && tree.boot_policy_record().is_some()).then_some(BOOT_POLICY_NODE)
}
