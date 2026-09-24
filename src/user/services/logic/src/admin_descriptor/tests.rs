use super::*;
use alloc::string::ToString;

#[test]
fn a_label_cannot_move_recolour_or_reorder_the_screen() {
	assert_eq!(escape_label("plain words"), "plain words");
	assert_eq!(escape_label("\u{1b}[2J wipe"), "\\u{1b}[2J wipe");
	assert_eq!(escape_label("line\nbreak\r"), "line\\u{a}break\\u{d}");
	assert_eq!(escape_label("\u{202e}txt.exe"), "\\u{202e}txt.exe", "a right-to-left override");
	assert_eq!(escape_label("\\u{41}"), "\\\\u{41}", "an escape cannot be forged");
	let long = "é".repeat(100);
	let shown = escape_label(&long);
	assert!(shown.len() <= MAX_LABEL && shown.chars().all(|character| character == 'é'), "cut at a character within 128 bytes");
}

#[test]
fn the_prompt_is_the_services_template_with_the_label_last_and_marked() {
	let descriptor = Descriptor { version: VERSION, action: Action::ProbeWrite, executor: "org.libersystem.admin-probe".to_string(), executor_epoch: 3, target: "probe-target".to_string(), target_generation: 2, parameters: alloc::vec![0xab], payload_length: 16, payload_digest: alloc::vec![0x11; 32] };
	let lines = prompt(&descriptor, "adminreq", "Approve\nEverything");
	assert_eq!(lines[0], "ADMINISTRATIVE CONFIRMATION");
	assert!(lines.iter().any(|line| line == "Requested by:  adminreq"));
	assert!(lines.iter().any(|line| line.contains("probe-target (generation 2)")));
	assert!(lines.iter().any(|line| line.contains(&"11".repeat(32))));
	let label = lines.iter().position(|line| line.starts_with("Label:")).unwrap();
	assert!(lines[label].contains("Approve\\u{a}Everything") && lines[label].contains("not verified"));
	assert!(label > lines.iter().position(|line| line.starts_with("Payload:")).unwrap(), "below the canonical operation");
}

#[test]
fn requests_and_preparations_are_checked_against_their_bounds() {
	let asked = Asked { action: Action::ProbeWrite, target: "probe-target".to_string(), parameters: alloc::vec![], payload_length: 4, label: String::new() };
	assert_eq!(check_asked(&asked), Ok(()));
	assert_eq!(check_asked(&Asked { target: String::new(), ..asked.clone() }), Err(Refusal::Bounds));
	assert_eq!(check_asked(&Asked { parameters: alloc::vec![0; 257], ..asked.clone() }), Err(Refusal::Bounds));
	let descriptor = Descriptor { version: VERSION, action: Action::ProbeWrite, executor: "x".to_string(), executor_epoch: 1, target: "probe-target".to_string(), target_generation: 1, parameters: alloc::vec![], payload_length: 4, payload_digest: alloc::vec![0; 32] };
	assert_eq!(check_prepared(&asked, &descriptor, 4096), Ok(()));
	assert_eq!(check_prepared(&asked, &descriptor, 4097), Err(Refusal::Bounds));
	assert_eq!(check_prepared(&asked, &Descriptor { executor: String::new(), ..descriptor.clone() }, 10), Err(Refusal::Malformed));
	assert_eq!(check_prepared(&asked, &Descriptor { payload_length: 5, ..descriptor }, 10), Err(Refusal::Mismatch));
}
