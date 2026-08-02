// Build a LiberFS system volume image on the host.
//
// Until now a LiberFS volume only ever existed at runtime: the system disk carried a factory
// ARCHIVE at LBA 0, and the storage service formatted a volume and seeded it from that archive
// after boot. That is why the loader had nothing to read - at the moment the loader runs, there
// is no filesystem on the disk, only a package.
//
// This makes the volume a build artifact instead. It uses the same `liberfs` crate the storage
// service and the loader use, so an image built here and a volume formatted on the running
// system are the same format because they are the same code - not because two implementations
// were kept in step by hand.
//
// Usage: mkvolume <image> <size-bytes> <src>:<dest> [<src>:<dest> ...]

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::process::ExitCode;

use fscore::BlockDevice;
use liberfs::{FormatOpts, LiberFs};

// The whole image, held in memory while it is built and written out once. A system volume is
// tens of megabytes, which is nothing on a build host, and it keeps the block device trivial.
struct Image {
	bytes: Vec<u8>,
	block: usize,
}

impl BlockDevice for Image {
	fn read_block(&mut self, index: u64, buf: &mut [u8]) -> bool {
		let start = index as usize * self.block;
		let Some(src) = self.bytes.get(start..start + buf.len()) else { return false };
		buf.copy_from_slice(src);
		true
	}

	fn write_block(&mut self, index: u64, buf: &[u8]) -> bool {
		let start = index as usize * self.block;
		let Some(dst) = self.bytes.get_mut(start..start + buf.len()) else { return false };
		dst.copy_from_slice(buf);
		true
	}
}

fn main() -> ExitCode {
	let args: Vec<String> = std::env::args().skip(1).collect();
	if args.len() < 2 {
		eprintln!("mkvolume: usage: mkvolume <image> <size-bytes> <src>:<dest> [<src>:<dest> ...]");
		return ExitCode::FAILURE;
	}
	let image_path = &args[0];
	let Ok(size) = args[1].parse::<usize>() else {
		eprintln!("mkvolume: size must be a byte count, got {:?}", args[1]);
		return ExitCode::FAILURE;
	};

	// The block size LiberFS formats with. Reading it back through a 512-byte firmware disk is
	// eight device reads per filesystem block, which the BlockDevice trait already loops for.
	const BLOCK: usize = 4096;
	if size % BLOCK != 0 || size == 0 {
		eprintln!("mkvolume: size must be a non-zero multiple of {BLOCK}, got {size}");
		return ExitCode::FAILURE;
	}

	let blocks = (size / BLOCK) as u64;
	let image = Image { bytes: vec![0u8; size], block: BLOCK };
	let opts = FormatOpts { uuid: *b"libersystem-vol\0", label: b"system".to_vec(), compress: false };
	let mut fs = match LiberFs::format_opts(image, blocks, opts) {
		Ok(fs) => fs,
		Err(error) => {
			eprintln!("mkvolume: cannot format a {size}-byte volume: {error:?}");
			return ExitCode::FAILURE;
		}
	};

	// Every parent directory is created once, in path order, so a staging list does not have to
	// name its directories or be sorted by the caller.
	let mut made: BTreeSet<String> = BTreeSet::new();
	let mut staged = 0usize;
	for entry in &args[2..] {
		let Some((src, dest)) = entry.split_once(':') else {
			eprintln!("mkvolume: expected <src>:<dest>, got {entry:?}");
			return ExitCode::FAILURE;
		};
		let bytes = match fs::read(src) {
			Ok(bytes) => bytes,
			Err(error) => {
				eprintln!("mkvolume: cannot read {src}: {error}");
				return ExitCode::FAILURE;
			}
		};
		let dest = dest.trim_start_matches('/');
		let mut prefix = String::new();
		if let Some((dirs, _)) = dest.rsplit_once('/') {
			for segment in dirs.split('/') {
				if !prefix.is_empty() {
					prefix.push('/');
				}
				prefix.push_str(segment);
				if made.insert(prefix.clone())
					&& let Err(error) = fs.mkdir(prefix.as_bytes())
				{
					eprintln!("mkvolume: cannot create {prefix}: {error:?}");
					return ExitCode::FAILURE;
				}
			}
		}
		if let Err(error) = fs.write_file(dest.as_bytes(), &bytes) {
			eprintln!("mkvolume: cannot write {dest} ({} bytes): {error:?}", bytes.len());
			return ExitCode::FAILURE;
		}
		staged += 1;
	}

	// Prove the image is readable through the same path the loader will take, before it is
	// written out. A volume that formats but does not mount is worth catching on the build host
	// rather than on a machine that will not boot.
	let image = fs.into_device();
	let mut check = LiberFs::mount(Image { bytes: image.bytes.clone(), block: BLOCK }).is_some();
	for entry in &args[2..] {
		if !check {
			break;
		}
		let Some((_, dest)) = entry.split_once(':') else { continue };
		let mut fs = LiberFs::mount(Image { bytes: image.bytes.clone(), block: BLOCK }).expect("mounted above");
		check = fs.read_file(dest.trim_start_matches('/').as_bytes()).is_ok();
		if !check {
			eprintln!("mkvolume: {dest} was written but cannot be read back");
		}
	}
	if !check {
		eprintln!("mkvolume: the image does not mount; refusing to write it");
		return ExitCode::FAILURE;
	}

	let mut out = match fs::File::create(image_path) {
		Ok(out) => out,
		Err(error) => {
			eprintln!("mkvolume: cannot create {image_path}: {error}");
			return ExitCode::FAILURE;
		}
	};
	if let Err(error) = out.write_all(&image.bytes) {
		eprintln!("mkvolume: cannot write {image_path}: {error}");
		return ExitCode::FAILURE;
	}
	println!("mkvolume: wrote {image_path} ({size} bytes, {staged} files)");
	ExitCode::SUCCESS
}
