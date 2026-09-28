//! RESOURCE TEMPLATES: every descriptor a platform row or a connection is built from, decoded, and hostile
//! templates refused.

use std::string::String;
use std::vec;

use super::build::*;
use crate::resource::{Resource, ResourceError, decode, eisa_id, eisa_value};

#[test]
fn every_descriptor_a_row_or_a_connection_needs_decodes() {
	let bytes = template(&[
		memory32_fixed(false, 0xFEDC_0000, 0x100),
		io(0x62, 1),
		irq_no_flags(&[1, 12]),
		interrupt(true, true, true, &[0x2F]),
		interrupt(false, false, false, &[0x30]),
		qword_memory(false, 0x1_0000_0000, 0x1000),
		qword_memory(true, 0x8000_0000, 0x1000_0000),
		gpio(true, 0x03, &[17], "\\_SB.GPI0"),
		gpio(false, 0x01, &[2, 3], "\\_SB.GPI0"),
		i2c(0x2C, 400_000, "\\_SB.I2C1"),
	]);
	let decoded = decode(&bytes).unwrap();
	assert_eq!(decoded[0], Resource::Memory { base: 0xFEDC_0000, length: 0x100, writable: false });
	assert_eq!(decoded[1], Resource::Io { base: 0x62, length: 1 });
	assert!(matches!(decoded[2], Resource::Interrupt { line: 1, .. }));
	assert!(matches!(decoded[3], Resource::Interrupt { line: 12, .. }));
	assert_eq!(decoded[4], Resource::Interrupt { line: 0x2F, level: true, active_low: true, shared: false, wake: false });
	// A producer's interrupt and a producer's window are not the device's own.
	assert_eq!(decoded[5], Resource::Memory { base: 0x1_0000_0000, length: 0x1000, writable: true });
	assert!(matches!(decoded[6], Resource::Other { tag: 0x8A }));
	match &decoded[7] {
		Resource::Gpio { interrupt, pins, controller, level, active_low, .. } => {
			assert!(*interrupt);
			assert_eq!(pins, &vec![17]);
			assert_eq!(controller, "\\_SB.GPI0");
			assert!(!*level, "mode bit set: edge");
			assert!(*active_low);
		}
		other => panic!("{other:?}"),
	}
	match &decoded[8] {
		Resource::Gpio { interrupt, pins, restriction, .. } => {
			assert!(!*interrupt);
			assert_eq!(pins, &vec![2, 3]);
			assert_eq!(*restriction, 1, "input only");
		}
		other => panic!("{other:?}"),
	}
	match &decoded[9] {
		Resource::I2c { address, speed, controller, ten_bit, .. } => {
			assert_eq!((*address, *speed, controller.as_str(), *ten_bit), (0x2C, 400_000, "\\_SB.I2C1", false));
		}
		other => panic!("{other:?}"),
	}
	assert_eq!(decoded.len(), 10);
}

#[test]
fn a_truncated_template_and_one_with_no_end_are_refused() {
	let mut bytes = template(&[memory32_fixed(true, 0x1000, 0x10)]);
	bytes.truncate(8);
	assert!(matches!(decode(&bytes), Err(ResourceError::Truncated(0))));
	let no_end = io(0x60, 1);
	assert_eq!(decode(&no_end), Err(ResourceError::NoEnd));
	// A large descriptor claiming more bytes than the buffer has.
	let mut lying = memory32_fixed(true, 0, 0);
	lying[1] = 0xFF;
	assert!(matches!(decode(&lying), Err(ResourceError::Truncated(0))));
}

#[test]
fn eisa_ids_round_trip() {
	for text in ["PNP0A08", "PNP0C09", "PNP0C0A", "ACPI0003", "MSFT0101"] {
		if text.len() != 7 {
			assert!(eisa_value(text).is_none());
			continue;
		}
		let value = eisa_value(text).unwrap();
		assert_eq!(eisa_id(value), String::from(text));
	}
	assert_eq!(eisa_value("PNP0A08"), Some(0x080A_D041));
}
