// The two UEFI variables that say whether firmware Secure Boot is enforcing.
//
// ONLY `GetVariable`, AND ONLY FOR THESE TWO. Runtime Services is a large table and this crate types
// one entry of it: a loader that could set variables could enrol its own key, and a loader that
// could call anything in the table would be carrying an attack surface for a fact it could get in
// one call. `SetVariable` is deliberately not here.
//
// WHAT THE TWO MEAN, because "secure boot is on" is three states rather than two:
//   - `SetupMode = 1` is firmware with no platform key: it accepts enrolments, so it is NOT
//     enforcing whatever `SecureBoot` says.
//   - `SecureBoot = 1` with `SetupMode = 0` is user mode with a platform key and image verification
//     on - the only combination a gate may call enforcing.
//   - `SecureBoot = 0` is verification off.
// A check that read only `SecureBoot` would call setup-mode firmware enforcing, which is exactly
// backwards: setup mode is where anybody can enrol themselves.

use core::ffi::c_void;

use crate::{Handle, Status};

// EFI_GLOBAL_VARIABLE, the namespace these two live in. A variable name without its GUID names
// nothing: two vendors may both define `SecureBoot`.
pub const GLOBAL_VARIABLE_GUID: Guid = Guid { data1: 0x8be4_df61, data2: 0x93ca, data3: 0x11d2, data4: [0xaa, 0x0d, 0x00, 0xe0, 0x98, 0x03, 0x2b, 0x8c] };

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Guid {
	pub data1: u32,
	pub data2: u16,
	pub data3: u16,
	pub data4: [u8; 8],
}

// EFI_RUNTIME_SERVICES, typed as far as the one entry this crate uses and no further. The fields
// before it are pointers whose shapes are not needed to reach past them; naming them as `c_void`
// keeps the offset right without inviting a call.
#[repr(C)]
pub struct RuntimeServices {
	pub header: crate::TableHeader,
	pub get_time: *const c_void,
	pub set_time: *const c_void,
	pub get_wakeup_time: *const c_void,
	pub set_wakeup_time: *const c_void,
	pub set_virtual_address_map: *const c_void,
	pub convert_pointer: *const c_void,
	pub get_variable: Option<unsafe extern "efiapi" fn(name: *const u16, guid: *const Guid, attributes: *mut u32, size: *mut usize, data: *mut c_void) -> Status>,
	pub get_next_variable_name: *const c_void,
	pub set_variable: *const c_void,
}

// What firmware says about its own verification state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SecureBootState {
	pub secure_boot: Option<u8>,
	pub setup_mode: Option<u8>,
}

impl SecureBootState {
	// ENFORCING MEANS BOTH, and `None` is not a zero. Firmware that does not define these variables
	// is firmware without Secure Boot at all, which is a different thing from firmware that has it
	// turned off - and neither is enforcing.
	pub fn enforcing(&self) -> bool {
		self.secure_boot == Some(1) && self.setup_mode == Some(0)
	}
}

// Read one byte-sized global variable, or None when firmware does not define it.
//
// # Safety
// `runtime` must be the firmware's EFI_RUNTIME_SERVICES table.
unsafe fn byte_variable(runtime: *const RuntimeServices, name: &[u16]) -> Option<u8> {
	let get = unsafe { (*runtime).get_variable }?;
	let mut value: u8 = 0;
	let mut size: usize = 1;
	let status = unsafe { get(name.as_ptr(), &GLOBAL_VARIABLE_GUID, core::ptr::null_mut(), &mut size, (&raw mut value).cast()) };
	// A variable that is not there, and one whose value is not one byte, are both "this does not
	// answer the question" - not a zero.
	if status != crate::STATUS_SUCCESS || size != 1 { None } else { Some(value) }
}

// What firmware says about Secure Boot.
//
// # Safety
// `system_table` must be the table the firmware handed the image entry point.
pub unsafe fn secure_boot_state(system_table: *const crate::SystemTable) -> SecureBootState {
	let runtime = unsafe { (*system_table).runtime_services }.cast::<RuntimeServices>();
	if runtime.is_null() {
		return SecureBootState { secure_boot: None, setup_mode: None };
	}
	// UTF-16, NUL-terminated, built here rather than taken as a parameter: these two names are the
	// whole of what this module reads, and a caller that could pass a name could read any variable.
	const SECURE_BOOT: [u16; 11] = [b'S' as u16, b'e' as u16, b'c' as u16, b'u' as u16, b'r' as u16, b'e' as u16, b'B' as u16, b'o' as u16, b'o' as u16, b't' as u16, 0];
	const SETUP_MODE: [u16; 10] = [b'S' as u16, b'e' as u16, b't' as u16, b'u' as u16, b'p' as u16, b'M' as u16, b'o' as u16, b'd' as u16, b'e' as u16, 0];
	SecureBootState { secure_boot: unsafe { byte_variable(runtime, &SECURE_BOOT) }, setup_mode: unsafe { byte_variable(runtime, &SETUP_MODE) } }
}

// The handle type is re-exported so a caller does not need two imports for one call.
pub type ImageHandle = Handle;

#[cfg(test)]
mod tests {
	use super::*;

	// THE THREE-STATE RULE, which is the whole reason this module reads two variables instead of
	// one. Firmware in setup mode accepts enrolments from anybody, so it is not enforcing whatever
	// it says about `SecureBoot`; firmware that defines neither variable has no Secure Boot at all,
	// which is not the same as having it turned off.
	#[test]
	fn only_user_mode_with_verification_on_is_enforcing() {
		assert!(SecureBootState { secure_boot: Some(1), setup_mode: Some(0) }.enforcing(), "a platform key enrolled and verification on");
		assert!(!SecureBootState { secure_boot: Some(1), setup_mode: Some(1) }.enforcing(), "setup mode accepts enrolments - it is not enforcing");
		assert!(!SecureBootState { secure_boot: Some(0), setup_mode: Some(0) }.enforcing(), "verification off");
		assert!(!SecureBootState { secure_boot: None, setup_mode: None }.enforcing(), "firmware without the variables has no Secure Boot");
		assert!(!SecureBootState { secure_boot: None, setup_mode: Some(0) }.enforcing(), "and a missing variable is not a zero");
	}

	// The GUID is written out rather than parsed, so it is worth one test that it is the one the
	// specification names.
	#[test]
	fn the_namespace_is_the_efi_global_one() {
		assert_eq!(GLOBAL_VARIABLE_GUID.data1, 0x8be4_df61);
		assert_eq!(GLOBAL_VARIABLE_GUID.data2, 0x93ca);
		assert_eq!(GLOBAL_VARIABLE_GUID.data3, 0x11d2);
		assert_eq!(GLOBAL_VARIABLE_GUID.data4, [0xaa, 0x0d, 0x00, 0xe0, 0x98, 0x03, 0x2b, 0x8c]);
	}

	// The table's shape is what makes `get_variable` land on the right entry, and the offset is the
	// specification's rather than this file's opinion.
	#[test]
	fn get_variable_is_the_seventh_entry_of_runtime_services() {
		let base = core::mem::offset_of!(RuntimeServices, header);
		let get = core::mem::offset_of!(RuntimeServices, get_variable);
		let header = core::mem::size_of::<crate::TableHeader>();
		assert_eq!(base, 0);
		// Six pointers between the header and it: GetTime, SetTime, GetWakeupTime, SetWakeupTime,
		// SetVirtualAddressMap, ConvertPointer.
		assert_eq!(get, header + 6 * core::mem::size_of::<*const core::ffi::c_void>());
	}
}
