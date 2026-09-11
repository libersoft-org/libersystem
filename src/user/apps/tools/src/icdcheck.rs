// icdcheck - reach a selected provider through a selection slot and report what it agreed to.
//
// WHY THIS PROGRAM EXISTS. A selection slot is a declared provider position a consumer is built
// against WITHOUT naming one provider: there is no `DT_NEEDED` edge, the candidates are named by
// digest inside the authenticated identity record, and ProcessService binds one of them into the
// closure before the first thread runs. Every part of that is invisible from outside the launch, so
// the only way to show it works is a program that CALLS through the slot and says what happened.
//
// WHAT IT PROVES, AND WHAT IT DOES NOT. It proves the bound provider is reachable and that the
// version negotiation between the two admitted ends produces the admitted answers. It implements no
// Vulkan and asks for no device: the provider behind the slot is a synthetic ICD, and a real driver
// is a separately authorised milestone away.

#![no_std]
#![no_main]

use rt::*;

// THE TWO EXPORTS, AND THERE IS NO THIRD. The Loader/Driver interface requires an ICD to export
// negotiation and the instance procedure lookup for interface versions 2 through 6; version 4's
// `vk_icdGetPhysicalDeviceProcAddr` is outside this profile and is refused by name, which is why
// nothing here declares it. There is no `dlsym` equivalent either: these resolve as ordinary
// provider exports, through the slot the launch bound.
unsafe extern "C" {
	fn vk_icdNegotiateLoaderICDInterfaceVersion(version: *mut u32) -> i32;
	fn vk_icdGetInstanceProcAddr(instance: *mut core::ffi::c_void, name: *const u8) -> *mut core::ffi::c_void;
}

/// What the substrate admits. Two exports are necessary AND sufficient across all five: negotiation
/// exists from 2, and it is only at 7 that the interface functions may be obtained through
/// `vk_icdGetInstanceProcAddr` rather than being exported.
const LOWEST: u32 = 2;
const HIGHEST: u32 = 6;

fn decimal(value: u32) -> ([u8; 10], usize) {
	let mut digits = [b'0'; 10];
	let mut at = digits.len();
	let mut left = value;
	loop {
		at -= 1;
		digits[at] = b'0' + (left % 10) as u8;
		left /= 10;
		if left == 0 {
			break;
		}
	}
	(digits, at)
}

fn report(label: &[u8], asked: u32, result: i32, agreed: u32) {
	print(label);
	print(b" asked=");
	let (digits, at) = decimal(asked);
	print(&digits[at..]);
	print(if result == 0 { b" accepted" } else { b" refused" });
	print(b" agreed=");
	let (digits, at) = decimal(agreed);
	print(&digits[at..]);
	print(b"\n");
}

/// Negotiate once, exactly as a loader would: write what this end supports, and read back what the
/// driver will do.
fn negotiate(asked: u32) -> (i32, u32) {
	let mut version = asked;
	// SAFETY: the slot was bound before the first thread ran, so the provider carrying this symbol
	// is in the verified closure; `version` is a live local.
	let result = unsafe { vk_icdNegotiateLoaderICDInterfaceVersion(&raw mut version) };
	(result, version)
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	inherit_stdout(bootstrap);
	let _ = recv_launch_bytes(bootstrap);

	// THE RANGE, NOT ONE CONVENIENT MEMBER OF IT. A gate that exercised a single version would leave
	// the behaviour of the substrate at every other admitted version undecided.
	let (low_result, low_agreed) = negotiate(LOWEST);
	report(b"icd lowest", LOWEST, low_result, low_agreed);
	let (high_result, high_agreed) = negotiate(HIGHEST);
	report(b"icd highest", HIGHEST, high_result, high_agreed);

	// BELOW THE FLOOR IS REFUSED. Version 1 has no negotiation function at all and version 0 has a
	// different bootstrap shape; a driver asked to speak either has nothing to agree to, and the
	// refusal is what keeps the two-export rule true rather than merely stated.
	for asked in [0u32, 1] {
		let (result, agreed) = negotiate(asked);
		report(b"icd below", asked, result, agreed);
	}

	// ABOVE THE CEILING IS LOWERED, NOT ACCEPTED AS ASKED. At 7 the interface functions may be
	// queried instead of exported, so a conforming driver need not export what this substrate
	// resolves; agreeing to 6 is the driver saying it will keep exporting them.
	let (above_result, above_agreed) = negotiate(7);
	report(b"icd above", 7, above_result, above_agreed);

	// THE LOOKUP IS CONSULTED, which is what says the second export is reachable and not merely
	// present in a symbol table. Its own name is the one query an admitted driver answers; a Vulkan
	// entry point is answered with the null pointer, the same answer a real driver gives for a
	// function it does not have.
	let known = b"vk_icdNegotiateLoaderICDInterfaceVersion\0";
	let unknown = b"vkCreateInstance\0";
	// SAFETY: as above - the provider is in the verified closure, and both names are NUL terminated.
	let (found, absent) = unsafe { (vk_icdGetInstanceProcAddr(core::ptr::null_mut(), known.as_ptr()), vk_icdGetInstanceProcAddr(core::ptr::null_mut(), unknown.as_ptr())) };
	print(b"icd lookup known=");
	print(if found.is_null() { b"null" } else { b"address" });
	print(b" unknown=");
	print(if absent.is_null() { b"null" } else { b"address" });
	print(b"\n");
	exit();
}
