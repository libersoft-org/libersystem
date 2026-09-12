// FontCatalogue - the installed-face catalogue.
//
// WHAT IS SERVICE-OWNED AND WHY ONLY THIS. Deterministic fallback has to enumerate the same set for
// every client, and a per-process view of "what is installed" is not a set - so the CATALOGUE is a
// service. Parsing, shaping and every glyph and shaping cache stay IN-PROCESS, in the calling
// application, where their allocations are charged to the caller's own Domain and a pathological
// document harms only the process that asked for it. This program therefore holds metadata and
// identities, never decoded faces and never caches, which is what makes its state bounded by a
// computable number.
//
// IT PARSES NO FONT. Family, style, axes, face index and format come out of OpenType bytes, and a
// parser here would be a second, unprofiled, hostile-input parser written before the item that
// authorises one. It reads a DECLARATION staged beside each face, checks it against the closed
// vocabularies, and digests the face to check the declaration names the bytes beside it. Whether the
// declaration is TRUE is checked by the profiled parser, elsewhere, and this program does not claim
// otherwise.
//
// ITS AUTHORITY IS ONE READ-ONLY DIRECTORY. ServiceManager holds StorageService's admin endpoint and
// mints a client scoped to the font destination with `writable: false`; a mint that fails is a
// start-up refusal, because a catalogue that cannot read a face is not a degraded mode to run in. It
// cannot refuse ambient authority on behalf of its clients while taking it for itself.
//
// THE OPERATOR'S RESCAN IS NOT ON THE CLIENT INTERFACE. The normal trigger for re-reading is the
// volume watch; an explicit scan exists only as recovery after a dropped hint, and it arrives on the
// seeded ADMIN root whose channel value this program knows. Authority to READ a font must not become
// authority to make the machine work.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use ipc_client::ChannelTransport;
use proto::generated::liber::font::v1 as font;
use proto::system::{Error, OpenOpts, volume};
use rt::*;
use service_logic::font_clients::Endpoints;
use service_logic::font_record::{self, FONT_DIRECTORY, MAX_FACE_METADATA_BYTES, RECORD_SUFFIX};
use service_logic::font_rescan::{Allowance, Completion};
use service_logic::font_scan::{self, Observation, Publication, PublishedFace};

include!(concat!(env!("OUT_DIR"), "/roles_font_catalogue.rs"));

// How long a completed scan blocks the next one, in monotonic ticks. The clock runs at 100 Hz, so
// this is five seconds: long enough that a loop of recovery scans is not a way to spend the machine,
// short enough that an operator correcting a bad drop is not left waiting on a number.
const RESCAN_DELAY_TICKS: u64 = 500;

// The biggest face this catalogue will serve. A resolve reads the whole file to digest it against
// the identity it is served under, so the read itself has to be bounded - and 16 MiB is the size the
// seam already names as the largest face a resolve hands back.
const MAX_FACE_BYTES: u64 = 16 * 1024 * 1024;

// The catalogue's state. Everything in it is derived from the font directory, which is what makes
// the service restartable: a fresh instance re-reads the directory and is in the same place.
struct Catalogue {
	// The read-only directory client. Zero is not a state this program runs in - see `__user_main`.
	volume: u64,
	published: Publication,
	endpoints: Endpoints,
	// Which endpoint slot a served channel belongs to, so a disconnect gives back the right one.
	slots: Vec<(u64, usize)>,
	// The subscription streams, as producer ends this program keeps: closing one is what tells a
	// consumer the stream ended.
	streams: Vec<(usize, u64)>,
	allowance: Allowance,
	// The seeded ADMIN root's channel value: the identity a privileged request is recognised by. A
	// connection minted on demand from the ordinary root is indistinguishable from every other, which
	// is why the admin endpoint is a root of its own rather than an opcode.
	admin: u64,
	// The channel the request being dispatched arrived on.
	current: u64,
}

impl Catalogue {
	// The face a client named, if it is published.
	fn face_named(&self, identity: &font::FaceIdentity) -> Option<&PublishedFace> {
		self.published.faces.iter().find(|face| face.record.digest[..] == identity.digest[..] && face.record.face_index == identity.index)
	}

	// Read one file through the scoped client, bounded. `None` for a file that is absent, too large
	// or unreadable - all of which are the same answer to a caller that wanted its bytes.
	fn read_file(&self, name: &str, limit: u64) -> Option<Vec<u8>> {
		let mut path = String::from(FONT_DIRECTORY);
		path.push('/');
		path.push_str(name);
		let mut client = volume::Client::new(ChannelTransport { chan: self.volume });
		let opts = OpenOpts { path, write: false, create: false };
		let opened = match client.open(&opts) {
			Some(Ok(result)) if result.file != 0 => result,
			_ => return None,
		};
		if opened.size > limit {
			close(opened.file);
			return None;
		}
		let mapped = match unsafe { map_object(opened.file) } {
			Some(base) => base,
			None => {
				close(opened.file);
				return None;
			}
		};
		// SAFETY: the mapping is live until `unmap_object` below, and `size` is the object's own.
		let bytes = unsafe { core::slice::from_raw_parts(mapped as *const u8, opened.size as usize) }.to_vec();
		unmap_object(opened.file);
		close(opened.file);
		Some(bytes)
	}

	// Read the directory and validate every face in it. The catalogue's whole input.
	fn observe(&self) -> Vec<Observation> {
		let mut client = volume::Client::new(ChannelTransport { chan: self.volume });
		let Some(Ok(consumer)) = client.list(FONT_DIRECTORY) else {
			return Vec::new();
		};
		let Some(entries) = drain_stream_complete(consumer, volume::list_read) else {
			// A SHORT LISTING IS NOT AN EMPTY DIRECTORY. Treating a drain that failed as "no faces"
			// would withdraw every published face because of a transport hiccup, which is exactly
			// what the withdrawal rule must not be reached by.
			return Vec::new();
		};
		let mut observations: Vec<Observation> = Vec::new();
		for entry in &entries {
			if entry.name.ends_with(RECORD_SUFFIX) {
				// A declaration is not a face. An orphan one is not published and names nothing that
				// could be.
				continue;
			}
			let mut declaration_name = entry.name.clone();
			declaration_name.push_str(RECORD_SUFFIX);
			let Some(bytes) = self.read_file(&entry.name, MAX_FACE_BYTES) else {
				observations.push(Observation { name: entry.name.clone(), record: Err(font_record::RecordError::Missing) });
				continue;
			};
			let digest = bootproto::sha256::digest(&bytes);
			// THE DECLARATION IS SMALL BY CONTRACT, and reading it under a bound is what keeps a
			// dropped-in megabyte from being read at all. The encoded record is bounded at 256 bytes;
			// the text that declares it is allowed a comfortable multiple of that and no more.
			let Some(text) = self.read_file(&declaration_name, (MAX_FACE_METADATA_BYTES * 16) as u64) else {
				observations.push(Observation { name: entry.name.clone(), record: Err(font_record::RecordError::Missing) });
				continue;
			};
			let record = match core::str::from_utf8(&text) {
				Ok(text) => font_record::parse(text, &digest),
				Err(_) => Err(font_record::RecordError::Malformed),
			};
			observations.push(Observation { name: entry.name.clone(), record });
		}
		observations
	}

	// Re-read the directory and apply the ordered publish/withdraw rule. Answers what the scan did,
	// so the operator's report and the subscriber notification are the same decision.
	fn rescan(&mut self) -> font_scan::ScanOutcome {
		let observed = self.observe();
		let outcome = font_scan::reconcile(&self.published, &observed);
		if outcome.advanced {
			self.published = outcome.publication.clone();
			self.announce();
		}
		outcome
	}

	// Tell every subscriber the generation moved. A stream whose consumer is gone is dropped here,
	// which is also how a subscriber that vanished without disconnecting gives its slot back.
	fn announce(&mut self) {
		let event = font::GenerationEvent { generation: self.published.generation };
		let mut frame = [0u8; 64];
		let mut handles = wire::Handles::new();
		let mut seq: u32 = 0;
		let mut dead: Vec<usize> = Vec::new();
		for (index, (_, producer)) in self.streams.iter().enumerate() {
			let Some(len) = font::font_catalogue::subscribe_frame(seq, &event, &mut frame, &mut handles) else {
				continue;
			};
			seq = seq.wrapping_add(1);
			if !send_blocking(*producer, &frame[..len], 0) {
				dead.push(index);
			}
		}
		for index in dead.into_iter().rev() {
			let (_, producer) = self.streams.remove(index);
			close(producer);
		}
	}

	// A client went: its place and its subscription come back on the same event.
	fn disconnected(&mut self, chan: u64) {
		let Some(at) = self.slots.iter().position(|(channel, _)| *channel == chan) else {
			return;
		};
		let (_, slot) = self.slots.remove(at);
		self.streams.retain(|(owner, producer)| {
			if *owner == slot {
				close(*producer);
				return false;
			}
			true
		});
		let _ = self.endpoints.disconnect(slot);
	}

	// The slot this channel holds, admitting it on first sight. `None` when the catalogue is holding
	// as many connections as it will.
	fn slot_of(&mut self, chan: u64) -> Option<usize> {
		if let Some((_, slot)) = self.slots.iter().find(|(channel, _)| *channel == chan) {
			return Some(*slot);
		}
		let slot = self.endpoints.connect().ok()?;
		self.slots.push((chan, slot));
		Some(slot)
	}
}

// The published record, as the wire carries it.
fn wire_record(face: &PublishedFace) -> font::FaceRecord {
	font::FaceRecord {
		identity: font::FaceIdentity { digest: face.record.digest.to_vec(), index: face.record.face_index },
		family: face.record.family.clone(),
		style: face.record.style.clone(),
		format: match face.record.format {
			font_record::FaceFormat::TruetypeGlyf => font::FaceFormat::TruetypeGlyf,
			font_record::FaceFormat::OpentypeCff => font::FaceFormat::OpentypeCff,
			font_record::FaceFormat::OpentypeCff2 => font::FaceFormat::OpentypeCff2,
			font_record::FaceFormat::Collection => font::FaceFormat::Collection,
		},
		weight: face.record.weight,
		width: match face.record.width {
			font_record::FaceWidth::UltraCondensed => font::FaceWidth::UltraCondensed,
			font_record::FaceWidth::ExtraCondensed => font::FaceWidth::ExtraCondensed,
			font_record::FaceWidth::Condensed => font::FaceWidth::Condensed,
			font_record::FaceWidth::SemiCondensed => font::FaceWidth::SemiCondensed,
			font_record::FaceWidth::Normal => font::FaceWidth::Normal,
			font_record::FaceWidth::SemiExpanded => font::FaceWidth::SemiExpanded,
			font_record::FaceWidth::Expanded => font::FaceWidth::Expanded,
			font_record::FaceWidth::ExtraExpanded => font::FaceWidth::ExtraExpanded,
			font_record::FaceWidth::UltraExpanded => font::FaceWidth::UltraExpanded,
		},
		slant: match face.record.slant {
			font_record::FaceSlant::Upright => font::FaceSlant::Upright,
			font_record::FaceSlant::Italic => font::FaceSlant::Italic,
			font_record::FaceSlant::Oblique => font::FaceSlant::Oblique,
		},
		axes: face.record.axes.iter().map(|axis| font::VariationAxis { tag: axis.tag, minimum: axis.minimum, default: axis.default, maximum: axis.maximum }).collect(),
	}
}

impl font::font_catalogue::Service for Catalogue {
	fn list(&mut self) -> Result<Vec<font::FaceRecord>, Error> {
		if self.slot_of(self.current).is_none() {
			return Err(Error::Exhausted);
		}
		Ok(self.published.faces.iter().map(wire_record).collect())
	}

	// How long the face is, and which publication that answer is about. It allocates nothing, which
	// is the whole reason resolve is two operations: a memory object is charged to the Domain that
	// CREATED it and stays charged after the handle moves.
	fn resolve_info(&mut self, identity: font::FaceIdentity) -> Result<font::FaceBytes, Error> {
		if self.slot_of(self.current).is_none() {
			return Err(Error::Exhausted);
		}
		let face = self.face_named(&identity).ok_or(Error::NotFound)?;
		let name = face.name.clone();
		let bytes = self.read_file(&name, MAX_FACE_BYTES).ok_or(Error::Io)?;
		Ok(font::FaceBytes { identity, generation: self.published.generation, length: bytes.len() as u64 })
	}

	// Fill the caller's object with the face's bytes.
	//
	// THE HANDLE IS CLOSED BEFORE THE REPLY, on every path. Not retaining it is what the plan's
	// retained-face gate rests on, and it is a code obligation rather than a right the transport
	// could have withheld - `transfer` has to be present for the handle to arrive at all.
	fn resolve_into(&mut self, identity: font::FaceIdentity, generation: u64, target: u64) -> Result<font::ResolveOutcome, Error> {
		let answer = self.fill(&identity, generation, target);
		close(target);
		answer
	}

	// The snapshot half of a subscription. The LIVE half cannot be served from here - the producer
	// end has to outlive the reply - so the op is intercepted before the generated dispatch and this
	// is what answers if it ever is not.
	fn subscribe(&mut self) -> Vec<font::GenerationEvent> {
		alloc::vec![font::GenerationEvent { generation: self.published.generation }]
	}
}

impl Catalogue {
	fn fill(&mut self, identity: &font::FaceIdentity, generation: u64, target: u64) -> Result<font::ResolveOutcome, Error> {
		if self.slot_of(self.current).is_none() {
			return Err(Error::Exhausted);
		}
		// THE GENERATION IS A FENCE AGAINST A REPLACEMENT THIS CATALOGUE HAS SEEN.
		if generation != self.published.generation {
			return Ok(font::ResolveOutcome::Stale(self.published.generation));
		}
		let face = self.face_named(identity).ok_or(Error::NotFound)?;
		let name = face.name.clone();
		let published_digest = face.record.digest;
		let bytes = self.read_file(&name, MAX_FACE_BYTES).ok_or(Error::Io)?;
		// AND THE BYTES ARE CHECKED AGAINST THE IDENTITY THEY WOULD BE SERVED UNDER, which is the
		// case the generation cannot reach: the watch is a HINT and its events can be dropped, so an
		// admitted read under a still-current generation could otherwise return a replacement's bytes
		// under the previous content identity. A client keying decoded data on that identity would
		// then hold two different faces under one name.
		if bootproto::sha256::digest(&bytes) != published_digest {
			// Rescan immediately rather than waiting for a hint that may never arrive.
			let _ = self.rescan();
			return Ok(font::ResolveOutcome::Stale(self.published.generation));
		}
		let info = object_info(target).ok_or(Error::Invalid)?;
		if info.size < bytes.len() as u64 {
			return Ok(font::ResolveOutcome::TooSmall(bytes.len() as u64));
		}
		let mapped = match unsafe { map_object(target) } {
			Some(base) => base,
			None => return Err(Error::Invalid),
		};
		// SAFETY: the object is at least `bytes.len()` long - checked above - and the mapping is live
		// until the unmap below. The handle carries `map` and `write`, which the generated dispatch
		// refused it without.
		unsafe {
			core::ptr::copy_nonoverlapping(bytes.as_ptr(), mapped as *mut u8, bytes.len());
		}
		unmap_object(target);
		Ok(font::ResolveOutcome::Filled(bytes.len() as u64))
	}
}

// The admin interface, which is the same program answering on a channel it can recognise.
impl font::font_catalogue_admin::Service for Catalogue {
	fn rescan(&mut self) -> Result<font::RescanOutcome, Error> {
		// A REQUEST THAT DID NOT ARRIVE ON THE ADMIN ROOT IS NOT AN ADMIN REQUEST. Ordinary
		// connections are minted from the other root and can never carry this channel value.
		if self.admin == 0 || self.current != self.admin {
			return Err(Error::Denied);
		}
		let now = clock();
		match self.allowance.request(now, self.published.generation) {
			Ok(()) => {}
			Err(service_logic::font_rescan::Refusal::TryAgainAt(at)) => return Ok(font::RescanOutcome::TryAgainAt(at)),
			Err(service_logic::font_rescan::Refusal::InFlight) => return Ok(font::RescanOutcome::InFlight(())),
			Err(service_logic::font_rescan::Refusal::AlreadyPublished) => return Err(Error::Again),
		}
		let previous = self.published.generation;
		let outcome = self.rescan();
		let completion = if outcome.advanced {
			Completion::Published { replaced: previous }
		} else if outcome.breach.is_some() {
			Completion::Failed
		} else {
			Completion::Unchanged
		};
		self.allowance.completed(clock(), completion);
		Ok(font::RescanOutcome::Completed(report_of(&outcome)))
	}
}

// What a scan did, in the vocabulary the report is read in.
fn report_of(outcome: &font_scan::ScanOutcome) -> font::ScanReport {
	font::ScanReport {
		generation: outcome.publication.generation,
		advanced: outcome.advanced,
		published_faces: outcome.publication.faces.len() as u32,
		withdrawn: outcome.withdrawn.clone(),
		rejected: outcome
			.rejected
			.iter()
			.map(|rejection| font::Rejection {
				name: rejection.name.clone(),
				reason: match rejection.reason {
					font_scan::RejectionReason::Declaration(_) => font::RejectionReason::Declaration,
					font_scan::RejectionReason::Relabelled => font::RejectionReason::Relabelled,
					font_scan::RejectionReason::PastCeiling(_) => font::RejectionReason::PastCeiling,
				},
			})
			.collect(),
		breach: match outcome.breach {
			None => font::Ceiling::None,
			Some(font_scan::Ceiling::InstalledFaces) => font::Ceiling::InstalledFaces,
			Some(font_scan::Ceiling::FaceMetadataBytes) => font::Ceiling::FaceMetadataBytes,
			Some(font_scan::Ceiling::ListReplyBytes) => font::Ceiling::ListReplyBytes,
		},
	}
}

// Open one live subscription: the snapshot and the stream, in one operation.
//
// The reply carries the CONSUMER end and this program keeps the producer, so closing it is what
// tells a consumer the stream ended. Intercepted before the generated dispatch for the same reason
// the device catalogue's is: a stream that must outlive its reply cannot be returned by value.
fn open_subscription(catalogue: &mut Catalogue, chan: u64, request: &[u8]) -> bool {
	let Some(slot) = catalogue.slot_of(chan) else {
		return false;
	};
	if catalogue.endpoints.subscribe(slot).is_err() {
		return false;
	}
	let corr: u32 = if request.len() >= 6 { u32::from_le_bytes([request[2], request[3], request[4], request[5]]) } else { return false };
	let Some((producer, consumer)) = channel() else {
		let _ = catalogue.endpoints.disconnect(slot);
		return false;
	};
	// Queue the current generation before releasing the endpoint: a consumer polls as soon as the
	// reply arrives, and a subscription that said nothing until the next change would leave it
	// guessing what it is subscribed to.
	let mut frame = [0u8; 64];
	let mut handles = wire::Handles::new();
	if let Some(len) = font::font_catalogue::subscribe_frame(0, &font::GenerationEvent { generation: catalogue.published.generation }, &mut frame, &mut handles) {
		let _ = send_blocking(producer, &frame[..len], 0);
	}
	catalogue.streams.push((slot, producer));
	if !send_blocking(chan, &corr.to_le_bytes(), consumer) {
		close(consumer);
	}
	true
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	// 1. take the roles the manifest says this service is handed, checked against the GENERATED list
	//    so a wrong bootstrap is refused by the name of the role rather than by a later symptom.
	let mut roles: [u64; BOOTSTRAP_ROLES.len()] = [0; BOOTSTRAP_ROLES.len()];
	if let Err(error) = receive_roles(bootstrap, &BOOTSTRAP_ROLES, &mut roles) {
		fail_bootstrap(bootstrap, error.tag(), error.reason());
	}
	let (fontdir, admin, service): (u64, u64, u64) = (roles[0], roles[1], roles[2]);

	// 2. A MINT THAT FAILED IS A START-UP REFUSAL, reported as such. A font service with no way to
	//    read a face is not a degraded mode to run in: every answer it could give would be a
	//    truthful "nothing installed" that is actually "I was never given the directory".
	if fontdir == 0 {
		fail_bootstrap(bootstrap, b"FONTDIR", b"no read-only client on the font directory");
	}

	let mut catalogue = Catalogue { volume: fontdir, published: Publication::default(), endpoints: Endpoints::new(), slots: Vec::new(), streams: Vec::new(), allowance: Allowance::new(RESCAN_DELAY_TICKS), admin, current: 0 };

	// 3. the scan at start seeds the publication. An EMPTY directory is a real answer - an image that
	//    stages no face has no faces - and it is not the same as the refusal above.
	let seeded = catalogue.rescan();
	if seeded.breach.is_some() {
		print(b"FontCatalogue: the staged font directory exceeds an installation ceiling; nothing is published\n");
	}

	send_blocking(bootstrap, b"FontCatalogue: online", 0);

	// 4. serve. The ADMIN root is SEEDED so its channel value is knowable, which is the identity
	//    `rescan` compares against - a connection minted on demand from the ordinary root is
	//    indistinguishable from every other.
	let seed: [u64; 1] = [admin];
	let seeded_set: &[u64] = if admin != 0 { &seed } else { &[] };
	let mut request: [u8; 1024] = [0u8; 1024];
	let mut reply: [u8; 32768] = [0u8; 32768];
	serve_multi_seeded(service, seeded_set, &mut request, &mut reply, |chan, req, handles, out, reply_handles| -> Option<usize> {
		catalogue.current = chan;
		// A CLIENT THAT WENT TAKES ITS PLACE AND ITS SUBSCRIPTION WITH IT, on the event the serve
		// loop already raises.
		if req.is_empty() {
			catalogue.disconnected(chan);
			return None;
		}
		let op = if req.len() >= 2 { u16::from_le_bytes([req[0], req[1]]) } else { return None };
		// THE ADMIN ROOT ANSWERS THE ADMIN INTERFACE AND NOTHING ELSE, and the ordinary roots never
		// answer it: two interfaces whose opcodes overlap must be told apart by the channel they
		// arrived on rather than by the number in the message.
		if chan == catalogue.admin && catalogue.admin != 0 {
			return font::font_catalogue_admin::dispatch(&mut catalogue, req, handles, out, reply_handles);
		}
		if op == font::font_catalogue::OP_SUBSCRIBE {
			open_subscription(&mut catalogue, chan, req);
			return None;
		}
		font::font_catalogue::dispatch(&mut catalogue, req, handles, out, reply_handles)
	});
	exit();
}
