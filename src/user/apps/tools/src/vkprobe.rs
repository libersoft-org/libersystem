// vkprobe - run the AUDIT-LINKED loader in a guest, against a synthetic ICD.
//
// WHAT THIS IS FOR, AND WHY NOTHING SMALLER WOULD DO. The selection-slot gate proves a provider is
// bound and reachable; the surface gates prove what the pinned configuration asks a system for. What
// neither shows is the thing this milestone is actually preparing: the PORTED LOADER running. A
// substrate that satisfies every undefined symbol and still cannot load an ICD has prepared nothing,
// and a gate that never runs the thing being ported proves the wrong half.
//
// IT IS A QUARANTINE ARTIFACT AND IS NOT SHIPPED. Its bytes come from the audit link rather than
// from the ordinary consumer build: the loader's sources are audit-only and are not in this tree, so
// this program is staged into the DEVELOPMENT image the gate builds and into no other.
//
// WHAT IT DOES, IN ORDER:
//   1. installs the substrate's diagnostic sink, so a foreign diagnostic reaches this console
//      instead of the default, which panics rather than discarding;
//   2. installs the discovery record naming the ICD bound into its own closure through a selection
//      slot - by the two exports that ICD has and by nothing else;
//   3. asks the loader for the layer list, which the port makes always empty;
//   4. asks it to create an instance ENABLING a layer, which must be refused by name;
//   5. asks it for an instance procedure address, which is the path that reaches the ICD.

#![no_std]
#![no_main]

use rt::*;

// THE SUBSTRATE'S CONTROL SURFACE. Not inventory surface: these are how a launch hands the substrate
// what only a launch knows, which is the opposite direction from every other symbol it exports.
unsafe extern "C" {
	fn liber_foreign_install_sink(report: unsafe extern "C" fn(*const u8, usize), stop: unsafe extern "C" fn() -> !);
	fn liber_foreign_install_icd(manifest_path: *const u8, manifest: *const u8, library_path: *const u8, self_path: *const u8, negotiate: unsafe extern "C" fn(*mut u32) -> i32, gpa: unsafe extern "C" fn(*mut core::ffi::c_void, *const u8) -> *mut core::ffi::c_void) -> i32;
}

// THE ICD, REACHED THROUGH A SELECTION SLOT. The consumer names no provider for it: the candidates
// are carried by digest in its authenticated identity record and ProcessService binds one before the
// first thread runs. These are the two exports the `vulkan-icd` kind admits, and there is no third.
unsafe extern "C" {
	fn vk_icdNegotiateLoaderICDInterfaceVersion(version: *mut u32) -> i32;
	fn vk_icdGetInstanceProcAddr(instance: *mut core::ffi::c_void, name: *const u8) -> *mut core::ffi::c_void;
}

// THE LOADER'S OWN EXPORTS, which is what "running the ported loader" means. They come from the
// audit-linked artifact this program is linked against.
unsafe extern "C" {
	fn vkEnumerateInstanceLayerProperties(count: *mut u32, properties: *mut core::ffi::c_void) -> i32;
	fn vkEnumerateInstanceExtensionProperties(layer: *const u8, count: *mut u32, properties: *mut core::ffi::c_void) -> i32;
	fn vkGetInstanceProcAddr(instance: *mut core::ffi::c_void, name: *const u8) -> *mut core::ffi::c_void;
	fn vkCreateInstance(info: *const InstanceCreateInfo, allocator: *const core::ffi::c_void, instance: *mut *mut core::ffi::c_void) -> i32;
	fn vkDestroyInstance(instance: *mut core::ffi::c_void, allocator: *const core::ffi::c_void);
}

/// `VkInstanceCreateInfo`, laid out by hand because the pinned headers are audit-only and this
/// program is built from this tree. Every field is here even though most are zero: a struct written
/// short is one the loader reads past the end of.
#[repr(C)]
struct InstanceCreateInfo {
	structure_type: u32,
	next: *const core::ffi::c_void,
	flags: u32,
	application_info: *const core::ffi::c_void,
	enabled_layer_count: u32,
	enabled_layer_names: *const *const u8,
	enabled_extension_count: u32,
	enabled_extension_names: *const *const u8,
}

/// `VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO`.
const INSTANCE_CREATE_INFO: u32 = 1;

/// The Vulkan result values this program compares against, spelled here because the pinned headers
/// are audit-only and this program is built from this tree.
const VK_SUCCESS: i32 = 0;
const VK_ERROR_LAYER_NOT_PRESENT: i32 = -6;

// THE ICD IS REACHED THROUGH SHIMS, AND THAT IS WHAT MAKES IT OBSERVABLE. The loader opens a
// provider through the substrate's replaced lookup and then calls the two entry points it was
// handed; handing it these instead of the ICD's own, and forwarding, counts the calls without
// changing what happens. The ICD itself cannot report: its export surface is exactly two symbols by
// the kind's own rule, and a counter would be a third.
static NEGOTIATED: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static LOOKED_UP: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

unsafe extern "C" fn negotiate_shim(version: *mut u32) -> i32 {
	NEGOTIATED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
	// SAFETY: the loader passes a live pointer, and this forwards it unchanged to the provider the
	// launch bound into this closure.
	unsafe { vk_icdNegotiateLoaderICDInterfaceVersion(version) }
}

unsafe extern "C" fn get_instance_proc_addr_shim(instance: *mut core::ffi::c_void, name: *const u8) -> *mut core::ffi::c_void {
	LOOKED_UP.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
	// SAFETY: as above - the arguments are the loader's own and are forwarded unchanged.
	unsafe { vk_icdGetInstanceProcAddr(instance, name) }
}

unsafe extern "C" fn report(bytes: *const u8, len: usize) {
	// SAFETY: the substrate passes a slice out of its own image.
	print(b"vkprobe: foreign: ");
	print(unsafe { core::slice::from_raw_parts(bytes, len) });
	print(b"\n");
}

unsafe extern "C" fn stop() -> ! {
	print(b"vkprobe: the substrate stopped the process\n");
	exit()
}

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

fn number(label: &[u8], value: u32) {
	print(label);
	let (digits, at) = decimal(value);
	print(&digits[at..]);
	print(b"\n");
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	inherit_stdout(bootstrap);
	let _ = recv_launch_bytes(bootstrap);

	// SAFETY: both pointers are functions in this image, which outlives every foreign call.
	unsafe { liber_foreign_install_sink(report, stop) };

	// THE RECORD. The paths are what the loader compares against - it was built to work with paths -
	// and they reach no file system: they select among what this record holds, which is one provider
	// already in the verified closure.
	let installed = unsafe { liber_foreign_install_icd(b"/system/share/vulkan/icd.d/icdprobe.json\0".as_ptr(), b"{\"file_format_version\":\"1.0.0\",\"ICD\":{\"library_path\":\"/system/lib/foreign/icdprobe.lslib\",\"api_version\":\"1.3.0\"}}\0".as_ptr(), b"/system/lib/foreign/icdprobe.lslib\0".as_ptr(), b"/system/libexec/vkprobe.lsexe\0".as_ptr(), negotiate_shim, get_instance_proc_addr_shim) };
	number(b"vkprobe: record installed=", installed as u32);

	// 3. THE LAYER LIST. The port makes this always empty, which is legal under the specification and
	//    true here: there is no layer directory and no way to load one.
	let mut layers: u32 = 0xffff_ffff;
	let result = unsafe { vkEnumerateInstanceLayerProperties(&raw mut layers, core::ptr::null_mut()) };
	print(if result == VK_SUCCESS { b"vkprobe: layers accepted " } else { b"vkprobe: layers refused " });
	number(b"count=", layers);

	// 4. AN EXTENSION QUERY NAMING A LAYER. Asking for the extensions OF a layer is the shortest
	//    request that has to look a layer up, and a loader that answers zero layers must refuse it by
	//    name rather than silently returning an empty list - dropping a requested layer is how a
	//    validation build reports success while validating nothing.
	let mut extensions: u32 = 0;
	let refused = unsafe { vkEnumerateInstanceExtensionProperties(b"VK_LAYER_KHRONOS_validation\0".as_ptr(), &raw mut extensions, core::ptr::null_mut()) };
	print(if refused == VK_ERROR_LAYER_NOT_PRESENT { b"vkprobe: enabled layer refused by name\n" } else { b"vkprobe: enabled layer NOT refused\n" });
	number(b"vkprobe: layer query result=", refused as u32);

	// 5. THE PATH THAT REACHES THE ICD. `vkGetInstanceProcAddr` with a null instance answers out of
	//    the loader's own table; the point is that the loader is running its own code here, in a
	//    guest, on this substrate.
	let entry = unsafe { vkGetInstanceProcAddr(core::ptr::null_mut(), b"vkCreateInstance\0".as_ptr()) };
	print(if entry.is_null() { b"vkprobe: loader lookup null\n" } else { b"vkprobe: loader lookup address\n" });

	// 6. AND THE CALL THAT MAKES THE LOADER REACH THE DRIVER. Enumerating instance extensions with no
	//    layer named is the shortest request that has to consult every ICD: the loader opens the
	//    provider through the substrate's replaced lookup and calls the two entry points it was
	//    handed. The counts below are what say it did.
	let mut driver_extensions: u32 = 0;
	let reached = unsafe { vkEnumerateInstanceExtensionProperties(core::ptr::null(), &raw mut driver_extensions, core::ptr::null_mut()) };
	number(b"vkprobe: driver extension query result=", reached as u32);

	// 7. AND THE CALL THAT ACTUALLY REACHES THE DRIVER. The extension query above is answered out of
	//    the loader's own tables; creating an INSTANCE is what makes it enumerate drivers, open each
	//    one through the substrate's replaced lookup, and call the two entry points it was handed.
	let info = InstanceCreateInfo { structure_type: INSTANCE_CREATE_INFO, next: core::ptr::null(), flags: 0, application_info: core::ptr::null(), enabled_layer_count: 0, enabled_layer_names: core::ptr::null(), enabled_extension_count: 0, enabled_extension_names: core::ptr::null() };
	let mut instance: *mut core::ffi::c_void = core::ptr::null_mut();
	// SAFETY: the structure is fully initialised above and outlives the call; the loader writes at
	// most one handle into `instance`.
	let created = unsafe { vkCreateInstance(&raw const info, core::ptr::null(), &raw mut instance) };
	number(b"vkprobe: instance result=", created as u32);
	number(b"vkprobe: icd negotiations=", NEGOTIATED.load(core::sync::atomic::Ordering::Relaxed));
	number(b"vkprobe: icd lookups=", LOOKED_UP.load(core::sync::atomic::Ordering::Relaxed));
	if created == VK_SUCCESS && !instance.is_null() {
		// SAFETY: the handle came from the call above and is destroyed exactly once.
		unsafe { vkDestroyInstance(instance, core::ptr::null()) };
	}

	// 8. AND THE SAME CALL WITH A LAYER ENABLED, which must be refused by name. Silently dropping a
	//    requested layer is how a validation build reports success while validating nothing.
	let layer: *const u8 = b"VK_LAYER_KHRONOS_validation\0".as_ptr();
	let with_layer = InstanceCreateInfo { enabled_layer_count: 1, enabled_layer_names: &raw const layer, ..info };
	let mut refused_instance: *mut core::ffi::c_void = core::ptr::null_mut();
	// SAFETY: as above.
	let refused_create = unsafe { vkCreateInstance(&raw const with_layer, core::ptr::null(), &raw mut refused_instance) };
	print(if refused_create == VK_ERROR_LAYER_NOT_PRESENT { b"vkprobe: instance with a layer refused by name\n" } else { b"vkprobe: instance with a layer NOT refused\n" });

	print(b"vkprobe: done\n");
	exit();
}
