use super::*;
use alloc::vec;

const US: [u8; 6] = [0x00, 0x1b, 0xdc, 0x00, 0x00, 0x01];
const PHONE: [u8; 6] = [0x00, 0x1b, 0xdc, 0x20, 0x00, 0x02];

fn open() -> Panu {
	let (mut panu, request) = Panu::new(US, PHONE);
	assert_eq!(request, [0x01, 0x01, 0x02, 0x11, 0x16, 0x11, 0x15], "the setup request: NAP from PANU, 16-bit UUIDs");
	assert_eq!(panu.send(&[0; 20]), None, "nothing goes before the setup is answered");
	assert_eq!(panu.receive(&[0x01, 0x02, 0x00, 0x00]), Ok(Received::Setup(State::Open)));
	panu
}

fn frame(destination: [u8; 6], source: [u8; 6], ethertype: u16, payload: &[u8]) -> Vec<u8> {
	let mut out = destination.to_vec();
	out.extend_from_slice(&source);
	out.extend_from_slice(&ethertype.to_be_bytes());
	out.extend_from_slice(payload);
	out
}

#[test]
// THE SETUP: answered success, the link opens; answered anything else, it is refused and stays so.
fn the_setup_opens_or_refuses_the_link() {
	open();
	let (mut panu, _) = Panu::new(US, PHONE);
	assert_eq!(panu.receive(&[0x01, 0x02, 0x00, 0x04]), Ok(Received::Setup(State::Refused(4))));
	assert_eq!(panu.receive(&[0x01, 0x02, 0x00, 0x00]), Ok(Received::Setup(State::Refused(4))), "a later answer does not reopen it");
	assert_eq!(panu.send(&frame(PHONE, US, 0x0800, &[1])), None);
}

#[test]
// SENT IN THE MOST COMPRESSED FORM the addresses allow, and every form read back as the whole frame.
fn frames_go_compressed_and_come_back_whole() {
	let mut panu = open();
	let other = [0xff; 6];
	let to_phone = frame(PHONE, US, 0x0800, b"ip");
	assert_eq!(panu.send(&to_phone).unwrap(), [&[0x02, 0x08, 0x00][..], b"ip"].concat());
	let broadcast = frame(other, US, 0x0806, b"arp");
	let sent = panu.send(&broadcast).unwrap();
	assert_eq!(sent[0], 0x04, "destination only");
	assert_eq!(&sent[1..7], &other);
	let relayed = frame(PHONE, other, 0x86dd, b"v6");
	assert_eq!(panu.send(&relayed).unwrap()[0], 0x03, "source only");
	let general = frame(other, other, 0x0800, b"x");
	assert_eq!(panu.send(&general).unwrap()[0], 0x00);
	// AND BACK: what the phone sends, in each form, is the whole frame NetworkService reads.
	assert_eq!(panu.receive(&[&[0x02, 0x08, 0x00][..], b"ip"].concat()), Ok(Received::Frame(frame(US, PHONE, 0x0800, b"ip"))));
	let mut source_only = vec![0x03];
	source_only.extend_from_slice(&other);
	source_only.extend_from_slice(&[0x08, 0x06]);
	assert_eq!(panu.receive(&source_only), Ok(Received::Frame(frame(US, other, 0x0806, b""))));
	let mut general = vec![0x00];
	general.extend_from_slice(&other);
	general.extend_from_slice(&PHONE);
	general.extend_from_slice(&[0x08, 0x00, 7]);
	assert_eq!(panu.receive(&general), Ok(Received::Frame(frame(other, PHONE, 0x0800, &[7]))));
	assert_eq!(panu.send(&frame(PHONE, US, 0x0800, &[0; MTU + 1])), None, "past the MTU");
}

#[test]
// EXTENSION HEADERS are skipped; a packet running past itself, or of a type BNEP does not have, is refused.
fn extensions_are_skipped_and_bad_packets_refused() {
	let mut panu = open();
	// Compressed, extended: one extension of two bytes that says another follows, then one of none.
	let packet = [0x82, 0x08, 0x00, 0x80, 0x02, 0xaa, 0xbb, 0x00, 0x00, 0x42];
	assert_eq!(panu.receive(&packet), Ok(Received::Frame(frame(US, PHONE, 0x0800, &[0x42]))));
	assert_eq!(panu.receive(&[0x82, 0x08, 0x00, 0x00, 0x09, 1]), Err(Refusal::Short));
	assert_eq!(panu.receive(&[0x00, 1, 2]), Err(Refusal::Short));
	assert_eq!(panu.receive(&[0x05]), Err(Refusal::Unknown));
	assert_eq!(panu.receive(&[]), Err(Refusal::Short));
}

#[test]
// THE PEER'S CONTROL MESSAGES: filters answered unsupported, a setup asked of a PANU refused, and an unknown one
// answered not understood.
fn control_messages_are_answered() {
	let mut panu = open();
	assert_eq!(panu.receive(&[0x01, 0x03, 0x00, 0x00]), Ok(Received::Reply(vec![0x01, 0x04, 0x00, 0x01])));
	assert_eq!(panu.receive(&[0x01, 0x05, 0x00, 0x00]), Ok(Received::Reply(vec![0x01, 0x06, 0x00, 0x01])));
	assert_eq!(panu.receive(&[0x01, 0x01, 0x02, 0x11, 0x15, 0x11, 0x16]), Ok(Received::Reply(vec![0x01, 0x02, 0x00, 0x04])));
	assert_eq!(panu.receive(&[0x01, 0x09]), Ok(Received::Reply(vec![0x01, 0x00, 0x09])));
	assert_eq!(panu.receive(&[0x01, 0x00, 0x07]), Ok(Received::Nothing));
}
