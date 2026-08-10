use super::Process;
use crate::object::address_space::AddressSpace;
use crate::{elf, sched};

crate::tagged_test!(dynamic_symbol_names_accept_rust_mangling_with_a_bound, [Dynamic, Memory, Process], id = "kernel.object.process.dynamic_symbol_names_accept_rust_mangling_with_a_bound", covers = ["kernel"]);
fn dynamic_symbol_names_accept_rust_mangling_with_a_bound() {
	let address_space = AddressSpace::create().expect("address space");
	let process = Process::new(address_space, sched::root_domain());
	let accepted = alloc::string::String::from_utf8(alloc::vec![b'x'; elf::MAX_DYNAMIC_SYMBOL_NAME]).expect("ASCII symbol");
	assert!(process.register_dynamic_symbols(&[(accepted, 0x2000_1000)]), "the bounded Rust symbol is accepted");
	let rejected = alloc::string::String::from_utf8(alloc::vec![b'y'; elf::MAX_DYNAMIC_SYMBOL_NAME + 1]).expect("ASCII symbol");
	assert!(!process.register_dynamic_symbols(&[(rejected, 0x2000_2000)]), "an overlong symbol is rejected");
}
