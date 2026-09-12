// lsfont - list the installed faces, and the one privileged font operation.
//
// TWO CAPABILITIES, HELD APART, and the separation is the whole authority argument. `font-catalogue`
// is the READ - what faces are installed and what they declare - and every text client holds it.
// `font-admin` is the RESCAN, and it exists for recovery after a dropped watch hint: a full
// directory read, digest and metadata pass is real work, so authority to READ a font must not become
// authority to make the machine work. Nothing that renders a font list needs the second, and holding
// the first gets a component no closer to it.
//
// IT IS A DELIVERABLE RATHER THAN A CONVENIENCE. The catalogue's recovery scan is issued from here
// and from nowhere else: a build without this tool is a build in which recovery cannot be issued at
// all, which is why the tool is named in the milestone rather than left to whoever wanted one.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use font_client::{FontAdminClient, FontClient};
use proto::codec::JsonMode;
use proto::generated::liber::font::v1 as font;
use proto::system::LaunchContext;
use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	// 1. adopt the forwarded stdout console, so our output renders on the terminal of the shell
	//    that launched us.
	inherit_stdout(bootstrap);
	// 2. the argument string: the sub-form, or `--rescan`.
	let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
		Some(context) => context,
		None => exit(),
	};
	let args: Vec<u8> = context.arguments.clone().into_bytes();
	// 3. the two capabilities, in the order the manifest grants them. The second is OPTIONAL: a boot
	//    that granted no operator path has none, and saying so is better than refusing to list.
	let catalogue: u64 = recv_tagged(bootstrap, &mut buf, b"FONT").unwrap_or_else(|| exit());
	let admin: u64 = recv_tagged(bootstrap, &mut buf, b"FONTADMIN").unwrap_or(0);

	if args.starts_with(b"--rescan") {
		rescan(admin);
		exit();
	}
	list_faces(catalogue, JsonMode::parse(&args));
	exit();
}

// The installed faces, as text or as a JSON array rendered on the client side.
fn list_faces(catalogue: u64, mode: Option<JsonMode>) {
	let mut client = FontClient::new(catalogue);
	let faces: Vec<font::FaceRecord> = match client.list() {
		Some(Ok(faces)) => faces,
		Some(Err(error)) => {
			print(format!("lsfont: the catalogue refused the listing ({error:?})\n").as_bytes());
			return;
		}
		None => {
			print(b"lsfont: no answer from the font catalogue\n");
			return;
		}
	};
	if mode.is_none() {
		if faces.is_empty() {
			print(b"no faces are installed\n");
			return;
		}
		for face in &faces {
			print(format!("{:<28} {:<16} {:>4} {:<16} {:<16} {}\n", face.family, face.style, face.weight, width_name(face.width), slant_name(face.slant), format_name(face.format)).as_bytes());
		}
		return;
	}
	let mut out = String::from("[");
	for (index, face) in faces.iter().enumerate() {
		if index > 0 {
			out.push(',');
		}
		out.push_str(&format!("{{\"family\":\"{}\",\"style\":\"{}\",\"weight\":{},\"width\":\"{}\",\"slant\":\"{}\",\"format\":\"{}\",\"index\":{},\"identity\":\"{}\"}}", face.family, face.style, face.weight, width_name(face.width), slant_name(face.slant), format_name(face.format), face.identity.index, hex(&face.identity.digest)));
	}
	out.push_str("]\n");
	print(out.as_bytes());
}

// The operator's recovery scan, and its answer rendered as what it is: a scan that RAN and published
// nothing is not a failure, and one refused by the delay is not an error either.
fn rescan(admin: u64) {
	if admin == 0 {
		print(b"lsfont: this boot granted no font-administration authority\n");
		return;
	}
	let mut client = FontAdminClient::new(admin);
	match client.rescan() {
		Some(Ok(font::RescanOutcome::Completed(report))) => {
			let verb = if report.advanced { "published" } else { "found nothing different" };
			print(format!("lsfont: the scan {verb} at generation {} - {} face(s)\n", report.generation, report.published_faces).as_bytes());
			for name in &report.withdrawn {
				print(format!("lsfont:   withdrawn: {name}\n").as_bytes());
			}
			for rejection in &report.rejected {
				print(format!("lsfont:   rejected: {} ({:?})\n", rejection.name, rejection.reason).as_bytes());
			}
			if !matches!(report.breach, font::Ceiling::None) {
				print(format!("lsfont:   nothing new was published: {:?} was exceeded\n", report.breach).as_bytes());
			}
		}
		Some(Ok(font::RescanOutcome::TryAgainAt(at))) => {
			print(format!("lsfont: a scan completed recently; the next may start at tick {at}\n").as_bytes());
		}
		Some(Ok(font::RescanOutcome::InFlight(()))) => {
			print(b"lsfont: a scan is already running\n");
		}
		Some(Err(error)) => print(format!("lsfont: the catalogue refused the scan ({error:?})\n").as_bytes()),
		None => print(b"lsfont: no answer from the font catalogue\n"),
	}
}

fn width_name(width: font::FaceWidth) -> &'static str {
	match width {
		font::FaceWidth::UltraCondensed => "ultra-condensed",
		font::FaceWidth::ExtraCondensed => "extra-condensed",
		font::FaceWidth::Condensed => "condensed",
		font::FaceWidth::SemiCondensed => "semi-condensed",
		font::FaceWidth::Normal => "normal",
		font::FaceWidth::SemiExpanded => "semi-expanded",
		font::FaceWidth::Expanded => "expanded",
		font::FaceWidth::ExtraExpanded => "extra-expanded",
		font::FaceWidth::UltraExpanded => "ultra-expanded",
	}
}

fn slant_name(slant: font::FaceSlant) -> &'static str {
	match slant {
		font::FaceSlant::Upright => "upright",
		font::FaceSlant::Italic => "italic",
		font::FaceSlant::Oblique => "oblique",
	}
}

fn format_name(format: font::FaceFormat) -> &'static str {
	match format {
		font::FaceFormat::TruetypeGlyf => "truetype-glyf",
		font::FaceFormat::OpentypeCff => "opentype-cff",
		font::FaceFormat::OpentypeCff2 => "opentype-cff2",
		font::FaceFormat::Collection => "collection",
	}
}

// The identity as a person compares it: lower-case hexadecimal.
fn hex(digest: &[u8]) -> String {
	let mut out = String::with_capacity(digest.len() * 2);
	for byte in digest {
		out.push_str(&format!("{byte:02x}"));
	}
	out
}
