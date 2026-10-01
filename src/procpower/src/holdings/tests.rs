use super::*;
use crate::Space;

fn port(address: u64) -> Register {
	Register { space: Space::SystemIo, bits: 8, address }
}

#[test]
fn one_register_in_every_cores_table_is_taken_once_and_let_go_with_the_last() {
	let mut holdings = Holdings::new();
	let table = [port(0x514), port(0x515)];
	assert_eq!(holdings.to_take(&table).unwrap(), table, "the first table takes both");
	assert!(holdings.replace(&[], &table).unwrap().is_empty());
	for _ in 0..3 {
		assert!(holdings.to_take(&table).unwrap().is_empty(), "every other core's table takes nothing new");
		assert!(holdings.replace(&[], &table).unwrap().is_empty());
	}
	assert_eq!(holdings.count(&port(0x514)), 4);
	for _ in 0..3 {
		assert!(holdings.replace(&table, &[]).unwrap().is_empty(), "uninstalled, but another table still names them");
	}
	assert_eq!(holdings.replace(&table, &[]).unwrap(), table, "the last one lets them go");
	assert_eq!(holdings.count(&port(0x514)), 0);
}

#[test]
fn a_table_replaced_by_itself_changes_nothing_and_by_another_lets_go_only_what_it_alone_named() {
	let mut holdings = Holdings::new();
	let first = [port(0x514), port(0x515)];
	holdings.replace(&[], &first).unwrap();
	assert!(holdings.to_take(&first).unwrap().is_empty());
	assert!(holdings.replace(&first, &first).unwrap().is_empty(), "the same table again: nothing taken, nothing let go");
	assert_eq!(holdings.count(&port(0x515)), 1);
	let second = [port(0x514), port(0x516)];
	assert_eq!(holdings.to_take(&second).unwrap(), [port(0x516)]);
	assert_eq!(holdings.replace(&first, &second).unwrap(), [port(0x515)]);
	assert_eq!(holdings.count(&port(0x514)), 1, "shared by the two, counted once through the replacement");
}
