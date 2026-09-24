use super::*;

fn id(body: &str) -> Vec<u8> {
	let mut bytes = ((body.len() + 2) as u16).to_be_bytes().to_vec();
	bytes.extend_from_slice(body.as_bytes());
	bytes
}

#[test]
fn postscript_is_recognized_exactly_and_case_insensitively() {
	let parsed = parse(&id("MFG:Fixture;MDL:Sink 1;CMD:PCL,postscript3,ESCP;")).unwrap();
	assert!(parsed.postscript && parsed.command_set);
	assert_eq!((parsed.manufacturer.as_deref(), parsed.model.as_deref()), (Some("Fixture"), Some("Sink 1")));
	assert_eq!(parsed.other, alloc::vec![String::from("PCL"), String::from("ESCP")], "other tokens are metadata, not languages");
	assert!(parse(&id("COMMAND SET:POSTSCRIPT;")).unwrap().postscript, "the long key names the same set");
	// Near misses are not PostScript.
	for near in ["POSTSCRIPT4", "PS", "POSTSCRIPT LEVEL 2", "XPOSTSCRIPT"] {
		let parsed = parse(&id(&alloc::format!("CMD:{near};"))).unwrap();
		assert!(!parsed.postscript, "{near} is not an exact token");
	}
}

#[test]
fn missing_malformed_or_ambiguous_evidence_is_no_evidence() {
	let missing = parse(&id("MFG:Fixture;MDL:Sink;")).unwrap();
	assert!(!missing.command_set && !missing.postscript, "no command set: no language");
	assert_eq!(parse(&id("MFG:Fixture;garbage;")), Err(Refusal::Malformed), "a pair with no key");
	assert_eq!(parse(&id(":value;")), Err(Refusal::Malformed));
	assert_eq!(parse(&id("CMD:POSTSCRIPT;COMMAND SET:PCL;")), Err(Refusal::Ambiguous), "two command sets that disagree");
	assert!(parse(&id("CMD:POSTSCRIPT;COMMAND SET:postscript;")).unwrap().postscript, "two that agree are one answer");
	assert_eq!(parse(&id("MDL:A;MDL:B;")), Err(Refusal::Ambiguous), "a key twice");
	let mut control = id("CMD:POSTSCRIPT;");
	control[4] = 0x01;
	assert_eq!(parse(&control), Err(Refusal::Malformed), "a control byte");
}

#[test]
fn the_length_is_checked_before_anything_is_read() {
	let mut short = id("CMD:POSTSCRIPT;");
	short[1] += 1;
	assert_eq!(parse(&short), Err(Refusal::Length), "a length past what arrived");
	let mut long = id("CMD:POSTSCRIPT;");
	long.push(b';');
	assert_eq!(parse(&long), Err(Refusal::Length), "bytes past the length");
	assert_eq!(parse(&[0]), Err(Refusal::Length));
	// Exactly 4096 is read; one more is refused before a byte of it is looked at.
	let body = alloc::format!("CMD:POSTSCRIPT;MDL:{};", "x".repeat(MAX_DEVICE_ID - 2 - "CMD:POSTSCRIPT;MDL:;".len()));
	assert!(parse(&id(&body)).unwrap().postscript);
	let mut over = id(&body);
	over.push(b'x');
	over[0..2].copy_from_slice(&((MAX_DEVICE_ID + 1) as u16).to_be_bytes());
	assert_eq!(parse(&over), Err(Refusal::Length));
	// Unrecognized tokens are bounded: never more than sixteen kept, none longer than 32.
	let many: Vec<String> = (0..40).map(|n| alloc::format!("LANG{n}{}", "y".repeat(40))).collect();
	let parsed = parse(&id(&alloc::format!("CMD:{};", many.join(",")))).unwrap();
	assert_eq!(parsed.other.len(), MAX_TOKENS);
	assert!(parsed.other.iter().all(|token| token.len() <= MAX_TOKEN));
}

#[test]
fn the_port_bits_are_three_observations_and_prove_nothing_more() {
	let benign = port(0x18, None, None);
	assert_eq!((benign.paper_empty, benign.selected, benign.error), (Observation::No, Observation::Yes, Observation::No), "the benign reading");
	assert_eq!(port(0x30, None, None).paper_empty, Observation::Yes);
	assert_eq!(port(0x10, None, None).error, Observation::Yes, "NOT-error clear is an error");
	assert_eq!(port(0x08, None, None).selected, Observation::No);
	// The bits a class byte does not have are ignored rather than read as anything.
	assert_eq!(port(0x18 | 0xc7, None, None), benign);
	assert_eq!(Port::UNAVAILABLE.selected, Observation::Unavailable, "an unread port is unavailable, not benign");
}

#[test]
fn cover_and_jam_are_reported_only_on_evidence() {
	let silent = port(0x18, None, None);
	assert_eq!((silent.cover_open, silent.jam), (Observation::Unsupported, Observation::Unsupported), "no evidence is not a reassuring no");
	let identified = port(0x18, Some(true), Some(false));
	assert_eq!((identified.cover_open, identified.jam), (Observation::Yes, Observation::No));
	assert_eq!((Port::UNAVAILABLE.cover_open, Port::UNAVAILABLE.jam), (Observation::Unavailable, Observation::Unavailable));
}
