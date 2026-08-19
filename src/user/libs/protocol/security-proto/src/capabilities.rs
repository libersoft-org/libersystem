// WHAT A GENERATED ENCODER DOES WITH A CAPABILITY IT WAS GIVEN (P02M0090, WIRE-001).
//
// `pipeline-result` is the honest case rather than a fixture: it carries `group: handle<task-group>`
// beside a plain `stages: u32`, so its three encoders each have to answer what happens to the
// capability. Two of them cannot carry it and must refuse; the third takes both halves.
//
// The refusal used to be a line the CODE GENERATOR emitted at each site, which made it a property of
// the generator rather than of the codec - a hand-written encoder calling `VecWriter::into_inner`
// got the bytes and dropped the handle record, and `wire`'s own test for that was named for the
// property and asserted its opposite. `into_inner` and `SliceWriter::finish` are the refusal now, so
// what is worth testing is that the generated code ENDS IN THEM, which is only observable by running
// it.

use crate::codec::Handles;
use crate::generated::liber::security::v1::PipelineResult;

// A handle number the kernel would have given; nothing here dereferences it.
const GROUP: u64 = 0x2a;

fn with_group() -> PipelineResult {
	PipelineResult { group: GROUP, stages: 3 }
}

fn without_group() -> PipelineResult {
	PipelineResult { group: 0, stages: 3 }
}

#[test]
fn the_fixed_buffer_encoder_refuses_a_value_carrying_a_capability() {
	let mut out = [0u8; 64];
	assert_eq!(with_group().encode(&mut out), None, "a length beside a lost capability is not an encoding");
}

#[test]
fn the_owned_encoder_refuses_a_value_carrying_a_capability() {
	assert_eq!(with_group().encode_vec(), None, "and neither are the bytes alone");
}

#[test]
fn the_message_encoder_takes_both_halves() {
	let (bytes, handles) = with_group().encode_message().expect("a capability-bearing value encodes as a message");
	assert_eq!(handles.as_slice(), &[GROUP], "the capability travels beside the bytes");
	// And back: the decode side adopts it, and says so by clearing the caller's list.
	let mut carried = Handles::try_from_slice(handles.as_slice()).expect("one handle fits");
	let decoded = PipelineResult::decode_message(&bytes, &mut carried).expect("the message decodes");
	assert_eq!(decoded.group, GROUP);
	assert_eq!(decoded.stages, 3);
	assert!(carried.is_empty(), "the value adopted the capability, so the caller no longer holds it");
}

// THE OTHER HALF OF THE REFUSAL. Without this, an encoder that refused everything would pass the
// three tests above, and the property under test would be "these functions return None".
//
// `audit-entry` is the neighbour with no handle in it, which is the distinction being drawn: the
// refusal is about the SCHEMA carrying a capability and not about `encode` having become a function
// that fails. It is not `pipeline-result` with `group: 0` - `set_handle` records a slot whatever
// number goes into it, so a zero there is still a recorded capability and still a refusal, which is
// the right answer and not the one this test is looking for.
#[test]
fn a_value_carrying_no_capability_still_encodes_both_ways() {
	use crate::generated::liber::security::v1::{AuditEntry, Capability};
	let entry = AuditEntry { component: alloc::string::String::from("shell"), capability: Capability::Storage, granted: true, dynamic: false };
	let mut out = [0u8; 64];
	let n = entry.encode(&mut out).expect("nothing was recorded, so there is nothing to lose");
	assert!(n > 0);
	let bytes = entry.encode_vec().expect("and the owned encoder gives its bytes");
	assert_eq!(&out[..n], &bytes[..], "the two encoders agree on the bytes");
}

// And the zero case stated as itself, because it is the one a reader would guess wrong.
#[test]
fn a_handle_slot_holding_zero_is_still_a_recorded_capability() {
	let mut out = [0u8; 64];
	assert_eq!(without_group().encode(&mut out), None, "`set_handle` records the slot, not the number in it");
	assert_eq!(without_group().encode_vec(), None);
	let (_, handles) = without_group().encode_message().expect("and the message encoder carries the slot");
	assert_eq!(handles.as_slice(), &[0]);
}
