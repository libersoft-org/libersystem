// Minimal hand-rolled UEFI FFI: exactly the types, protocols, and boot-service
// calls the loader needs - nothing more. No external crate; the layouts and GUIDs
// come straight from the UEFI specification. Every firmware entry point uses the
// `efiapi` calling convention.
//
// The loader touches only: memory allocation (AllocatePages / AllocatePool), the
// memory map (GetMemoryMap), the loaded-image + simple-file-system protocols (to
// read the kernel and packages off the boot medium), the Graphics Output Protocol
// (the framebuffer), the ACPI configuration table (the RSDP), and ExitBootServices.

#![allow(dead_code)]

use core::ffi::c_void;

// A firmware return code: 0 is success, the high bit marks an error.
pub type Status = usize;

pub const STATUS_SUCCESS: Status = 0;
pub const STATUS_BUFFER_TOO_SMALL: Status = high_bit(5);
pub const STATUS_NOT_FOUND: Status = high_bit(14);

// Set the top bit (the UEFI error marker) on a small error number.
const fn high_bit(n: usize) -> usize {
	n | (1usize << (usize::BITS - 1))
}

// True for any UEFI error status (top bit set).
pub fn is_error(status: Status) -> bool {
	(status & (1usize << (usize::BITS - 1))) != 0
}

pub type Handle = *mut c_void;
pub type Event = *mut c_void;

// A UEFI GUID (mixed-endian first three fields, as stored on disk / in memory).
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Guid {
	pub d1: u32,
	pub d2: u16,
	pub d3: u16,
	pub d4: [u8; 8],
}

impl Guid {
	pub const fn new(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> Self {
		Self { d1, d2, d3, d4 }
	}
}

// Common header on every UEFI table.
#[repr(C)]
pub struct TableHeader {
	pub signature: u64,
	pub revision: u32,
	pub header_size: u32,
	pub crc32: u32,
	pub reserved: u32,
}

// EFI_MEMORY_TYPE: the kind of a memory-map region (and the pool/page type asked
// of the allocator).
pub type MemoryType = u32;
pub const RESERVED_MEMORY_TYPE: MemoryType = 0;
pub const LOADER_CODE: MemoryType = 1;
pub const LOADER_DATA: MemoryType = 2;
pub const BOOT_SERVICES_CODE: MemoryType = 3;
pub const BOOT_SERVICES_DATA: MemoryType = 4;
pub const RUNTIME_SERVICES_CODE: MemoryType = 5;
pub const RUNTIME_SERVICES_DATA: MemoryType = 6;
pub const CONVENTIONAL_MEMORY: MemoryType = 7;
pub const UNUSABLE_MEMORY: MemoryType = 8;
pub const ACPI_RECLAIM_MEMORY: MemoryType = 9;
pub const ACPI_MEMORY_NVS: MemoryType = 10;
pub const MEMORY_MAPPED_IO: MemoryType = 11;
pub const MEMORY_MAPPED_IO_PORT_SPACE: MemoryType = 12;
pub const PAL_CODE: MemoryType = 13;
pub const PERSISTENT_MEMORY: MemoryType = 14;

// EFI_ALLOCATE_TYPE passed to AllocatePages.
pub type AllocateType = u32;
pub const ALLOCATE_ANY_PAGES: AllocateType = 0;
pub const ALLOCATE_MAX_ADDRESS: AllocateType = 1;
pub const ALLOCATE_ADDRESS: AllocateType = 2;

pub type PhysicalAddress = u64;
pub type VirtualAddress = u64;

// One EFI_MEMORY_DESCRIPTOR. `descriptor_size` from GetMemoryMap may exceed
// size_of::<MemoryDescriptor>(), so the map must be strided by that byte count,
// never by this struct's size.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MemoryDescriptor {
	pub ty: u32,
	pub _pad: u32,
	pub phys_start: PhysicalAddress,
	pub virt_start: VirtualAddress,
	pub page_count: u64,
	pub attribute: u64,
}

// EFI_TABLE_HEADER-tagged EFI_BOOT_SERVICES: only the entry points the loader uses
// are typed; the rest are opaque `*const c_void` placeholders that keep the vtable
// offsets correct.
#[repr(C)]
pub struct BootServices {
	pub header: TableHeader,

	// Task priority.
	pub raise_tpl: *const c_void,
	pub restore_tpl: *const c_void,

	// Memory.
	pub allocate_pages: unsafe extern "efiapi" fn(AllocateType, MemoryType, usize, *mut PhysicalAddress) -> Status,
	pub free_pages: unsafe extern "efiapi" fn(PhysicalAddress, usize) -> Status,
	pub get_memory_map: unsafe extern "efiapi" fn(*mut usize, *mut MemoryDescriptor, *mut usize, *mut usize, *mut u32) -> Status,
	pub allocate_pool: unsafe extern "efiapi" fn(MemoryType, usize, *mut *mut c_void) -> Status,
	pub free_pool: unsafe extern "efiapi" fn(*mut c_void) -> Status,

	// Event & timer.
	pub create_event: *const c_void,
	pub set_timer: *const c_void,
	pub wait_for_event: *const c_void,
	pub signal_event: *const c_void,
	pub close_event: *const c_void,
	pub check_event: *const c_void,

	// Protocol handlers.
	pub install_protocol_interface: *const c_void,
	pub reinstall_protocol_interface: *const c_void,
	pub uninstall_protocol_interface: *const c_void,
	pub handle_protocol: unsafe extern "efiapi" fn(Handle, *const Guid, *mut *mut c_void) -> Status,
	pub reserved: *const c_void,
	pub register_protocol_notify: *const c_void,
	pub locate_handle: *const c_void,
	pub locate_device_path: *const c_void,
	pub install_configuration_table: *const c_void,

	// Image services.
	pub load_image: *const c_void,
	pub start_image: *const c_void,
	pub exit: *const c_void,
	pub unload_image: *const c_void,
	pub exit_boot_services: unsafe extern "efiapi" fn(Handle, usize) -> Status,

	// Misc.
	pub get_next_monotonic_count: *const c_void,
	pub stall: *const c_void,
	pub set_watchdog_timer: *const c_void,

	// DriverSupport.
	pub connect_controller: *const c_void,
	pub disconnect_controller: *const c_void,

	// Open/close protocol.
	pub open_protocol: *const c_void,
	pub close_protocol: *const c_void,
	pub open_protocol_information: *const c_void,

	// Library.
	pub protocols_per_handle: *const c_void,
	pub locate_handle_buffer: *const c_void,
	pub locate_protocol: unsafe extern "efiapi" fn(*const Guid, *mut c_void, *mut *mut c_void) -> Status,
	pub install_multiple_protocol_interfaces: *const c_void,
	pub uninstall_multiple_protocol_interfaces: *const c_void,

	// CRC.
	pub calculate_crc32: *const c_void,

	// Misc mem.
	pub copy_mem: *const c_void,
	pub set_mem: *const c_void,
	pub create_event_ex: *const c_void,
}

// One EFI_CONFIGURATION_TABLE entry (vendor GUID -> vendor table pointer). The
// loader scans these for the ACPI 2.0 RSDP.
#[repr(C)]
pub struct ConfigurationTable {
	pub vendor_guid: Guid,
	pub vendor_table: *mut c_void,
}

// EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL: only OutputString is typed, for early boot logs.
#[repr(C)]
pub struct SimpleTextOutput {
	pub reset: *const c_void,
	pub output_string: unsafe extern "efiapi" fn(*mut SimpleTextOutput, *const u16) -> Status,
	// The rest is unused.
	pub test_string: *const c_void,
	pub query_mode: *const c_void,
	pub set_mode: *const c_void,
	pub set_attribute: *const c_void,
	pub clear_screen: *const c_void,
	pub set_cursor_position: *const c_void,
	pub enable_cursor: *const c_void,
	pub mode: *const c_void,
}

// EFI_SYSTEM_TABLE: the root the firmware hands the loader's entry point.
#[repr(C)]
pub struct SystemTable {
	pub header: TableHeader,
	pub firmware_vendor: *const u16,
	pub firmware_revision: u32,
	pub console_in_handle: Handle,
	pub con_in: *const c_void,
	pub console_out_handle: Handle,
	pub con_out: *mut SimpleTextOutput,
	pub standard_error_handle: Handle,
	pub std_err: *mut SimpleTextOutput,
	pub runtime_services: *const c_void,
	pub boot_services: *mut BootServices,
	pub number_of_table_entries: usize,
	pub configuration_table: *mut ConfigurationTable,
}

// EFI_LOADED_IMAGE_PROTOCOL: gives the loader the device handle it was loaded
// from, so it can open that device's filesystem.
#[repr(C)]
pub struct LoadedImage {
	pub revision: u32,
	pub parent_handle: Handle,
	pub system_table: *const c_void,
	pub device_handle: Handle,
	pub file_path: *const c_void,
	pub reserved: *const c_void,
	pub load_options_size: u32,
	pub load_options: *const c_void,
	pub image_base: *const c_void,
	pub image_size: u64,
	pub image_code_type: MemoryType,
	pub image_data_type: MemoryType,
	pub unload: *const c_void,
}

// EFI_SIMPLE_FILE_SYSTEM_PROTOCOL: opens the root directory of a FAT volume.
#[repr(C)]
pub struct SimpleFileSystem {
	pub revision: u64,
	pub open_volume: unsafe extern "efiapi" fn(*mut SimpleFileSystem, *mut *mut FileProtocol) -> Status,
}

// EFI_FILE_PROTOCOL: open / read / seek / query / close a file or directory.
#[repr(C)]
pub struct FileProtocol {
	pub revision: u64,
	pub open: unsafe extern "efiapi" fn(*mut FileProtocol, *mut *mut FileProtocol, *const u16, u64, u64) -> Status,
	pub close: unsafe extern "efiapi" fn(*mut FileProtocol) -> Status,
	pub delete: *const c_void,
	pub read: unsafe extern "efiapi" fn(*mut FileProtocol, *mut usize, *mut c_void) -> Status,
	pub write: *const c_void,
	pub get_position: *const c_void,
	pub set_position: unsafe extern "efiapi" fn(*mut FileProtocol, u64) -> Status,
	pub get_info: unsafe extern "efiapi" fn(*mut FileProtocol, *const Guid, *mut usize, *mut c_void) -> Status,
	pub set_info: *const c_void,
	pub flush: *const c_void,
}

// EFI_FILE_MODE_READ.
pub const FILE_MODE_READ: u64 = 0x0000000000000001;

// Head of EFI_FILE_INFO (GetInfo output); the variable-length filename follows.
#[repr(C)]
pub struct FileInfo {
	pub size: u64,
	pub file_size: u64,
	pub physical_size: u64,
	pub create_time: [u8; 16],
	pub last_access_time: [u8; 16],
	pub modification_time: [u8; 16],
	pub attribute: u64,
	// followed by CHAR16 file_name[]
}

// EFI_GRAPHICS_OUTPUT_PROTOCOL: the linear framebuffer.
#[repr(C)]
pub struct GraphicsOutput {
	pub query_mode: *const c_void,
	pub set_mode: *const c_void,
	pub blt: *const c_void,
	pub mode: *mut GraphicsOutputMode,
}

#[repr(C)]
pub struct GraphicsOutputMode {
	pub max_mode: u32,
	pub mode: u32,
	pub info: *mut GraphicsOutputModeInfo,
	pub size_of_info: usize,
	pub frame_buffer_base: PhysicalAddress,
	pub frame_buffer_size: usize,
}

// EFI_GRAPHICS_PIXEL_FORMAT values.
pub const PIXEL_RGB: u32 = 0;
pub const PIXEL_BGR: u32 = 1;
pub const PIXEL_BIT_MASK: u32 = 2;
pub const PIXEL_BLT_ONLY: u32 = 3;

#[repr(C)]
pub struct PixelBitmask {
	pub red: u32,
	pub green: u32,
	pub blue: u32,
	pub reserved: u32,
}

#[repr(C)]
pub struct GraphicsOutputModeInfo {
	pub version: u32,
	pub horizontal_resolution: u32,
	pub vertical_resolution: u32,
	pub pixel_format: u32,
	pub pixel_information: PixelBitmask,
	pub pixels_per_scan_line: u32,
}

// Protocol / vendor-table GUIDs (UEFI spec + ACPI spec).
pub const LOADED_IMAGE_PROTOCOL_GUID: Guid = Guid::new(0x5B1B31A1, 0x9562, 0x11d2, [0x8E, 0x3F, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B]);
pub const SIMPLE_FILE_SYSTEM_PROTOCOL_GUID: Guid = Guid::new(0x964e5b22, 0x6459, 0x11d2, [0x8e, 0x39, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b]);
pub const FILE_INFO_GUID: Guid = Guid::new(0x09576e92, 0x6d3f, 0x11d2, [0x8e, 0x39, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b]);
pub const GRAPHICS_OUTPUT_PROTOCOL_GUID: Guid = Guid::new(0x9042a9de, 0x23dc, 0x4a38, [0x96, 0xfb, 0x7a, 0xde, 0xd0, 0x80, 0x51, 0x6a]);
pub const ACPI_20_TABLE_GUID: Guid = Guid::new(0x8868e871, 0xe4f1, 0x11d3, [0xbc, 0x22, 0x00, 0x80, 0xc7, 0x3c, 0x88, 0x81]);
pub const ACPI_10_TABLE_GUID: Guid = Guid::new(0xeb9d2d30, 0x2d88, 0x11d3, [0x9a, 0x16, 0x00, 0x90, 0x27, 0x3f, 0xc1, 0x4d]);
