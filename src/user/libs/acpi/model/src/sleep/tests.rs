use super::*;
use alloc::string::ToString;
use alloc::vec;

// A SCRIPTED NAMESPACE: each wake node's `_PRW`, the events the kernel refuses, and every call recorded in order.
#[derive(Default)]
struct Script {
	prw: Vec<(&'static str, Prw<&'static str>)>,
	refused_events: Vec<u16>,
	calls: Vec<String>,
}

impl Platform for Script {
	type Resource = &'static str;

	fn prw(&mut self, identity: &str) -> Prw<&'static str> {
		self.calls.push(format!("_PRW {identity}"));
		self.prw.iter().find(|(name, _)| *name == identity).map(|(_, prw)| prw.clone()).unwrap_or(Prw::Absent)
	}

	fn hold_power(&mut self, holder: &str, resources: &[&'static str]) {
		self.calls.push(format!("hold {holder}: {}", resources.join(" ")));
	}

	fn release_power(&mut self, holder: &str) {
		self.calls.push(format!("release {holder}"));
	}

	fn enable_device_wake(&mut self, identity: &str, target: u64) {
		self.calls.push(format!("_DSW {identity} {target}"));
	}

	fn set_wake_event(&mut self, number: u16) -> bool {
		self.calls.push(format!("set {number:#04x}"));
		!self.refused_events.contains(&number)
	}

	fn clear_wake_event(&mut self, number: u16) {
		self.calls.push(format!("clear {number:#04x}"));
	}

	fn root_method(&mut self, path: &str, argument: u64) {
		self.calls.push(format!("{path} {argument}"));
	}

	fn say(&mut self, _line: &str) {}
}

fn names(list: &[&str]) -> Vec<String> {
	list.iter().map(|name| name.to_string()).collect()
}

fn calls(list: &[&str]) -> Vec<String> {
	names(list)
}

fn machine() -> Script {
	Script {
		prw: vec![
			("acpi:\\_SB_.LID0", Prw::Event { number: 0x10, deepest: 4, resources: vec!["\\_SB_.PWAK"] }),
			("acpi:\\_SB_.PCI0.XHC_", Prw::Event { number: 0x0d, deepest: 3, resources: Vec::new() }),
			("acpi:\\_SB_.SLPB", Prw::Event { number: 0x11, deepest: 3, resources: vec!["\\_SB_.PWAK", "\\_SB_.PAUX"] }),
		],
		..Script::default()
	}
}

// S3, IN ACPI'S ORDER: each wake node's `_PRW`, its power on, `_DSW`, its event set - then `_PTS(3)` and `_SST(3)`.
// The wake: `_SST(2)`, `_WAK(3)`, every event cleared, every holder let go, `_SST(1)`.
#[test]
fn an_s3_prepares_each_wake_node_then_pts_and_sst_and_the_wake_undoes_it_in_order() {
	let mut script = machine();
	let mut sleeper = Sleeper::default();
	sleeper.prepare(&mut script, State::Ram, &names(&["acpi:\\_SB_.LID0", "acpi:\\_SB_.PCI0.XHC_"]));
	assert_eq!(
		script.calls,
		calls(&[
			"_PRW acpi:\\_SB_.LID0",
			"hold wake acpi:\\_SB_.LID0: \\_SB_.PWAK",
			"_DSW acpi:\\_SB_.LID0 3",
			"set 0x10",
			"_PRW acpi:\\_SB_.PCI0.XHC_",
			"_DSW acpi:\\_SB_.PCI0.XHC_ 3",
			"set 0x0d",
			"\\_PTS 3",
			"\\_SI_._SST 3",
		])
	);
	assert_eq!(sleeper.armed(), &[0x10, 0x0d]);
	script.calls.clear();
	sleeper.wake(&mut script, State::Ram);
	assert_eq!(script.calls, calls(&["\\_SI_._SST 2", "\\_WAK 3", "clear 0x10", "clear 0x0d", "release wake acpi:\\_SB_.LID0", "\\_SI_._SST 1"]));
	assert!(sleeper.armed().is_empty());
}

// SUSPEND TO IDLE IS NO FIRMWARE TRANSITION: no `_PTS` and no `_WAK`, `_DSW` told S0; S4 runs both with 4 and `_SST(4)`.
#[test]
fn suspend_to_idle_runs_no_pts_or_wak_and_s4_runs_both_with_the_hibernating_sst() {
	let mut script = machine();
	let mut sleeper = Sleeper::default();
	sleeper.prepare(&mut script, State::Idle, &names(&["acpi:\\_SB_.PCI0.XHC_"]));
	assert_eq!(script.calls, calls(&["_PRW acpi:\\_SB_.PCI0.XHC_", "_DSW acpi:\\_SB_.PCI0.XHC_ 0", "set 0x0d", "\\_SI_._SST 3"]));
	script.calls.clear();
	sleeper.wake(&mut script, State::Idle);
	assert_eq!(script.calls, calls(&["\\_SI_._SST 2", "clear 0x0d", "\\_SI_._SST 1"]));

	let mut script = machine();
	let mut sleeper = Sleeper::default();
	sleeper.prepare(&mut script, State::Disk, &names(&["acpi:\\_SB_.LID0"]));
	assert_eq!(script.calls[script.calls.len() - 2..], calls(&["\\_PTS 4", "\\_SI_._SST 4"]));
	assert!(script.calls.contains(&"_DSW acpi:\\_SB_.LID0 4".to_string()));
	script.calls.clear();
	sleeper.wake(&mut script, State::Disk);
	assert_eq!(script.calls[..2], calls(&["\\_SI_._SST 2", "\\_WAK 4"]));
}

// A NODE THAT CANNOT WAKE FROM THE STATE ENTERED is passed over whole: no power, no `_DSW`, no event - nor one absent,
// one with no `_PRW`, or one whose event is on a GPE block device.
#[test]
fn a_node_that_cannot_wake_from_the_state_is_passed_over_whole() {
	let mut script = machine();
	script.prw.push(("acpi:\\_SB_.NOWK", Prw::None));
	script.prw.push(("acpi:\\_SB_.BLKD", Prw::BlockDevice));
	let mut sleeper = Sleeper::default();
	sleeper.prepare(&mut script, State::Disk, &names(&["acpi:\\_SB_.SLPB", "acpi:\\_SB_.NOWK", "acpi:\\_SB_.BLKD", "acpi:\\_SB_.GONE"]));
	assert_eq!(script.calls, calls(&["_PRW acpi:\\_SB_.SLPB", "_PRW acpi:\\_SB_.NOWK", "_PRW acpi:\\_SB_.BLKD", "_PRW acpi:\\_SB_.GONE", "\\_PTS 4", "\\_SI_._SST 4"]), "the sleep button wakes from S3 at the deepest, not from S4");
	assert!(sleeper.armed().is_empty());
	script.calls.clear();
	sleeper.wake(&mut script, State::Disk);
	assert_eq!(script.calls, calls(&["\\_SI_._SST 2", "\\_WAK 4", "\\_SI_._SST 1"]), "nothing to clear or let go");
}

// AN EVENT THE KERNEL REFUSED is not cleared at the wake - it was never set - while the power held for it is let go.
#[test]
fn an_event_the_kernel_refused_is_not_cleared_and_its_power_is_still_let_go() {
	let mut script = machine();
	script.refused_events.push(0x10);
	let mut sleeper = Sleeper::default();
	sleeper.prepare(&mut script, State::Ram, &names(&["acpi:\\_SB_.LID0"]));
	assert!(sleeper.armed().is_empty());
	script.calls.clear();
	sleeper.wake(&mut script, State::Ram);
	assert_eq!(script.calls, calls(&["\\_SI_._SST 2", "\\_WAK 3", "release wake acpi:\\_SB_.LID0", "\\_SI_._SST 1"]));
}

// A PREPARE WITH NO WAKE BETWEEN - a sleep unwound after its platform step - lets go of what the last one took first.
#[test]
fn a_second_prepare_lets_go_of_what_the_first_took_before_it_takes_anything() {
	let mut script = machine();
	let mut sleeper = Sleeper::default();
	sleeper.prepare(&mut script, State::Ram, &names(&["acpi:\\_SB_.SLPB"]));
	script.calls.clear();
	sleeper.prepare(&mut script, State::Ram, &names(&["acpi:\\_SB_.LID0"]));
	assert_eq!(script.calls[0], "release wake acpi:\\_SB_.SLPB");
	assert_eq!(sleeper.armed(), &[0x10], "only this prepare's events");
}

// THE SLEEP TYPES: SLP_TYPa and SLP_TYPb, three bits each, the second the first where it is missing or not an integer.
#[test]
fn a_sleep_type_is_read_from_its_package() {
	assert_eq!(sleep_type(&[Some(5), Some(5), Some(0), Some(0)]), Some((5, 5)));
	assert_eq!(sleep_type(&[Some(1), Some(3)]), Some((1, 3)));
	assert_eq!(sleep_type(&[Some(6)]), Some((6, 6)), "one integer: both halves");
	assert_eq!(sleep_type(&[Some(2), None]), Some((2, 2)), "a second that is not an integer: the first again");
	assert_eq!(sleep_type(&[Some(0x0D), Some(0x1F)]), Some((5, 7)), "three bits each");
	assert_eq!(sleep_type(&[None, Some(5)]), None, "a first that is not an integer names no sleep type");
	assert_eq!(sleep_type(&[]), None);
}
