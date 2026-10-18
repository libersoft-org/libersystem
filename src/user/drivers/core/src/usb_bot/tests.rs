use super::*;
use alloc::vec;
use alloc::vec::Vec;

// A BULK-ONLY DISK, played by the test: it reads the CBW out of the wrapper page, answers the command set the
// transport sends - TEST UNIT READY with one power-on unit attention first, REQUEST SENSE, READ CAPACITY, READ(10),
// WRITE(10) and SYNCHRONIZE CACHE - and writes its CSW back where the transport reads it. Faults are planted by the
// test: a stall in the next data stage, a CSW with the wrong signature, a residue that lies.
struct Disk {
	sectors: Vec<[u8; 512]>,
	wrapper: Vec<u8>,
	data: Vec<u8>,
	attention: bool,
	sense: [u8; 18],
	// The command the last CBW carried, until its CSW has gone.
	pending: Option<(u32, Vec<u8>, u32, bool)>,
	status: u8,
	residue: u32,
	moved: u32,
	stall_next_data: bool,
	stalled_in: bool,
	bad_signature: bool,
	lying_residue: bool,
	recovered: Vec<bool>,
	resets: u32,
}

impl Disk {
	fn new(sectors: usize) -> Disk {
		Disk { sectors: vec![[0u8; 512]; sectors], wrapper: vec![0u8; 64], data: vec![0u8; 4096], attention: true, sense: [0; 18], pending: None, status: 0, residue: 0, moved: 0, stall_next_data: false, stalled_in: false, bad_signature: false, lying_residue: false, recovered: Vec::new(), resets: 0 }
	}

	fn check_condition(&mut self, key: u8, asc: u8) {
		self.status = 1;
		self.sense = [0; 18];
		self.sense[0] = 0x70;
		self.sense[2] = key;
		self.sense[7] = 10;
		self.sense[12] = asc;
	}

	// The command's data stage: what it moves and in which direction.
	fn execute(&mut self, cb: &[u8], len: u32, data_in: bool) -> u32 {
		self.status = 0;
		match cb[0] {
			scsi::TEST_UNIT_READY if self.attention => {
				self.attention = false;
				self.check_condition(6, 0x29);
				0
			}
			scsi::TEST_UNIT_READY => 0,
			scsi::REQUEST_SENSE => {
				let take = (len as usize).min(18);
				self.data[..take].copy_from_slice(&self.sense[..take]);
				take as u32
			}
			scsi::READ_CAPACITY10 => {
				let last = self.sectors.len() as u32 - 1;
				self.data[..4].copy_from_slice(&last.to_be_bytes());
				self.data[4..8].copy_from_slice(&512u32.to_be_bytes());
				8
			}
			scsi::READ10 | scsi::WRITE10 => {
				let lba = u32::from_be_bytes([cb[2], cb[3], cb[4], cb[5]]) as usize;
				let count = u16::from_be_bytes([cb[7], cb[8]]) as usize;
				for index in 0..count {
					let at = index * 512;
					if data_in {
						self.data[at..at + 512].copy_from_slice(&self.sectors[lba + index]);
					} else {
						self.sectors[lba + index].copy_from_slice(&self.data[at..at + 512]);
					}
				}
				(count * 512) as u32
			}
			scsi::SYNCHRONIZE_CACHE10 => 0,
			_ => {
				self.check_condition(5, 0x20);
				0
			}
		}
	}
}

impl Transport for Disk {
	fn bulk(&mut self, dir_in: bool, at: At, len: u32) -> Moved {
		match at {
			At::Wrapper(0) => {
				assert!(!dir_in && len == 31, "a CBW is 31 bytes OUT");
				assert_eq!(&self.wrapper[0..4], b"USBC");
				let tag = u32::from_le_bytes(self.wrapper[4..8].try_into().unwrap());
				let data_len = u32::from_le_bytes(self.wrapper[8..12].try_into().unwrap());
				let data_in = self.wrapper[12] & 0x80 != 0;
				let cb = self.wrapper[15..15 + self.wrapper[14] as usize].to_vec();
				self.pending = Some((tag, cb, data_len, data_in));
				self.moved = 0;
				Moved::Done(31)
			}
			At::Data => {
				let (tag, cb, data_len, data_in) = self.pending.clone().expect("a data stage follows a CBW");
				assert_eq!(dir_in, data_in, "the data stage runs the way the CBW said");
				assert_eq!(len, data_len);
				if self.stall_next_data {
					self.stall_next_data = false;
					self.stalled_in = dir_in;
					self.check_condition(4, 0x44);
					self.residue = data_len;
					self.pending = Some((tag, cb, data_len, data_in));
					return Moved::Stall(0);
				}
				let moved = self.execute(&cb, len, data_in);
				self.moved = moved;
				self.residue = data_len - moved;
				Moved::Done(moved)
			}
			At::Wrapper(offset) => {
				assert!(dir_in && offset == CSW_OFF && len == 13, "the CSW is 13 bytes IN at the second slot");
				let (tag, cb, data_len, data_in) = self.pending.take().expect("a CSW closes a CBW");
				if data_len == 0 {
					let _ = self.execute(&cb, 0, data_in);
				}
				let at = offset as usize;
				self.wrapper[at..at + 4].copy_from_slice(if self.bad_signature { b"USBX" } else { b"USBS" });
				self.wrapper[at + 4..at + 8].copy_from_slice(&tag.to_le_bytes());
				let residue = if self.lying_residue { data_len + 1 } else { self.residue };
				self.wrapper[at + 8..at + 12].copy_from_slice(&residue.to_le_bytes());
				self.wrapper[at + 12] = self.status;
				self.residue = 0;
				Moved::Done(13)
			}
		}
	}

	fn recover(&mut self, dir_in: bool) {
		self.recovered.push(dir_in);
	}

	fn class_reset(&mut self, iface: u16) {
		assert_eq!(iface, 0);
		self.resets += 1;
	}

	fn wrapper(&mut self) -> &mut [u8] {
		&mut self.wrapper
	}

	fn data(&mut self, bytes: u64) -> Option<&mut [u8]> {
		if bytes > 64 * 1024 {
			return None;
		}
		if self.data.len() < bytes as usize {
			self.data.resize(bytes as usize, 0);
		}
		Some(&mut self.data)
	}

	fn max_transfer(&self) -> u32 {
		64 * 1024
	}
}

#[test]
// THE UNIT IS STARTED THROUGH ITS POWER-ON UNIT ATTENTION: the first TEST UNIT READY fails, the sense read clears it,
// the second passes, and READ CAPACITY gives the size - one more sector than the last LBA it reports.
fn a_unit_is_started_through_its_unit_attention_and_sized_by_read_capacity() {
	let mut disk = Disk::new(2048);
	let mut unit = Unit::new(0);
	assert!(start(&mut disk, &mut unit));
	assert_eq!(unit.capacity, 2048 * 512);
	assert!(!disk.attention, "the attention was taken");
	assert_eq!(disk.resets, 0, "and a refused command is not a reset");
}

#[test]
// SECTORS GO OUT AND COME BACK through the data buffer, each transaction under a tag of its own.
fn sectors_written_are_read_back() {
	let mut disk = Disk::new(64);
	let mut unit = Unit::new(0);
	assert!(start(&mut disk, &mut unit));
	let pattern: Vec<u8> = (0..1024u32).map(|at| (at * 7 + 3) as u8).collect();
	disk.data(1024).unwrap()[..1024].copy_from_slice(&pattern);
	let tag = unit.tag;
	assert!(command(&mut disk, &mut unit, &scsi::read_write10(true, 5, 2), 1024, false));
	assert_eq!(unit.tag, tag + 1, "one transaction, one tag");
	disk.data(1024).unwrap()[..1024].fill(0);
	assert!(command(&mut disk, &mut unit, &scsi::read_write10(false, 5, 2), 1024, true));
	assert_eq!(&disk.data(1024).unwrap()[..1024], &pattern[..]);
	assert_eq!(disk.sectors[6][..], pattern[512..], "the second sector landed where it was sent");
}

#[test]
// A STALLED DATA STAGE IS ROUTINE: the endpoint it stalled on is unhalted and the CSW is still read, which says the
// command failed - and the transport is not reset, since a refusal is not lost sync.
fn a_stalled_data_stage_is_unhalted_and_the_csw_still_decides() {
	let mut disk = Disk::new(64);
	let mut unit = Unit::new(0);
	assert!(start(&mut disk, &mut unit));
	disk.stall_next_data = true;
	assert!(!command(&mut disk, &mut unit, &scsi::read_write10(false, 0, 1), 512, true));
	assert_eq!(disk.recovered, [true], "the IN endpoint it stalled on was unhalted, once");
	assert_eq!(disk.resets, 0, "and the transport kept its sync");
	assert_eq!(read_sense(&mut disk, &mut unit)[2], 4, "the sense says why");
}

#[test]
// A WRAPPER THAT IS NOT ONE MEANS THE TRANSPORT LOST SYNC, and so does a residue larger than the command asked for,
// which describes a transfer that never happened: both reset it - the class request and both endpoints unhalted.
fn a_malformed_wrapper_or_a_false_residue_resets_the_transport() {
	let mut disk = Disk::new(64);
	let mut unit = Unit::new(0);
	assert!(start(&mut disk, &mut unit));
	disk.bad_signature = true;
	assert_eq!(transfer(&mut disk, &mut unit, &scsi::test_unit_ready(), 0, false, false), Err(BotFault::Framing));
	assert_eq!((disk.resets, disk.recovered.clone()), (1, vec![true, false]));
	disk.bad_signature = false;
	disk.lying_residue = true;
	assert_eq!(transfer(&mut disk, &mut unit, &scsi::read_capacity10(), 8, true, false), Err(BotFault::Residue));
	assert_eq!(disk.resets, 2, "and that is lost sync too");
}

#[test]
// THE SENSE READ TAKES A SHORT ANSWER, and only the sense read.
fn only_the_sense_read_is_content_with_a_short_answer() {
	let mut disk = Disk::new(64);
	let mut unit = Unit::new(0);
	assert!(start(&mut disk, &mut unit));
	assert_eq!(transfer(&mut disk, &mut unit, &scsi::read_capacity10(), 16, true, true), Ok(8), "eight bytes of sixteen, accepted when a short answer is one");
	assert_eq!(transfer(&mut disk, &mut unit, &scsi::read_capacity10(), 16, true, false), Err(BotFault::Short { moved: 8, wanted: 16 }), "and refused when it is not");
	assert_eq!(disk.resets, 0, "a short answer is a refusal, not lost sync");
}

#[test]
// THE INTERFACE IS FOUND BY CLASS, SUBCLASS AND PROTOCOL, with its first bulk endpoint of each direction - and a
// UAS interface, or the interrupt endpoint of a CBI one, is not a Bulk-Only pair.
fn the_bulk_only_interface_and_its_pair_are_found_by_class_and_protocol() {
	let mut config = vec![9u8, 0x02, 0, 0, 2, 1, 0, 0x80, 50];
	config.extend_from_slice(&[9, 0x04, 0, 0, 3, 8, 6, 0x62, 0]); // UAS
	config.extend_from_slice(&[7, 0x05, 0x81, 0x02, 0x00, 0x02, 0]);
	config.extend_from_slice(&[7, 0x05, 0x02, 0x02, 0x00, 0x02, 0]);
	config.extend_from_slice(&[9, 0x04, 1, 0, 3, 8, 6, 0x50, 0]); // Bulk-Only
	config.extend_from_slice(&[7, 0x05, 0x83, 0x03, 8, 0, 10]);
	config.extend_from_slice(&[7, 0x05, 0x84, 0x02, 0x40, 0, 0]);
	config.extend_from_slice(&[7, 0x05, 0x05, 0x02, 0x40, 0, 0]);
	let total = config.len() as u16;
	config[2..4].copy_from_slice(&total.to_le_bytes());
	assert_eq!(find(&config), Some(Interface { iface: 1, ep_in: Bulk { address: 0x84, max_packet: 64 }, ep_out: Bulk { address: 0x05, max_packet: 64 } }));
	assert_eq!(find(&config[..9 + 9 + 14]), None, "a UAS interface alone is not one");
}
