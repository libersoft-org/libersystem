use super::answer::*;
use super::command::*;
use super::*;
use alloc::collections::VecDeque;
use alloc::vec;

// A SCRIPTED PPM: every command sent is answered by what `answer` says - notifications at a time after the send, each
// setting CCI and MESSAGE_IN - and the model records what the OPM did, so a test can hold it to the discipline.
struct Model {
	now: u64,
	sent: Vec<u64>,
	pending: VecDeque<(u64, u32, Vec<u8>)>,
	cci: u32,
	message: Vec<u8>,
	refreshes: usize,
	// A reset completes after this many refreshes; `None` never.
	reset_after: Option<usize>,
	answer: fn(u64) -> Vec<(u64, u32, Vec<u8>)>,
}

impl Model {
	fn new(answer: fn(u64) -> Vec<(u64, u32, Vec<u8>)>) -> Model {
		Model { now: 1_000, sent: Vec::new(), pending: VecDeque::new(), cci: 0, message: Vec::new(), refreshes: 0, reset_after: Some(3), answer }
	}
}

impl Ppm for Model {
	fn send(&mut self, control: u64, _message_out: &[u8]) -> Result<(), Failure> {
		assert!(self.pending.is_empty(), "a command was sent while another was still being answered");
		self.sent.push(control);
		let now = self.now;
		for (delay, cci, message) in (self.answer)(control) {
			self.pending.push_back((now + delay, cci, message));
		}
		Ok(())
	}
	fn wait_notify(&mut self, deadline: u64) -> Result<bool, Failure> {
		match self.pending.front() {
			Some(&(at, _, _)) if at <= deadline => {
				let (at, cci, message) = self.pending.pop_front().expect("front");
				self.now = self.now.max(at);
				self.cci = cci;
				self.message = message;
				Ok(true)
			}
			_ => {
				self.now = deadline;
				Ok(false)
			}
		}
	}
	fn refresh(&mut self) -> Result<(), Failure> {
		self.refreshes += 1;
		if self.sent.last().is_some_and(|control| *control as u8 == PPM_RESET) && self.reset_after.is_some_and(|after| self.refreshes >= after) {
			self.cci = Cci::RESET_COMPLETE;
		}
		Ok(())
	}
	fn cci(&mut self) -> Cci {
		Cci(self.cci)
	}
	fn message_in(&mut self, out: &mut [u8]) {
		for (at, byte) in out.iter_mut().enumerate() {
			*byte = self.message.get(at).copied().unwrap_or(0);
		}
	}
	fn now_ms(&mut self) -> u64 {
		self.now
	}
	fn pause(&mut self) {
		self.now += POLL_MS;
	}
}

const COMPLETE: u32 = Cci::COMMAND_COMPLETE;
const ACKED: u32 = Cci::ACK_COMPLETE;

fn length(bytes: usize) -> u32 {
	(bytes as u32) << 8
}

fn change(connector: u8) -> u32 {
	u32::from(connector) << 1
}

// Every command completes at once with sixteen bytes; every acknowledgement completes.
fn prompt(control: u64) -> Vec<(u64, u32, Vec<u8>)> {
	if control as u8 == ACK_CC_CI {
		return vec![(1, ACKED, Vec::new())];
	}
	vec![(5, COMPLETE | length(16), (1..=16).collect())]
}

fn v21() -> Layout {
	Layout::of(0x0210, 528).expect("a 2.1 mailbox")
}

#[test]
fn the_version_fixes_the_layout_and_a_short_range_or_a_foreign_major_is_refused() {
	let old = Layout::of(0x0120, 48).expect("1.2 fits 48 bytes");
	assert_eq!((old.message_in, old.message_out, old.message_size), (16, 32, 16));
	let new = v21();
	assert_eq!((new.message_in, new.message_out, new.message_size), (16, 272, 256));
	assert_eq!(Layout::of(0x0200, 527), Err(LayoutRefusal::Short { needed: 528, have: 527 }));
	assert_eq!(Layout::of(0x0400, 4096), Err(LayoutRefusal::Version(0x0400)));
	assert_eq!(Layout::of(0x0000, 4096), Err(LayoutRefusal::Version(0)));
	assert!(new.at_least(2, 1) && !old.at_least(2, 0));
}

#[test]
fn cci_names_its_connector_its_length_and_its_indicators() {
	let cci = Cci(COMPLETE | length(19) | change(3));
	assert_eq!((cci.connector(), cci.length(), cci.has(Cci::COMMAND_COMPLETE), cci.has(Cci::BUSY)), (Some(3), 19, true, false));
	assert_eq!(Cci(0).connector(), None);
}

#[test]
fn a_command_is_answered_read_and_acknowledged_before_the_next() {
	let mut ppm = Model::new(prompt);
	let exchange = execute(&mut ppm, &v21(), get_capability(), &[], 0, false).expect("answered");
	assert_eq!(exchange.completion, Completion::Answered((1..=16).collect()));
	assert_eq!(ppm.sent, [get_capability().control, ack(true, false).control], "the command, then its acknowledgement alone");
	assert_eq!(ppm.refreshes, 0, "nothing refreshes a mailbox a notification refreshed");
}

#[test]
fn a_busy_ppm_is_waited_on_and_a_slow_one_is_served_inside_the_bound() {
	let mut ppm = Model::new(|control| {
		if control as u8 == ACK_CC_CI {
			return vec![(1, ACKED, Vec::new())];
		}
		vec![(100, Cci::BUSY, Vec::new()), (8_000, COMPLETE, Vec::new())]
	});
	let exchange = execute(&mut ppm, &v21(), set_notification_enable(NOTIFY_ALL), &[], 0, false).expect("eight seconds is inside ten");
	assert_eq!(exchange.completion, Completion::Answered(Vec::new()));
}

#[test]
fn a_silent_ppm_fails_at_ten_seconds_and_an_unacknowledged_ack_fails_too() {
	let mut ppm = Model::new(|_| Vec::new());
	assert_eq!(execute(&mut ppm, &v21(), get_capability(), &[], 0, false), Err(Failure::Silent));
	assert_eq!(ppm.now, 1_000 + COMMAND_MS);
	let mut ppm = Model::new(|control| if control as u8 == ACK_CC_CI { Vec::new() } else { vec![(1, COMPLETE, Vec::new())] });
	assert_eq!(execute(&mut ppm, &v21(), get_capability(), &[], 0, false), Err(Failure::Silent), "a completion is not done until its acknowledgement is");
}

#[test]
fn a_length_past_message_in_or_past_the_command_is_refused_before_a_byte_is_read() {
	let old = Layout::of(0x0120, 48).expect("1.2");
	let mut ppm = Model::new(|_| vec![(1, COMPLETE | length(17), vec![0xAA; 17])]);
	assert_eq!(execute(&mut ppm, &old, get_capability(), &[], 0, false), Err(Failure::Length { cci: 17, limit: 16 }));
	let mut ppm = Model::new(|_| vec![(1, COMPLETE | length(6), vec![0xAA; 6])]);
	assert_eq!(execute(&mut ppm, &v21(), get_cable_property(1), &[], 1, false), Err(Failure::Length { cci: 6, limit: 5 }));
}

#[test]
fn a_connector_out_of_range_and_a_completion_that_is_no_answer_are_refused() {
	let mut ppm = Model::new(|_| vec![(1, COMPLETE | change(5), Vec::new())]);
	assert_eq!(execute(&mut ppm, &v21(), get_connector_status(1), &[], 2, true), Err(Failure::Connector(5)));
	let mut ppm = Model::new(|_| vec![(1, COMPLETE | Cci::ACK_COMPLETE, Vec::new())]);
	assert_eq!(execute(&mut ppm, &v21(), get_capability(), &[], 0, false), Err(Failure::Malformed));
}

#[test]
fn not_supported_and_error_complete_and_are_still_acknowledged() {
	let mut ppm = Model::new(|control| if control as u8 == ACK_CC_CI { vec![(1, ACKED, Vec::new())] } else { vec![(1, COMPLETE | Cci::NOT_SUPPORTED, Vec::new())] });
	assert_eq!(execute(&mut ppm, &v21(), get_cable_property(1), &[], 1, false).map(|exchange| exchange.completion), Ok(Completion::NotSupported));
	assert_eq!(ppm.sent.len(), 2);
	let mut ppm = Model::new(|control| if control as u8 == ACK_CC_CI { vec![(1, ACKED, Vec::new())] } else { vec![(1, COMPLETE | Cci::ERROR, Vec::new())] });
	assert_eq!(execute(&mut ppm, &v21(), set_pdr(1, true, false, true), &[], 1, false).map(|exchange| exchange.completion), Ok(Completion::Error));
}

#[test]
fn a_change_is_reported_and_acknowledged_only_beside_the_status_that_reads_it() {
	// A connector change indicated while another command ran: reported, and NOT acknowledged with it.
	let mut ppm = Model::new(|control| if control as u8 == ACK_CC_CI { vec![(1, ACKED | change(2), Vec::new())] } else { vec![(1, change(2), Vec::new()), (2, COMPLETE | change(2), Vec::new())] });
	let exchange = execute(&mut ppm, &v21(), get_capability(), &[], 2, false).expect("answered");
	assert_eq!(exchange.change, Some(2));
	assert_eq!(ppm.sent[1], ack(true, false).control, "the change waits for its status");
	// The status that reads it acknowledges both, in one.
	let mut ppm = Model::new(|control| if control as u8 == ACK_CC_CI { vec![(1, ACKED, Vec::new())] } else { vec![(1, COMPLETE | change(2) | length(19), vec![0; 19])] });
	execute(&mut ppm, &v21(), get_connector_status(2), &[], 2, true).expect("answered");
	assert_eq!(ppm.sent[1], ack(true, true).control);
}

#[test]
fn a_reset_is_polled_through_refresh_alone_and_a_silent_one_fails_at_the_bound() {
	let mut ppm = Model::new(|_| Vec::new());
	reset(&mut ppm).expect("completes after three refreshes");
	assert_eq!((ppm.sent.as_slice(), ppm.refreshes), ([ppm_reset().control].as_slice(), 3));
	let mut ppm = Model::new(|_| Vec::new());
	ppm.reset_after = None;
	assert_eq!(reset(&mut ppm), Err(Failure::Silent));
	assert!(ppm.now >= 1_000 + COMMAND_MS);
}

#[test]
fn the_commands_carry_their_fields_where_the_specification_puts_them() {
	assert_eq!(ack(true, true).control, 0x04 | 1 << 16 | 1 << 17);
	assert_eq!(set_uor(3, false, true, true).control, 0x09 | 3 << 16 | 2 << 23 | 1 << 25);
	assert_eq!(set_pdr(1, true, false, false).control, 0x0B | 1 << 16 | 1 << 23);
	let pdos = get_pdos(1, true, 4, 4, true);
	assert_eq!((pdos.control, pdos.answer), (0x10 | 1 << 16 | 1 << 23 | 4 << 24 | 3 << 32 | 1 << 34, 16));
	assert_eq!(get_alternate_modes(Recipient::Partner, 2, 1, 2).control, 0x0C | 1 << 16 | 2 << 24 | 1 << 32 | 1 << 40);
	assert_eq!(get_alternate_modes(Recipient::Connector, 2, 0, 1).control, 0x0C | 2 << 24);
	assert_eq!(set_new_cam(1, true, 0, 0x0000_0406).control, 0x0F | 1 << 16 | 1 << 23 | 0x406 << 32);
	assert_eq!(set_notification_enable(NOTIFY_ALL).control >> 16, 0xDBE7);
}

#[test]
fn the_capability_and_a_connector_capability_decode_and_a_ppm_without_connectors_is_refused() {
	let mut data = vec![0x04, 0, 0, 0, 2, 0x3C, 0, 0, 1, 0, 0, 0, 0x00, 0x03, 0x00, 0x02];
	let capability = Capability::decode(&data).expect("sixteen bytes");
	assert_eq!((capability.connectors, capability.has(Capability::ALT_MODE_OVERRIDE), capability.has(Capability::PDO_DETAILS), capability.attributes & Capability::ATTRIBUTE_PD), (2, true, true, 4));
	data[4] = 0;
	assert_eq!(Capability::decode(&data), None);
	assert_eq!(Capability::decode(&data[..7]), None, "short");
	let connector = ConnectorCapability::decode(&[0x04, 0x03 | 0x04 | 0x08]).expect("two bytes");
	assert!(connector.host() && connector.device() && connector.provider && connector.consumer && connector.swap_to_host && connector.swap_to_device);
}

// A connector status as a 2.1 PPM writes it: a PD contract at position 4, sinking, UFP partner... bits placed by hand.
fn status_21(flip: bool, ready: bool) -> Vec<u8> {
	let mut data = vec![0u8; 19];
	let set = |data: &mut [u8], at: usize, width: usize, value: u64| {
		for bit in 0..width {
			if value >> bit & 1 != 0 {
				data[(at + bit) / 8] |= 1 << ((at + bit) % 8);
			}
		}
	};
	set(&mut data, 0, 16, 1 << 14);
	set(&mut data, 16, 3, 3);
	set(&mut data, 19, 1, 1);
	set(&mut data, 21, 8, 1);
	set(&mut data, 29, 3, 1);
	set(&mut data, 32, 32, 4 << 28 | 300 << 10 | 300);
	set(&mut data, 86, 1, u64::from(flip));
	set(&mut data, 89, 1, u64::from(ready));
	set(&mut data, 90, 3, 1);
	set(&mut data, 109, 16, 600);
	set(&mut data, 125, 4, 1);
	set(&mut data, 129, 16, 3_000);
	data
}

#[test]
fn a_connector_status_decodes_per_version_with_the_readings_only_when_ready() {
	let status = ConnectorStatus::decode(&status_21(true, true), 0x0210).expect("nineteen bytes");
	assert_eq!((status.operation_mode, status.connected, status.source, status.partner, status.position(), status.partner_usb()), (3, true, false, PartnerType::Dfp, 4, true));
	assert_eq!((status.orientation, status.voltage, status.current), (Some(true), Some(15_000_000), Some(3_000_000)));
	let unready = ConnectorStatus::decode(&status_21(false, false), 0x0210).expect("nineteen bytes");
	assert_eq!((unready.orientation, unready.voltage), (Some(false), None));
	let old = ConnectorStatus::decode(&status_21(true, true)[..9], 0x0120).expect("1.x's nine bytes");
	assert_eq!((old.orientation, old.voltage), (None, None), "1.x has neither");
	assert_eq!(ConnectorStatus::decode(&status_21(true, true)[..9], 0x0210), None, "a 2.1 status needs its readings' bits");
	let mut reserved = status_21(true, true);
	reserved[3] |= 0xE0;
	assert_eq!(ConnectorStatus::decode(&reserved, 0x0210), None, "partner type 7 is reserved");
}

#[test]
fn cable_modes_objects_and_errors_decode_against_hostile_lengths() {
	let cable = CableProperty::decode(&[0x0F, 0x00, 60, 0x01 | 0x02 | 0x10 | 0x80, 0]).expect("five bytes");
	assert_eq!((cable.current, cable.vbus, cable.active, cable.plug_end, cable.pd_revision), (3000, true, true, 2, 2));
	assert_eq!(CableProperty::decode(&[0; 4]), None);
	let modes = alternate_modes(&[0x01, 0xFF, 0x05, 0x04, 0, 0, 0, 0, 0, 0, 0, 0]).expect("twelve bytes");
	assert_eq!(modes, [AlternateMode { svid: 0xFF01, vdo: 0x0405 }], "an SVID of zero ends the list");
	assert_eq!(alternate_modes(&[1, 2, 3]), None);
	assert_eq!(pdos(&[0x2C, 0x91, 0x01, 0x08, 0, 0, 0, 0]), Some(vec![0x0801_912C]));
	assert_eq!(pdos(&[0; 5]), None);
	assert_eq!(error_status(&[0x04, 0x00]), Some(4));
	assert_eq!(error_status(&[]), None);
	assert!(dp_hot_plug(attention_vdo(&[0x80, 0, 0, 0]).expect("four bytes")));
}
