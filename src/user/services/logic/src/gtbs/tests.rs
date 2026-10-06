use super::*;
use alloc::vec;

#[test]
// THE CALL AS EARBUDS READ IT, and their commands relayed only where a call is declared and the command fits it.
fn the_declared_call_is_served_and_commands_relayed() {
	assert!(call_state(Call::None).is_empty());
	assert_eq!(call_state(Call::Incoming), [1, 0x00, 0]);
	assert_eq!(call_state(Call::Outgoing), [1, 0x02, 1]);
	assert_eq!(current_calls(Call::Active), [3, 1, 0x03, 0]);
	assert_eq!(control_point(&[opcode::ACCEPT, 1], Call::None), (vec![0, 1, result::OPERATION_NOT_POSSIBLE], None));
	assert_eq!(control_point(&[opcode::ACCEPT, 1], Call::Incoming), (vec![0, 1, result::SUCCESS], Some(Command::Answer)));
	assert_eq!(control_point(&[opcode::TERMINATE, 1], Call::Incoming), (vec![1, 1, result::SUCCESS], Some(Command::Reject)));
	assert_eq!(control_point(&[opcode::TERMINATE, 1], Call::Active), (vec![1, 1, result::SUCCESS], Some(Command::HangUp)));
	assert_eq!(control_point(&[opcode::ACCEPT, 1], Call::Active).1, None, "nothing to accept");
	assert_eq!(control_point(&[opcode::TERMINATE, 2], Call::Active), (vec![1, 2, result::INVALID_CALL_INDEX], None));
	assert_eq!(control_point(&[0x04, 1], Call::Active), (vec![4, 1, result::OPCODE_NOT_SUPPORTED], None));
	assert_eq!(characteristics(Call::None).len(), 12);
}
