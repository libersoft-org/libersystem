// The monotonic boot floor, enforced before the hand-off by a build that says it does.
//
// WHAT THIS FILE ADDS TO `bootproto::rollback`, AND WHAT IT DOES NOT. The record, the classification
// of what firmware answered, the comparison and the order of the writes are all shared and
// host-tested there; this file supplies the firmware (UEFI runtime services, through the typed
// store in the `uefi` crate), the generation the manifests latched, and the sentences. Nothing
// here decides anything the shared code has not already decided.
//
// ENFORCEMENT IS A BUILD IDENTITY. Only a loader built with `LIBER_TRUST_PROFILE=rollback-enforcing`
// reads or writes the floor; `test-trust` and `external-release` say so and go on exactly as before,
// with the per-run fresh variables image every ordinary boot on this machine uses. The enforcing
// profile is signed for the firmware by a signer of its own, which is what makes replacement media
// unable to offer a current, correctly signed loader that ignores the floor.

use bootproto::rollback::{Fault, Invalid, MarkerRefusal, Outcome, Refusal, Slot, SlotState, WriteFault};

use crate::trust;

// The one line a gate reads to know this build enforces. Written once, never assembled.
pub const ENFORCING_MARKER: &str = "ROLLBACK FLOOR ENFORCED";

// Compare the selected set's generation with the floor, converge or advance it, or halt. Called
// once, after every source has been verified and the DMA mode resolved, and before the hand-off.
pub(crate) fn enforce(system_table: *const uefi::SystemTable) {
	if !trust::IS_ROLLBACK_ENFORCING {
		crate::arch::serial::write_str("loader: rollback floor - not enforced by this build's trust profile\n");
		return;
	}
	// AN ENFORCING BUILD BOOTS NOTHING IT DID NOT VERIFY. A generation comes only from a signed
	// manifest at the current format; a boot that verified none - the text-manifest fallbacks -
	// has no number to compare and stops here rather than reading "no manifest" as zero.
	let Some(generation) = trust::latched_generation() else {
		crate::arch::serial::write_str("loader: FATAL - rollback floor: no signed manifest supplied a security generation, and an enforcing build does not read absence as zero\n");
		crate::arch::halt();
	};
	let Some(mut store) = (unsafe { uefi::variables::RollbackStore::new(system_table) }) else {
		crate::arch::serial::write_str("loader: FATAL - rollback floor: the firmware offers no runtime services, so no floor can be kept\n");
		crate::arch::halt();
	};
	let product = bootproto::rollback::product_identity(trust::THIS_PRODUCT);
	match bootproto::rollback::enforce(&mut store, &product, generation) {
		Ok(Outcome::Unprovisioned { generation }) => {
			crate::arch::serial::write_str("loader: rollback floor - this machine is UNPROVISIONED (no provisioned marker): generation ");
			write_u64(generation);
			crate::arch::serial::write_str(" boots and the floor is NOT advanced\n");
		}
		Ok(Outcome::Accepted { previous, floor, converged }) => {
			crate::arch::serial::write_str("loader: rollback floor ");
			write_u64(floor);
			crate::arch::serial::write_str(" - generation ");
			write_u64(generation);
			crate::arch::serial::write_str(if floor > previous { " accepted, floor advanced from " } else { " accepted, equal to the floor of " });
			write_u64(previous);
			if let Some(slot) = converged.first {
				crate::arch::serial::write_str(", slot ");
				crate::arch::serial::write_str(slot.letter());
				crate::arch::serial::write_str(" written and read back");
			}
			if let Some(slot) = converged.second {
				crate::arch::serial::write_str(", slot ");
				crate::arch::serial::write_str(slot.letter());
				crate::arch::serial::write_str(" written and read back");
			}
			crate::arch::serial::write_str("; both slots carry the floor\n");
		}
		Err(Fault::Below { floor, generation }) => {
			crate::arch::serial::write_str("loader: FATAL - rollback floor ");
			write_u64(floor);
			crate::arch::serial::write_str(" REFUSES generation ");
			write_u64(generation);
			crate::arch::serial::write_str(": a correctly signed release older than the highest this machine has accepted\n");
			crate::arch::halt();
		}
		Err(Fault::Refused(refusal)) => {
			crate::arch::serial::write_str("loader: FATAL - rollback floor: ");
			write_refusal(refusal);
			crate::arch::serial::write_str(" - a provisioned machine whose state cannot be believed is not reset by a boot; the ceremony recovers it\n");
			crate::arch::halt();
		}
		Err(Fault::Write { slot, fault }) => {
			crate::arch::serial::write_str("loader: FATAL - rollback floor: writing slot ");
			crate::arch::serial::write_str(slot.letter());
			crate::arch::serial::write_str(match fault {
				WriteFault::NoWritePath => " is impossible - the firmware offers no SetVariable",
				WriteFault::Refused(_) => " was refused by the firmware",
			});
			crate::arch::serial::write_str(" - the floor was not advanced and control is not transferred\n");
			crate::arch::halt();
		}
		Err(Fault::Readback { slot, found }) => {
			crate::arch::serial::write_str("loader: FATAL - rollback floor: slot ");
			crate::arch::serial::write_str(slot.letter());
			crate::arch::serial::write_str(" did not read back as written (");
			write_slot_state(found);
			crate::arch::serial::write_str(") - a floor the firmware did not keep is not a floor, and control is not transferred\n");
			crate::arch::halt();
		}
	}
}

fn write_refusal(refusal: Refusal) {
	match refusal {
		Refusal::Marker(reason) => {
			crate::arch::serial::write_str("the provisioned marker ");
			crate::arch::serial::write_str(match reason {
				MarkerRefusal::Length(_) => "is not one byte",
				MarkerRefusal::Value(_) => "holds a value that is not 0x01",
				MarkerRefusal::Attributes(_) => "carries attributes that are not non-volatile boot-service access",
				MarkerRefusal::AccessDenied => "could not be read (access denied)",
				MarkerRefusal::DeviceError => "could not be read (device error)",
				MarkerRefusal::Failed(_) => "could not be read (firmware failure)",
			});
			crate::arch::serial::write_str(" - which is not absence");
		}
		Refusal::NoValidSlot { a, b } => {
			crate::arch::serial::write_str("the marker is present and neither slot holds a valid record (A: ");
			write_slot_state(a);
			crate::arch::serial::write_str(", B: ");
			write_slot_state(b);
			crate::arch::serial::write_str(")");
		}
	}
}

fn write_slot_state(state: SlotState) {
	crate::arch::serial::write_str(match state {
		SlotState::Valid(_) => "valid",
		SlotState::Absent => "absent",
		SlotState::Invalid(Invalid::Length(_)) => "not sixty-four bytes",
		SlotState::Invalid(Invalid::Magic) => "not this record format",
		SlotState::Invalid(Invalid::Version(_)) => "an unknown record version",
		SlotState::Invalid(Invalid::Reserved(_)) => "a reserved word that is not zero",
		SlotState::Invalid(Invalid::Product) => "another product's record",
		SlotState::Invalid(Invalid::Tag) => "a commit tag that does not match - torn, altered or copied from the other slot",
		SlotState::WrongAttributes(_) => "the wrong attributes",
		SlotState::AccessDenied => "unreadable (access denied)",
		SlotState::DeviceError => "unreadable (device error)",
		SlotState::Failed(_) => "unreadable (firmware failure)",
	});
	if let SlotState::Valid(floor) = state {
		crate::arch::serial::write_str(" at ");
		write_u64(floor);
	}
}

fn write_u64(value: u64) {
	crate::serial_write_usize(value as usize);
}

// Spelled once for the sentence above, so a slot's letter is never assembled by hand.
#[allow(dead_code)]
const fn letter(slot: Slot) -> &'static str {
	slot.letter()
}
