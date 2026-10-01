use super::*;

const RAM: [(u64, u64); 1] = [(0, 0x8000_0000)];
const BARS: [Bar; 2] = [Bar { base: 0xFE00_0000, len: 0x1000, fixture: false }, Bar { base: 0xFE80_0000, len: 0x10_0000, fixture: true }];

fn world(fixture_exception: bool) -> World<'static> {
	World { reserved_ports: &[(0x600, 0x608), (0x20, 0x22)], granted_ports: &[(0x3F8, 0x400)], ram: &RAM, kernel_held: &[(0xFEC0_0000, 0x1000)], claimed: &[(0xFD00_0000, 0x10_0000)], bars: &BARS, fixture_exception }
}

fn memory(address: u64) -> Register {
	Register { space: Space::SystemMemory, bits: 32, address }
}

fn port(address: u64) -> Register {
	Register { space: Space::SystemIo, bits: 8, address }
}

#[test]
fn an_ordinary_port_and_memory_register_are_admitted() {
	assert_eq!(admit(&port(0x514), &world(false)), Ok(Admitted::Port));
	assert_eq!(admit(&memory(0xFEB0_0000), &world(false)), Ok(Admitted::Memory));
}

#[test]
fn a_register_in_ram_in_the_reserved_set_in_a_grant_or_inside_a_claim_is_refused() {
	assert_eq!(admit(&memory(0x1000), &world(false)), Err(Refused::Ram));
	assert_eq!(admit(&port(0x604), &world(false)), Err(Refused::Reserved), "the FADT's PM1 block");
	assert_eq!(admit(&port(0x3FC), &world(false)), Err(Refused::Granted), "COM1's driver holds it");
	assert_eq!(admit(&memory(0xFEC0_0010), &world(false)), Err(Refused::KernelHeld));
	assert_eq!(admit(&memory(0xFD00_8000), &world(false)), Err(Refused::Claimed));
	assert_eq!(admit(&memory(0xFE00_0010), &world(false)), Err(Refused::Bar), "a device's BAR");
	assert_eq!(admit(&Register { space: Space::Other(2), bits: 32, address: 0x10 }, &world(false)), Err(Refused::Space));
}

#[test]
fn the_fixture_bars_register_is_admitted_only_where_the_development_exception_is_compiled_in() {
	assert_eq!(admit(&memory(0xFE80_2000), &world(true)), Ok(Admitted::FixtureMemory));
	assert_eq!(admit(&memory(0xFE80_2000), &world(false)), Err(Refused::Bar), "a shipping build refuses it as any BAR");
	assert_eq!(admit(&memory(0xFE00_0010), &world(true)), Err(Refused::Bar), "and the exception is the fixture's BAR alone");
	assert_eq!(admit(&memory(0xFE8F_FFFE), &world(true)), Err(Refused::Bar), "a register running past the fixture's BAR");
}
