// WHAT A DECODER OWES A HOSTILE MESSAGE, WHICH IS TO REFUSE IT.
//
// THE FIRST THING A MESSAGE FROM A CLIENT REACHES IS A GENERATED DECODER, before any service code
// runs and before any check a service author wrote. What it owes is that every byte string is either
// a value or a refusal, and never a panic: a decoder that panicked would be a denial of service any
// client could perform with a short message and no privilege at all.
//
// SWEPT RATHER THAN FUZZED, because a deterministic sweep is what a build gate can run: the same
// inputs every time, in a second, with no corpus to carry. What is swept is the shape a hostile
// message actually has - short, with boundary bytes in the places a length or a tag is read from -
// rather than a large random space that mostly exercises the same early refusal.

use crate::codec::Handles;
use crate::generated::liber::base::v1::Error;
use crate::generated::liber::display::v1::{AcquiredImage, DamageRegion, DisplayResources, FrameTiming, ImageLimits, OutputColour, OutputTransform, PresentComplete, PresentOutcome, PresentQueue, PresentationStats, ScaleRatio, SubpixelLayout, SurfaceConfiguration, SurfaceEvent, SurfaceRequest, TimestampEvidence, display, display_admin, display_stats, surface};
use alloc::vec::Vec;

// The bytes a decoder is most likely to get wrong, in the positions it reads a tag or a length from.
const EDGES: [u8; 6] = [0x00, 0x01, 0x02, 0x7f, 0x80, 0xff];

#[test]
fn every_display_record_refuses_arbitrary_bytes_without_panicking() {
	let mut buf = [0u8; 4];
	for a in 0..=255u8 {
		buf[0] = a;
		for b in EDGES {
			buf[1] = b;
			for c in EDGES {
				buf[2] = c;
				for d in [0u8, 0xff] {
					buf[3] = d;
					for len in 0..=4usize {
						let bytes = &buf[..len];
						let _ = ScaleRatio::decode(bytes);
						let _ = OutputTransform::decode(bytes);
						let _ = SubpixelLayout::decode(bytes);
						let _ = OutputColour::decode(bytes);
						let _ = SurfaceConfiguration::decode(bytes);
						let _ = TimestampEvidence::decode(bytes);
						let _ = PresentOutcome::decode(bytes);
						let _ = FrameTiming::decode(bytes);
						let _ = PresentComplete::decode(bytes);
						let _ = SurfaceEvent::decode(bytes);
						let _ = AcquiredImage::decode(bytes);
						let _ = ImageLimits::decode(bytes);
						let _ = SurfaceRequest::decode(bytes);
						let _ = PresentationStats::decode(bytes);
						let _ = DisplayResources::decode(bytes);
						let _ = DamageRegion::decode(bytes);
						// The one record that carries capabilities, decoded the way a message
						// carrying none would be: a reader with an EMPTY handle list, which is
						// exactly what a client that sent the bytes and kept the handles produces.
						let mut empty = Handles::new();
						let _ = PresentQueue::decode_message(bytes, &mut empty);
					}
				}
			}
		}
	}
}

// A service that answers every call with a refusal.
//
// WHAT IS UNDER TEST IS THE DISPATCH AND NOT THE SERVICE, so this one does nothing a decoder could
// be blamed for: every handler is reached only by a request that decoded, and what it returns is the
// same thing whatever the request said.
struct Refuser;

impl surface::Service for Refuser {
	fn configuration(&mut self) -> Result<SurfaceConfiguration, Error> {
		Err(Error::Invalid)
	}

	fn ack_configure(&mut self, _serial: u64) -> Result<(), Error> {
		Err(Error::Invalid)
	}

	fn queue(&mut self) -> Result<PresentQueue, Error> {
		Err(Error::Invalid)
	}

	fn acquire_next(&mut self) -> Result<AcquiredImage, Error> {
		Err(Error::Invalid)
	}

	fn abandon(&mut self, _image: u32) -> Result<(), Error> {
		Err(Error::Invalid)
	}

	fn present(&mut self, _image: u32, _serial: u64, _generation: u64, _damage: DamageRegion) -> Result<u64, Error> {
		Err(Error::Invalid)
	}

	fn events(&mut self) -> Vec<SurfaceEvent> {
		Vec::new()
	}

	fn input_focus(&mut self) -> Result<u64, Error> {
		Err(Error::Invalid)
	}

	fn close(&mut self) -> Result<(), Error> {
		Err(Error::Invalid)
	}

	fn provide_image(&mut self, _index: u32, _image: u64) -> Result<(), Error> {
		Err(Error::Invalid)
	}
}

impl display::Service for Refuser {
	fn create_surface(&mut self, _request: SurfaceRequest) -> Result<u64, Error> {
		Err(Error::Invalid)
	}

	fn image_limits(&mut self) -> Result<ImageLimits, Error> {
		Err(Error::Invalid)
	}
}

impl display_admin::Service for Refuser {
	fn bind(&mut self, _task: u64) -> Result<u64, Error> {
		Err(Error::Invalid)
	}

	fn stats(&mut self) -> PresentationStats {
		PresentationStats { presents: 0, direct_presents: 0, scaled_presents: 0, source_pixels: 0, output_pixels: 0, blit_ns: 0, flush_ns: 0, max_present_ns: 0, displayed: 0, discarded: 0, replaced: 0, lost: 0 }
	}

	fn set_visible(&mut self, _task: u64, _surface: u64) -> Result<(), Error> {
		Err(Error::Unsupported)
	}
}

impl display_stats::Service for Refuser {
	fn resources(&mut self) -> DisplayResources {
		DisplayResources { surfaces: 0, surface_bound: 0, present_images: 0, image_bound: 0, queued_presents: 0, present_bound: 0, damage_entries: 0, damage_bound: 0, waiters: 0, waiter_bound: 0, resets: 0, faulted: false }
	}
}

#[test]
fn every_display_dispatch_refuses_arbitrary_requests_without_panicking() {
	// A request is `[op][corr][args...]`, so the sweep walks every op this package defines plus a
	// few that are not ops at all, and fills the argument space with the bytes a length or a tag is
	// most likely to be misread from. Twelve bytes is past the point where every op has either
	// decoded or refused.
	let mut request = [0u8; 12];
	let mut out = [0u8; 256];
	for op in 0..=16u16 {
		request[..2].copy_from_slice(&op.to_le_bytes());
		for filler in EDGES {
			request[2..].fill(filler);
			for len in 0..=12usize {
				let bytes = &request[..len];
				// A HANDLE LIST THAT IS EMPTY AND ONE THAT IS NOT, because a parameter written as a
				// capability reads from the list and a request that carried none is the ordinary
				// hostile shape: the bytes say a handle is there and nothing was sent.
				for handles in [Handles::new(), Handles::try_from_slice(&[7]).expect("one stand-in handle")] {
					for dispatch in 0..4u8 {
						let mut service = Refuser;
						let mut taken = handles.clone();
						let mut reply = Handles::new();
						let _ = match dispatch {
							0 => surface::dispatch(&mut service, bytes, &mut taken, &mut out, &mut reply),
							1 => display::dispatch(&mut service, bytes, &mut taken, &mut out, &mut reply),
							2 => display_admin::dispatch(&mut service, bytes, &mut taken, &mut out, &mut reply),
							_ => display_stats::dispatch(&mut service, bytes, &mut taken, &mut out, &mut reply),
						};
					}
				}
			}
		}
	}
}

// A REQUEST THAT CARRIES A CAPABILITY ITS SIGNATURE DOES NOT NAME.
//
// THE DEFECT THIS IS ABOUT IS NOT THE REFUSAL, IT IS WHAT HAPPENS TO THE HANDLE. A dispatch that
// decoded the bytes it understood and ignored the rest would leave a live capability in nobody's
// hands and nobody's list: not refused, not closed, and still charged to the sender's Domain for the
// life of the process. One per request, from any client, with no privilege at all.
//
// `Reader::finish` is where it is caught, because it answers BOTH halves of "this message is over" -
// the bytes AND the handles - and every generated op calls it. What reaches the service is a request
// whose signature accounts for everything it carried, and what is left in the caller's list is
// exactly what the serve loop then closes.
#[test]
fn a_request_carrying_a_capability_its_signature_does_not_name_is_refused() {
	let mut out = [0u8; 256];
	// `ack-configure` takes a serial and NO capability, written exactly as the generated client
	// writes it: the op, the correlation, the argument.
	let mut request = surface::OP_ACK_CONFIGURE.to_le_bytes().to_vec();
	request.extend_from_slice(&7u32.to_le_bytes());
	request.extend_from_slice(&1u64.to_le_bytes());

	// First without one, so the fixture is measuring the handle and not the bytes.
	let mut service = Refuser;
	let mut none = Handles::new();
	let mut reply = Handles::new();
	assert!(surface::dispatch(&mut service, &request, &mut none, &mut out, &mut reply).is_some(), "the request itself is well formed");

	// And now with one attached. The service is NOT reached, and the capability is still in the
	// caller's list - which is where the serve loop's own sweep closes it.
	let mut service = Refuser;
	let mut carried = Handles::try_from_slice(&[0x4242]).expect("one handle fits");
	let mut reply = Handles::new();
	assert!(surface::dispatch(&mut service, &request, &mut carried, &mut out, &mut reply).is_none(), "a capability the signature does not name is a refusal");
	assert_eq!(carried.as_slice(), &[0x4242], "and the handle is left for the caller to close rather than dropped");
	assert!(reply.is_empty(), "a refused request hands nothing back");
}
