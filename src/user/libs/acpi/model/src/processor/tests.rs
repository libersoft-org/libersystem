use super::*;
use alloc::string::String;
use alloc::vec;

fn register(space: u8, bits: u8, offset: u8, address: u64) -> Value {
	let mut bytes = vec![0x82, 12, 0, space, bits, offset, 0];
	bytes.extend_from_slice(&address.to_le_bytes());
	bytes.extend_from_slice(&[0x79, 0]);
	Value::Buffer(bytes)
}

fn int(value: u64) -> Value {
	Value::Integer(value)
}

#[test]
fn a_generic_register_is_read_and_anything_else_refused() {
	assert_eq!(gas(&register(SPACE_IO, 8, 0, 0x514)), Ok(Gas { space: SPACE_IO, bits: 8, offset: 0, access: 0, address: 0x514 }));
	assert_eq!(gas(&Value::Buffer(vec![0x86, 9, 0])), Err(Refusal::Register), "a memory descriptor is no register");
	assert_eq!(gas(&int(5)), Err(Refusal::Register));
}

#[test]
fn a_cst_reads_the_halt_an_mwait_hint_and_a_port_with_its_costs() {
	let value = Value::Package(vec![
		int(3),
		Value::Package(vec![register(SPACE_FIXED_HARDWARE, 1, 1, 0), int(1), int(1), int(1000)]),
		Value::Package(vec![register(SPACE_FIXED_HARDWARE, 1, 2, 0x10), int(2), int(50), int(500)]),
		Value::Package(vec![register(SPACE_IO, 8, 0, 0x515), int(3), int(300), int(100)]),
	]);
	let states = cst(&value).unwrap();
	assert_eq!(states[0].entry, Entry::Halt);
	assert_eq!(states[1].entry, Entry::Mwait { hint: 0x10 });
	assert_eq!(states[1].residency_us, 150, "three times the exit latency where `_CST` states no residency");
	assert!(!states[1].stops_timer);
	assert!(matches!(states[2].entry, Entry::Register(Gas { space: SPACE_IO, address: 0x515, .. })));
	assert!(states[2].stops_timer && states[2].bus_master_arbitration, "a C3 by a port read: its timer may stop, and it wants arbitration off");
	assert_eq!(cst(&Value::Package(vec![int(2), Value::Package(vec![register(SPACE_IO, 8, 0, 1), int(1), int(1), int(1)])])), Err(Refusal::Shape), "a count the package does not hold");
}

#[test]
fn an_lpi_leaves_a_disabled_state_out_and_refuses_a_private_entry_method() {
	let state = |enabled: u64, entry: Value| Value::Package(vec![int(800), int(100), int(enabled), int(0), int(0), int(0), entry, register(0, 0, 0, 0), register(0, 0, 0, 0), Value::String(String::from("C2"))]);
	let value = Value::Package(vec![
		int(0),
		int(0),
		int(3),
		state(1, register(SPACE_FIXED_HARDWARE, 1, 1, 0)),
		state(0, register(SPACE_MEMORY, 32, 0, 0xFE80_3000)),
		state(1, register(SPACE_MEMORY, 32, 0, 0xFE80_3000)),
	]);
	let states = lpi(&value).unwrap();
	assert_eq!(states.len(), 2, "the disabled state is left out");
	assert_eq!(states[0].entry, Entry::Halt);
	assert_eq!(states[1].residency_us, 800);
	assert_eq!(states[1].latency_us, 100);
	assert!(states[1].stops_timer, "as deep as a C3");
	let private = Value::Package(vec![int(0), int(0), int(1), state(1, int(7))]);
	assert_eq!(lpi(&private), Err(Refusal::Register));
}

#[test]
fn pss_pct_psd_and_tss_read_as_laid_out() {
	let pss_value = Value::Package(vec![
		Value::Package(vec![int(3000), int(15000), int(10), int(10), int(0x10), int(0x10)]),
		Value::Package(vec![int(1500), int(6000), int(10), int(10), int(0x11), int(0x11)]),
	]);
	let states = pss(&pss_value).unwrap();
	assert_eq!(states.len(), 2);
	assert_eq!(states[1].control, 0x11);
	assert_eq!(pss(&Value::Package(vec![Value::Package(vec![int(1); 5])])), Err(Refusal::Shape));
	let (control, status) = control_status(&Value::Package(vec![register(SPACE_MEMORY, 32, 0, 0xFE80_2000), register(SPACE_MEMORY, 32, 0, 0xFE80_2004)])).unwrap();
	assert_eq!((control.address, status.address), (0xFE80_2000, 0xFE80_2004));
	assert_eq!(domain(&Value::Package(vec![Value::Package(vec![int(5), int(0), int(1), int(0xFC), int(4)])])), Ok(Domain { domain: 1, coordination: 0xFC, processors: 4 }));
	assert_eq!(domain(&Value::Package(vec![Value::Package(vec![int(5), int(0), int(1), int(0x10), int(4)])])), Err(Refusal::Element), "no such coordination type");
	let tss_value = Value::Package(vec![Value::Package(vec![int(100), int(1000), int(5), int(0), int(0)]), Value::Package(vec![int(50), int(500), int(5), int(0x14), int(0)])]);
	assert_eq!(tss(&tss_value).unwrap()[1].control, 0x14);
}

#[test]
fn a_cpc_needs_its_desired_register_and_reads_its_levels() {
	let mut elements = vec![int(23), int(3), int(255), int(200), int(100), int(55), int(200), register(SPACE_MEMORY, 32, 0, 0xFE80_2100), register(SPACE_MEMORY, 32, 0, 0), int(0)];
	let cppc = cpc(&Value::Package(elements.clone())).unwrap();
	assert_eq!((cppc.highest, cppc.nominal, cppc.lowest), (255, 200, 55));
	assert_eq!(cppc.desired.address, 0xFE80_2100);
	assert_eq!(cppc.minimum, None, "a register at address zero is not supported");
	assert_eq!(cppc.preference, None, "a revision-2 package of ten elements has no preference register");
	let mut revision3 = elements.clone();
	revision3.extend((10..19).map(|_| int(0)));
	revision3.push(register(SPACE_MEMORY, 8, 0, 0xFE80_2140));
	assert_eq!(cpc(&Value::Package(revision3)).unwrap().preference.map(|gas| gas.address), Some(0xFE80_2140));
	elements[7] = register(SPACE_MEMORY, 32, 0, 0);
	assert_eq!(cpc(&Value::Package(elements)), Err(Refusal::Element), "no desired-performance register");
}

#[test]
fn a_processor_is_the_core_whose_apic_id_the_madt_gives_its_uid() {
	let madt = [(0, 0), (1, 2), (2, 4), (3, 6)];
	let cores = [0, 2, 4, 6];
	assert_eq!(core_of(2, &madt, &cores), Some(2));
	assert_eq!(core_of(9, &madt, &cores), None, "a UID the MADT does not list");
	assert_eq!(core_of(1, &[(0, 0), (1, 6)], &[6, 0]), Some(0), "by the APIC ID, not by the table's position or the UID");
	assert_eq!(core_of(3, &madt, &[0, 2]), None, "a processor no running core is");
}

#[test]
fn a_uid_is_the_processor_id_or_a_number_the_uid_spells() {
	assert_eq!(uid_of(Some(3), Some("7")), Some(3), "a Processor object is keyed by its processor ID");
	assert_eq!(uid_of(None, Some("12")), Some(12));
	assert_eq!(uid_of(None, Some("0x1f")), Some(0x1f));
	assert_eq!(uid_of(None, Some("CPU0")), None);
	assert_eq!(uid_of(None, None), None);
}
