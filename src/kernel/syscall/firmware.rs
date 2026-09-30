// THE ACPI SERVICE'S CALLS, every one behind a `FirmwareInterpreter` privilege - see `abi::SYS_FIRMWARE_*` - and
// DeviceManager's read of what the namespace attached to a row. The decisions are `crate::firmware`'s and
// `platform::policy`'s; these marshal.

use super::*;

impl plain_data::Sealed for abi::FirmwareMapRequest {}
// SAFETY: `repr(C)` over integers and a byte array with no implicit padding (the abi layout test says so), so every
// bit pattern is a value; all-zero is an empty request, which `firmware::map` refuses.
unsafe impl UserPlain for abi::FirmwareMapRequest {}

// The calling process, by koid - the instance a report must come from; 0 is no process.
fn caller() -> u64 {
	sched::current_thread().map_or(0, |thread| thread.process().header().koid())
}

pub(super) fn sys_firmware_table(privilege: u64, selector: u64, buf: u64, len: u64) -> i64 {
	if let Err(error) = holds_privilege(privilege, PrivilegeKind::FirmwareInterpreter) {
		return error;
	}
	if !arch::firmware::available() {
		return ERR_UNSUPPORTED;
	}
	let signature = (selector as u32).to_le_bytes();
	let instance = (selector >> 32) as usize;
	let Some(table) = arch::firmware::table(&signature, instance) else { return ERR_INVALID };
	if len >= table.len() as u64 {
		if !user_buf_writable(buf, table.len() as u64) {
			return ERR_INVALID;
		}
		if let Err(error) = copy_to_user_exact(buf, table.as_ptr(), table.len()) {
			return error;
		}
	}
	table.len() as i64
}

pub(super) fn sys_firmware_map(privilege: u64, request: u64) -> i64 {
	if let Err(error) = holds_privilege(privilege, PrivilegeKind::FirmwareInterpreter) {
		return error;
	}
	if !user_buf_ok(request, core::mem::size_of::<abi::FirmwareMapRequest>() as u64) {
		return ERR_INVALID;
	}
	let request: abi::FirmwareMapRequest = read_user(request);
	let thread = current_thread!();
	if !thread.handles().lock().reserve(1) {
		return ERR_RESOURCE_EXHAUSTED;
	}
	match crate::firmware::map(&request) {
		Ok(object) => thread.handles().lock().insert_reserved(Capability::new(object, Rights::READ | Rights::WRITE | Rights::MAP)).raw() as i64,
		Err(error) => {
			thread.handles().lock().release_reservation(1);
			error
		}
	}
}

pub(super) fn sys_firmware_mediated(privilege: u64, operation: u64, a: u64, b: u64) -> i64 {
	if let Err(error) = holds_privilege(privilege, PrivilegeKind::FirmwareInterpreter) {
		return error;
	}
	match operation {
		abi::FIRMWARE_SMI_COMMAND => match u8::try_from(a) {
			Ok(value) => arch::firmware::smi_command(value),
			Err(_) => ERR_INVALID,
		},
		abi::FIRMWARE_CMOS_READ => arch::firmware::nvram_read(a),
		abi::FIRMWARE_CMOS_WRITE => arch::firmware::nvram_write(a, b),
		abi::FIRMWARE_PM_TIMER => arch::firmware::pm_timer().map_or(ERR_UNSUPPORTED, |count| count as i64),
		abi::FIRMWARE_GLOBAL_LOCK_RELEASE => arch::firmware::global_lock_release(),
		_ => ERR_INVALID,
	}
}

pub(super) fn sys_firmware_pci(privilege: u64, address: u64, access: u64, value: u64) -> i64 {
	if let Err(error) = holds_privilege(privilege, PrivilegeKind::FirmwareInterpreter) {
		return error;
	}
	crate::firmware::pci(address, access, value)
}

pub(super) fn sys_firmware_report(privilege: u64, buf: u64, len: u64) -> i64 {
	if let Err(error) = holds_privilege(privilege, PrivilegeKind::FirmwareInterpreter) {
		return error;
	}
	if len == 0 || len > platform::report::MAX_REPORT as u64 || !user_buf_ok(buf, len) {
		return ERR_INVALID;
	}
	let mut bytes: Vec<u8> = Vec::new();
	if bytes.try_reserve_exact(len as usize).is_err() {
		return ERR_NO_MEMORY;
	}
	bytes.resize(len as usize, 0);
	if let Err(error) = copy_from_user_exact(bytes.as_mut_ptr(), buf, len as usize) {
		return error;
	}
	crate::firmware::report(caller(), &bytes)
}

pub(super) fn sys_firmware_events(privilege: u64, handle: u64) -> i64 {
	if let Err(error) = holds_privilege(privilege, PrivilegeKind::FirmwareInterpreter) {
		return error;
	}
	let channel = match current_typed::<Channel>(handle, ObjectType::Channel, Rights::SEND) {
		Ok(channel) => channel,
		Err(error) => return error,
	};
	crate::firmware::attach(caller(), channel) as i64
}

pub(super) fn sys_firmware_gpe(privilege: u64, operation: u64, gpe: u64) -> i64 {
	if let Err(error) = holds_privilege(privilege, PrivilegeKind::FirmwareInterpreter) {
		return error;
	}
	// ONLY THE RUNNING INSTANCE: a second process holding the privilege does not enable events another's methods
	// answer.
	if operation != abi::GPE_COUNT && !crate::firmware::is_instance(caller()) {
		return ERR_ACCESS_DENIED;
	}
	arch::firmware::gpe_request(operation, gpe)
}

pub(super) fn sys_device_node(index: u64, buf: u64, len: u64) -> i64 {
	let size = core::mem::size_of::<abi::FirmwareNode>() as u64;
	if len < size || !user_buf_ok(buf, size) {
		return ERR_INVALID;
	}
	let Some(node) = crate::firmware::node(index as usize) else { return ERR_INVALID };
	match write_user(buf, node) {
		Ok(()) => 0,
		Err(error) => error,
	}
}

// THE SLEEP TYPES THE FIRMWARE DESCRIBES, registered by the ACPI service - see `abi::SYS_FIRMWARE_SLEEP_TYPE`.
pub(super) fn sys_firmware_sleep_type(privilege: u64, state: u64, typ_a: u64, typ_b: u64) -> i64 {
	if let Err(error) = holds_privilege(privilege, PrivilegeKind::FirmwareInterpreter) {
		return error;
	}
	crate::sleep::register(state, typ_a, typ_b)
}
