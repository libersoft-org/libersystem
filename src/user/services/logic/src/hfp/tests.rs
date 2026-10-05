use super::*;
use alloc::vec;

fn sent(out: &[Out]) -> Vec<alloc::string::String> {
	out.iter().filter_map(|out| if let Out::Send(bytes) = out { Some(alloc::string::String::from_utf8_lossy(bytes).trim().into()) } else { None }).collect()
}

fn events(out: &[Out]) -> Vec<Event> {
	out.iter().filter_map(|out| if let Out::Event(event) = out { Some(event.clone()) } else { None }).collect()
}

// THE SERVICE LEVEL CONNECTION a hands-free device with codec negotiation and HF indicators makes.
fn connected() -> Gateway {
	let mut gateway = Gateway::new(false);
	let features = HF_CODEC_NEGOTIATION | HF_INDICATORS;
	let out = gateway.receive(alloc::format!("AT+BRSF={features}\r").as_bytes());
	assert_eq!(sent(&out), [alloc::format!("+BRSF: {AG_FEATURES}"), "OK".into()]);
	assert_eq!(sent(&gateway.receive(b"AT+BAC=1,2\r")), ["OK"]);
	let list = gateway.receive(b"AT+CIND=?\r");
	assert!(sent(&list)[0].starts_with("+CIND: (\"service\",(0,1)),(\"call\",(0,1))"));
	assert_eq!(sent(&gateway.receive(b"AT+CIND?\r")), ["+CIND: 0,0,0,0,0,0,5", "OK"], "no call and no network service");
	let up = gateway.receive(b"AT+CMER=3,0,0,1\r");
	assert_eq!(sent(&up), ["OK", "+BCS: 2"], "mSBC chosen at once where both have it");
	assert_eq!(events(&up), [Event::Connected]);
	assert!(gateway.connected());
	gateway
}

#[test]
// THE SLC, AND THE CODEC NEGOTIATED: mSBC where both list it, confirmed by the headset.
fn the_service_level_connection_and_the_codec() {
	let mut gateway = connected();
	let confirmed = gateway.receive(b"AT+BCS=2\r");
	assert_eq!(sent(&confirmed), ["OK"]);
	assert_eq!(events(&confirmed), [Event::Codec(CODEC_MSBC)]);
	assert_eq!(gateway.codec(), CODEC_MSBC);
	assert_eq!(sent(&gateway.receive(b"AT+BCS=3\r")), ["ERROR"], "a codec HFP does not define");
	// A HEADSET WITH CVSD ALONE is chosen CVSD.
	let mut plain = Gateway::new(false);
	plain.receive(alloc::format!("AT+BRSF={HF_CODEC_NEGOTIATION}\r").as_bytes());
	plain.receive(b"AT+BAC=1\r");
	plain.receive(b"AT+CIND=?\r");
	plain.receive(b"AT+CIND?\r");
	assert_eq!(sent(&plain.receive(b"AT+CMER=3,0,0,1\r")), ["OK", "+BCS: 1"]);
}

#[test]
// NO CALL DECLARED: every call command is ERROR - there is no telephone behind this gateway.
fn with_no_call_declared_every_call_command_is_error() {
	let mut gateway = connected();
	for command in [&b"ATA\r"[..], b"AT+CHUP\r", b"AT+BLDN\r"] {
		let out = gateway.receive(command);
		assert_eq!(sent(&out), ["ERROR"]);
		assert!(events(&out).is_empty());
	}
}

#[test]
// A DECLARED CALL: the indicators follow it, RING while it rings, and the headset's answer and hang-up are relayed.
fn a_declared_call_is_reported_and_its_commands_relayed() {
	let mut gateway = connected();
	let ringing = gateway.set_call(Call::Incoming);
	assert_eq!(sent(&ringing), ["+CIEV: 1,1", "+CIEV: 3,1", "RING"]);
	let answered = gateway.receive(b"ATA\r");
	assert_eq!(sent(&answered), ["OK"]);
	assert_eq!(events(&answered), [Event::Command(Command::Answer)]);
	assert_eq!(sent(&gateway.set_call(Call::Active)), ["+CIEV: 2,1", "+CIEV: 3,0"]);
	assert_eq!(events(&gateway.receive(b"AT+CHUP\r")), [Event::Command(Command::HangUp)]);
	assert_eq!(sent(&gateway.set_call(Call::None)), ["+CIEV: 1,0", "+CIEV: 2,0"]);
	// AN INCOMING CALL HUNG UP is a rejection.
	gateway.set_call(Call::Incoming);
	assert_eq!(events(&gateway.receive(b"AT+CHUP\r")), [Event::Command(Command::Reject)]);
}

#[test]
// GAINS AND THE BATTERY: the headset's gains and its HF battery indicator come back as events, out of range refused.
fn gains_and_the_battery_come_back() {
	let mut gateway = connected();
	assert_eq!(events(&gateway.receive(b"AT+VGS=12\r")), [Event::SpeakerGain(12)]);
	assert_eq!(events(&gateway.receive(b"AT+VGM=7\r")), [Event::MicrophoneGain(7)]);
	assert_eq!(sent(&gateway.receive(b"AT+VGS=16\r")), ["ERROR"]);
	assert_eq!(sent(&gateway.receive(b"AT+BIND=?\r")), ["+BIND: (2)", "OK"]);
	assert_eq!(events(&gateway.receive(b"AT+BIEV=2,80\r")), [Event::Battery(80)]);
	assert_eq!(sent(&gateway.receive(b"AT+BIEV=2,101\r")), ["ERROR"]);
	assert_eq!(sent(&gateway.set_speaker_gain(9)), ["+VGS: 9"]);
	for level in 0..=100u8 {
		assert!(level_of(gain_of(level)).abs_diff(level) <= 4);
	}
	for gain in 0..=15u8 {
		assert_eq!(gain_of(level_of(gain)), gain);
	}
}

#[test]
// LINES ARRIVE IN PIECES, AND A LINE THAT NEVER ENDS IS NOT READ.
fn lines_arrive_in_pieces() {
	let mut gateway = Gateway::new(false);
	assert!(gateway.receive(b"AT+BR").is_empty());
	assert_eq!(sent(&gateway.receive(b"SF=0\r")).len(), 2);
	assert_eq!(sent(&gateway.receive(&[b'A'; 600])), ["ERROR"]);
	assert_eq!(sent(&gateway.receive(b"AT+UNKNOWN\r")), ["ERROR"]);
}

#[test]
// THE HEADSET PROFILE'S ONE BUTTON: audio with no call, an answer while ringing.
fn the_headset_profile_button() {
	let mut gateway = Gateway::new(true);
	assert_eq!(events(&gateway.receive(b"AT+CKPD=200\r")), [Event::AudioRequested]);
	gateway.set_call(Call::Incoming);
	assert_eq!(events(&gateway.receive(b"AT+CKPD=200\r")), [Event::Command(Command::Answer)]);
}

#[test]
// mSBC ON THE LINK: the H2 header's four sequence words, a 60-byte packet, and anything else refused.
fn msbc_packets_carry_the_h2_header() {
	let mut frame = vec![crate::sbc::MSBC_SYNC, 0, 0];
	frame.resize(57, 0x55);
	let mut packet = [0u8; MSBC_PACKET];
	for sequence in 0..4u8 {
		h2_frame(sequence, &frame, &mut packet);
		assert_eq!(packet[1], H2_SEQUENCE[usize::from(sequence)]);
		assert_eq!(h2_payload(&packet), Some(&frame[..]));
	}
	packet[1] = 0x18;
	assert_eq!(h2_payload(&packet), None);
}

#[test]
// THE HEADSET ASKS FOR AUDIO: refused with no session holding the link, taken with one.
fn audio_is_asked_for_only_while_a_session_holds_it() {
	let mut gateway = connected();
	assert_eq!(sent(&gateway.receive(b"AT+BCC\r")), ["ERROR"]);
	gateway.set_audio_allowed(true);
	let asked = gateway.receive(b"AT+BCC\r");
	assert_eq!(sent(&asked), ["OK"]);
	assert_eq!(events(&asked), [Event::AudioRequested]);
	assert!(gateway.negotiates());
	assert!(!Gateway::new(true).negotiates(), "the headset profile negotiates nothing");
}
