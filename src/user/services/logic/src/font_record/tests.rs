use super::*;

const DIGEST_TEXT: &str = "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";

fn digest() -> [u8; 32] {
	let mut bytes = [0u8; 32];
	for (at, slot) in bytes.iter_mut().enumerate() {
		*slot = at as u8 + 1;
	}
	bytes
}

fn declaration() -> String {
	alloc::format!("# the last-resort face\nfamily = Liber Sans\nstyle = Book\nformat = truetype-glyf\nface-index = 0\nweight = 400\nwidth = normal\nslant = upright\ndigest = {DIGEST_TEXT}\n")
}

#[test]
// THE ORDINARY DECLARATION, read as what it says. Everything below is a way of getting this wrong,
// and a parser that refused everything would pass all of them.
fn a_declaration_is_read_as_what_it_declares() {
	let record = parse(&declaration(), &digest()).expect("a valid declaration");
	assert_eq!(record.family, "Liber Sans");
	assert_eq!(record.style, "Book");
	assert_eq!(record.format, FaceFormat::TruetypeGlyf);
	assert_eq!(record.face_index, 0);
	assert_eq!(record.weight, 400);
	assert_eq!(record.width, FaceWidth::Normal);
	assert_eq!(record.slant, FaceSlant::Upright);
	assert!(record.axes.is_empty());
	assert_eq!(record.digest, digest());
	assert!(record.encoded_len() <= MAX_FACE_METADATA_BYTES);
}

#[test]
// THE VOCABULARIES ARE CLOSED, and a value outside one is REFUSED rather than carried through as a
// string somebody downstream will have to interpret. The whole point of a closed vocabulary is that
// a fallback decision is steered by values whose meaning was decided here.
fn a_value_outside_a_closed_vocabulary_is_refused() {
	for (field, value) in [("format", "svg"), ("width", "very-wide"), ("slant", "backslanted")] {
		let text = declaration().replace(&alloc::format!("{field} = "), "unused = ");
		let text = alloc::format!("{text}{field} = {value}\n");
		assert_eq!(parse(&text, &digest()), Err(RecordError::UnknownKey), "an unknown key is refused before its value is read");
		let good = declaration();
		let replaced = match field {
			"format" => good.replace("format = truetype-glyf", &alloc::format!("format = {value}")),
			"width" => good.replace("width = normal", &alloc::format!("width = {value}")),
			_ => good.replace("slant = upright", &alloc::format!("slant = {value}")),
		};
		assert_eq!(parse(&replaced, &digest()), Err(RecordError::UnknownValue), "{field} = {value} is outside the vocabulary");
	}
	// AND THE ONE NUMERIC RANGE THAT IS A VOCABULARY TOO: OpenType weights are 1..=1000.
	assert_eq!(parse(&declaration().replace("weight = 400", "weight = 0"), &digest()), Err(RecordError::OutOfRange));
	assert_eq!(parse(&declaration().replace("weight = 400", "weight = 1001"), &digest()), Err(RecordError::OutOfRange));
	assert!(parse(&declaration().replace("weight = 400", "weight = 1000"), &digest()).is_ok());
}

#[test]
// A DECLARATION THAT CONTRADICTS ITSELF, OR OMITS WHAT IT MUST STATE, IS REFUSED. Neither is
// something a reader would notice: a repeated key would silently take whichever came last, and a
// missing one would take a default nobody declared.
fn a_repeated_or_missing_field_is_refused() {
	assert_eq!(parse(&alloc::format!("{}style = Bold\n", declaration()), &digest()), Err(RecordError::Repeated));
	for field in ["family = Liber Sans\n", "style = Book\n", "format = truetype-glyf\n", "face-index = 0\n", "weight = 400\n", "width = normal\n", "slant = upright\n"] {
		let text = declaration().replace(field, "");
		assert_eq!(parse(&text, &digest()), Err(RecordError::Missing), "a declaration without `{}` is incomplete", field.trim());
	}
	assert_eq!(parse("family Liber Sans\n", &digest()), Err(RecordError::Malformed));
	assert_eq!(parse("family =\n", &digest()), Err(RecordError::Malformed));
	assert_eq!(parse("colour = red\n", &digest()), Err(RecordError::UnknownKey));
}

#[test]
// THE DIGEST IS WHAT TIES THE RECORD TO THE BYTES BESIDE IT. It proves ASSOCIATION and not truth -
// which is said in the module's own documentation - and a record that does not even name the file it
// sits beside is the case this catches.
fn a_record_whose_digest_is_not_the_face_beside_it_is_refused() {
	let mut other = digest();
	other[31] ^= 1;
	assert_eq!(parse(&declaration(), &other), Err(RecordError::DigestMismatch));
	assert_eq!(parse(&declaration().replace(DIGEST_TEXT, "00"), &digest()), Err(RecordError::OutOfRange));
	assert_eq!(parse(&declaration().replace(DIGEST_TEXT, &"z".repeat(64)), &digest()), Err(RecordError::OutOfRange));
}

#[test]
// THE ENCODED RECORD IS BOUNDED, and the bound is on the WIRE bytes rather than on the text - a
// declaration can be comfortable to read and still not fit a LIST reply.
fn a_record_past_the_metadata_ceiling_is_refused() {
	let long_family = "F".repeat(MAX_FAMILY_BYTES);
	let accepted = parse(&declaration().replace("Liber Sans", &long_family), &digest()).expect("a family at its bound");
	assert!(accepted.encoded_len() <= MAX_FACE_METADATA_BYTES);
	let too_long = "F".repeat(MAX_FAMILY_BYTES + 1);
	assert_eq!(parse(&declaration().replace("Liber Sans", &too_long), &digest()), Err(RecordError::TooLong));
	// AND THE AXES ARE BOUNDED THE SAME WAY: eight is the contract's bound and a ninth is refused
	// rather than dropped.
	let mut axes = declaration();
	for _ in 0..MAX_DECLARED_AXES {
		axes.push_str("axis = wght 100 400 900\n");
	}
	let at_bound = parse(&axes, &digest()).expect("eight axes");
	assert_eq!(at_bound.axes.len(), MAX_DECLARED_AXES);
	assert!(at_bound.encoded_len() <= MAX_FACE_METADATA_BYTES, "the ceiling holds at the axis bound: {} bytes", at_bound.encoded_len());
	axes.push_str("axis = ital 0 0 1\n");
	assert_eq!(parse(&axes, &digest()), Err(RecordError::TooLong));
}

#[test]
// AN AXIS IS A TAG AND A RANGE, and a range whose default is outside it is a declaration no
// instance can satisfy.
fn an_axis_states_a_tag_and_an_ordered_range() {
	let text = alloc::format!("{}axis = wght 100 400 900\n", declaration());
	let record = parse(&text, &digest()).expect("one axis");
	assert_eq!(record.axes.len(), 1);
	assert_eq!(record.axes[0].tag, u32::from_be_bytes(*b"wght"));
	assert_eq!((record.axes[0].minimum, record.axes[0].default, record.axes[0].maximum), (100, 400, 900));
	for bad in ["wght 400 100 900", "wght 100 900 400", "wg 100 400 900", "wght 100 400", "wght 100 400 900 1"] {
		let text = alloc::format!("{}axis = {bad}\n", declaration());
		assert!(parse(&text, &digest()).is_err(), "`axis = {bad}` is not an axis");
	}
}

#[test]
// A FACE INDEX SELECTS A FACE WITHIN A COLLECTION, and a non-zero index on a single-face file is a
// declaration nothing can resolve - checkable without opening the font, which is the only kind of
// check this module is allowed to make.
fn a_face_index_is_only_meaningful_in_a_collection() {
	assert_eq!(parse(&declaration().replace("face-index = 0", "face-index = 2"), &digest()), Err(RecordError::OutOfRange));
	let collection = declaration().replace("format = truetype-glyf", "format = collection").replace("face-index = 0", "face-index = 2");
	assert_eq!(parse(&collection, &digest()).expect("a collection face").face_index, 2);
}

#[test]
// THE RELABELLING RULE: a replacement is a new version of the SAME face, and a declaration that
// changes what the face IS is a different face arriving under somebody else's name. This is the
// comparison the withdrawal rule is built on, and the digest is deliberately not part of it - a
// replacement always changes the bytes.
fn a_replacement_that_changes_the_declared_identity_is_not_the_same_face() {
	let original = parse(&declaration(), &digest()).expect("valid");
	let mut replaced_bytes = digest();
	replaced_bytes[0] ^= 0xff;
	let new_digest_text = {
		let mut text = String::new();
		for byte in replaced_bytes {
			text.push_str(&alloc::format!("{byte:02x}"));
		}
		text
	};
	let same = parse(&declaration().replace(DIGEST_TEXT, &new_digest_text), &replaced_bytes).expect("valid");
	assert!(original.declares_same_identity_as(&same), "new bytes under the same declaration are the same face");
	for changed in [
		declaration().replace("family = Liber Sans", "family = Someone Else"),
		declaration().replace("style = Book", "style = Bold"),
		declaration().replace("format = truetype-glyf", "format = opentype-cff"),
		alloc::format!("{}axis = wght 100 400 900\n", declaration()),
	] {
		let other = parse(&changed, &digest()).expect("valid");
		assert!(!original.declares_same_identity_as(&other), "a changed declaration is a different face");
	}
}

#[test]
// THE THREE INSTALLATION CEILINGS ARE ARITHMETIC AND NOT HOPE. The reply bound used to be `64 * 256`
// exactly, which is the records ALONE - a reply is never only its records, so the exact-bound
// assertion could not have passed.
fn the_reply_bound_holds_every_record_and_its_framing() {
	assert_eq!(MAX_INSTALLED_FACES, 64);
	assert_eq!(MAX_FACE_METADATA_BYTES, 256);
	assert_eq!(MAX_LIST_REPLY_BYTES, 20480);
	assert!(MAX_LIST_REPLY_BYTES > MAX_INSTALLED_FACES * MAX_FACE_METADATA_BYTES, "a reply is never only its records");
	assert_eq!(MAX_LIST_REPLY_BYTES - MAX_INSTALLED_FACES * MAX_FACE_METADATA_BYTES, LIST_REPLY_ENVELOPE_BYTES);
}

#[test]
// THE DESTINATION AND THE SIDECAR NAMING, WRITTEN ONCE. Two copies of a path is a mint that succeeds
// over a directory nobody reads, and a stem-keyed sidecar would be one declaration claiming both
// `sans.ttf` and `sans.otf`.
fn the_destination_and_the_sidecar_name_are_one_definition() {
	assert_eq!(FONT_DIRECTORY, "vol://system/share/fonts");
	assert_eq!(RECORD_SUFFIX, ".face");
	assert_eq!(alloc::format!("sans.ttf{RECORD_SUFFIX}"), "sans.ttf.face");
}
