use std::env;
use std::ffi::{c_char, c_int, c_void};
use std::fs;
use std::ptr;

const ICNS_STATUS_OK: c_int = 0;
const ICNS_128X128_32BIT_DATA: u32 = 0x6974_3332;
const ICNS_128X128_8BIT_MASK: u32 = 0x7438_6d6b;
const PIXELS: usize = 128 * 128;

#[repr(C)]
struct IcnsImage {
	image_width: u32,
	image_height: u32,
	image_channels: u8,
	image_pixel_depth: u16,
	image_data_size: u64,
	image_data: *mut u8,
	png_filename: *const c_char,
}

#[link(name = "icns")]
unsafe extern "C" {
	fn icns_init_image(width: u32, height: u32, channels: u32, pixel_depth: u32, output: *mut IcnsImage) -> c_int;
	fn icns_free_image(image: *mut IcnsImage) -> c_int;
	fn icns_create_family(output: *mut *mut c_void) -> c_int;
	fn icns_new_element_from_image(image: *mut IcnsImage, kind: u32, output: *mut *mut c_void) -> c_int;
	fn icns_new_element_from_mask(image: *mut IcnsImage, kind: u32, output: *mut *mut c_void) -> c_int;
	fn icns_add_element_in_family(family: *mut *mut c_void, element: *mut c_void) -> c_int;
	fn icns_export_family_data(family: *mut c_void, size: *mut i32, data: *mut *mut u8) -> c_int;
	fn free(pointer: *mut c_void);
}

fn check(status: c_int, step: &str) {
	if status != ICNS_STATUS_OK {
		panic!("{step} failed: {status}");
	}
}

fn main() {
	let arguments: Vec<String> = env::args().collect();
	assert_eq!(arguments.len(), 3, "usage: generate-legacy input.rgba output.icns");
	let source = fs::read(&arguments[1]).expect("cannot read RGBA input");
	assert_eq!(source.len(), PIXELS * 4, "input must be exact 128x128 RGBA");

	unsafe {
		let mut image = std::mem::zeroed::<IcnsImage>();
		let mut alpha = std::mem::zeroed::<IcnsImage>();
		check(icns_init_image(128, 128, 4, 8, &mut image), "init image");
		check(icns_init_image(128, 128, 1, 8, &mut alpha), "init alpha");
		ptr::copy_nonoverlapping(source.as_ptr(), image.image_data, source.len());
		for pixel in 0..PIXELS {
			*alpha.image_data.add(pixel) = source[pixel * 4 + 3];
		}

		let mut family = ptr::null_mut();
		let mut color = ptr::null_mut();
		let mut mask = ptr::null_mut();
		check(icns_create_family(&mut family), "create family");
		check(icns_new_element_from_image(&mut image, ICNS_128X128_32BIT_DATA, &mut color), "encode it32");
		check(icns_new_element_from_mask(&mut alpha, ICNS_128X128_8BIT_MASK, &mut mask), "encode t8mk");
		check(icns_add_element_in_family(&mut family, color), "add it32");
		check(icns_add_element_in_family(&mut family, mask), "add t8mk");

		let mut size = 0i32;
		let mut data = ptr::null_mut();
		check(icns_export_family_data(family, &mut size, &mut data), "export family");
		assert!(size >= 0 && !data.is_null(), "libicns returned invalid output");
		fs::write(&arguments[2], std::slice::from_raw_parts(data, size as usize)).expect("cannot write output");

		check(icns_free_image(&mut image), "free image");
		check(icns_free_image(&mut alpha), "free alpha");
		free(data.cast());
		free(color);
		free(mask);
		free(family);
	}
}
