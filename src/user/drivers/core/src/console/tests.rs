// THE HOSTILE HALF OF A CONTROL QUEUE, which is the half a booted device never shows you: every one
// of these messages is bytes the DEVICE chose, and a driver that trusted them would be a driver a
// malicious or broken device programs.
use super::{Action, Control, Ports, Refusal, event};

fn message(id: u32, event: u16, value: u16) -> [u8; 8] {
	let mut bytes = [0u8; 8];
	Control { id, event, value }.encode(&mut bytes).expect("a control message is eight bytes");
	bytes
}

#[test]
// THE QUEUE INDEX RULE, WHICH IS THE SPECIFICATION'S. Port 0 keeps 0 and 1 because it is the console
// that exists before any control queue does; the control pair takes 2 and 3; every other port
// follows at `2n + 2`. A driver that numbered them sequentially would set up queues the device does
// not have and find a port that answers nothing.
fn a_port_owns_the_queue_pair_the_specification_gives_it() {
	assert_eq!(Ports::queue_pair(0), Some((0, 1)), "the console port precedes the control queue");
	assert_eq!(Ports::queue_pair(1), Some((4, 5)), "and the first generic port follows it");
	assert_eq!(Ports::queue_pair(2), Some((6, 7)));
	assert_eq!(Ports::queue_pair(super::MAX_PORTS), None, "a port past the cap owns no queues at all");
	// AND THE CONTROL PAIR IS NOBODY'S PORT, which is the mistake the rule exists to prevent.
	for port in 0..super::MAX_PORTS {
		let (receive, transmit) = Ports::queue_pair(port).expect("a port inside the cap");
		assert!(receive != super::CONTROL_RECEIVE_QUEUE && transmit != super::CONTROL_RECEIVE_QUEUE, "port {port} claims the control receive queue");
		assert!(receive != super::CONTROL_TRANSMIT_QUEUE && transmit != super::CONTROL_TRANSMIT_QUEUE, "port {port} claims the control transmit queue");
	}
}

#[test]
// THE ORDINARY LIFE OF A PORT: added, named, opened, closed, removed - and each step is an action the
// driver owes something for, rather than a state it discovers later.
fn a_port_is_added_named_opened_and_removed_in_that_order() {
	let mut ports = Ports::new(4);
	assert_eq!(ports.apply(&message(1, event::PORT_ADD, 0)), Action::Add(1));
	let mut named = alloc::vec::Vec::from(message(1, event::PORT_NAME, 0));
	named.extend_from_slice(b"org.libersystem.dev\0");
	assert_eq!(ports.apply(&named), Action::Named(1));
	assert_eq!(ports.get(1).expect("the port").name(), b"org.libersystem.dev", "the NUL is not part of the name");
	assert_eq!(ports.apply(&message(1, event::PORT_OPEN, 1)), Action::Open(1));
	assert_eq!(ports.open_count(), 1, "one open port is one byte stream to publish");
	assert_eq!(ports.apply(&message(1, event::PORT_OPEN, 0)), Action::Close(1));
	assert_eq!(ports.open_count(), 0);
	assert_eq!(ports.apply(&message(1, event::PORT_REMOVE, 0)), Action::Remove(1));
	assert_eq!(ports.apply(&message(1, event::PORT_OPEN, 1)), Action::Refused(Refusal::UnknownPort), "a removed port is not an open one");
}

#[test]
// A REPEAT IS NOT A SECOND EVENT. A host that sends `PORT_OPEN` twice has not opened two streams, and
// a driver that published one per message would hand the catalogue two providers for one port.
fn the_same_message_twice_is_one_transition() {
	let mut ports = Ports::new(4);
	assert_eq!(ports.apply(&message(2, event::PORT_ADD, 0)), Action::Add(2));
	assert_eq!(ports.apply(&message(2, event::PORT_ADD, 0)), Action::Settled, "a port added twice is added once");
	assert_eq!(ports.apply(&message(2, event::PORT_OPEN, 1)), Action::Open(2));
	assert_eq!(ports.apply(&message(2, event::PORT_OPEN, 1)), Action::Settled);
	assert_eq!(ports.open_count(), 1);
}

#[test]
// A DEVICE ANNOUNCING FOUR THOUSAND PORTS GETS SIXTEEN. `max_nr_ports` is a number the device writes
// into its own configuration, so a driver that sized anything from it would let the device choose how
// much memory it allocates - and the refusal is BY NAME, because a port that is silently ignored is
// indistinguishable from one that never arrived.
fn a_device_cannot_choose_how_many_ports_this_driver_tracks() {
	let mut ports = Ports::new(4000);
	assert_eq!(ports.announced(), super::MAX_PORTS, "the announcement is clamped to what this driver will track");
	assert_eq!(ports.apply(&message(super::MAX_PORTS, event::PORT_ADD, 0)), Action::Refused(Refusal::PortOutOfRange));
	assert_eq!(ports.apply(&message(4000, event::PORT_ADD, 0)), Action::Refused(Refusal::PortOutOfRange));
	assert_eq!(ports.apply(&message(super::MAX_PORTS - 1, event::PORT_ADD, 0)), Action::Add(super::MAX_PORTS - 1), "and the last one inside the cap is ordinary");
}

#[test]
// A MESSAGE SHORTER THAN ITS OWN HEADER IS NOT A MESSAGE. The device chooses the used length of the
// buffer it returns, so a driver reading eight bytes out of a four-byte answer reads whatever the
// slot held before it - which on a recycled DMA slot is the previous message.
fn a_truncated_message_is_refused_rather_than_read_past() {
	let mut ports = Ports::new(4);
	assert_eq!(ports.apply(&[]), Action::Refused(Refusal::Truncated));
	assert_eq!(ports.apply(&[0, 0, 0, 0]), Action::Refused(Refusal::Truncated));
	assert_eq!(ports.apply(&message(0, 4242, 0)), Action::Refused(Refusal::UnknownEvent), "and an event the specification does not define is refused rather than guessed");
}

#[test]
// A NAME LONGER THAN THE BUFFER IS TRUNCATED AND KEPT, not refused: a device that names a port with
// two hundred bytes has given a long name rather than a hostile one. What must not happen is the
// copy past the end, which is what the cap is for.
fn a_long_name_is_truncated_to_the_cap() {
	let mut ports = Ports::new(2);
	assert_eq!(ports.apply(&message(1, event::PORT_ADD, 0)), Action::Add(1));
	let mut named = alloc::vec::Vec::from(message(1, event::PORT_NAME, 0));
	named.extend_from_slice(&[b'x'; 500]);
	assert_eq!(ports.apply(&named), Action::Named(1));
	assert_eq!(ports.get(1).expect("the port").name().len(), super::MAX_NAME);
}

#[test]
// THE CONSOLE PORT IS MARKED AND NOT ASSUMED. Which port is the console decides where an emergency
// write goes, and on a multiport device it is whichever one the host says - not always port zero.
fn the_console_port_is_the_one_the_host_names() {
	let mut ports = Ports::new(4);
	assert_eq!(ports.apply(&message(2, event::PORT_ADD, 0)), Action::Add(2));
	assert_eq!(ports.apply(&message(2, event::CONSOLE_PORT, 0)), Action::Console(2));
	assert!(ports.get(2).expect("the port").console);
	assert!(!ports.get(0).expect("port zero").console, "port zero is not the console by default");
}
