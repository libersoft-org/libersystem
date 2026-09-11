//! The one place a human form of an address is produced or consumed, so this is where the two
//! directions are held to agreeing with each other.

use super::*;

fn text(rendered: &[u8], len: usize) -> &str {
	core::str::from_utf8(&rendered[..len]).expect("ascii")
}

fn render6(source: &str) -> alloc::string::String {
	let parsed = Ipv6Addr::parse(source.as_bytes()).expect(source);
	let mut out = [0u8; 64];
	let len = parsed.render(&mut out);
	alloc::string::String::from(text(&out, len))
}

#[test]
fn a_dotted_quad_round_trips_and_a_leading_zero_is_refused() {
	let addr = Ipv4Addr::parse(b"10.0.2.15").expect("valid");
	assert_eq!(addr.octets(), [10, 0, 2, 15]);
	let mut out = [0u8; 16];
	let len = addr.render(&mut out);
	assert_eq!(text(&out, len), "10.0.2.15");

	// A LEADING ZERO MEANS TWO THINGS. Some resolvers read `010` as octal and some as decimal, so an
	// address that parses either way is worse than one that is refused.
	assert_eq!(Ipv4Addr::parse(b"010.1.1.1"), None);
	assert_eq!(Ipv4Addr::parse(b"1.2.3"), None);
	assert_eq!(Ipv4Addr::parse(b"1.2.3.4.5"), None);
	assert_eq!(Ipv4Addr::parse(b"1.2.3.256"), None);
	assert_eq!(Ipv4Addr::parse(b""), None);
	assert_eq!(Ipv4Addr::parse(b"1.2.3."), None);
	assert!(Ipv4Addr::parse(b"0.0.0.0").expect("valid").is_unspecified());
}

#[test]
fn the_compressed_form_is_parsed_wherever_it_falls() {
	assert_eq!(Ipv6Addr::parse(b"::").expect("valid").octets(), [0u8; 16]);
	assert_eq!(Ipv6Addr::parse(b"::1").expect("valid").groups(), [0, 0, 0, 0, 0, 0, 0, 1]);
	assert_eq!(Ipv6Addr::parse(b"2001:db8::").expect("valid").groups(), [0x2001, 0x0db8, 0, 0, 0, 0, 0, 0]);
	assert_eq!(Ipv6Addr::parse(b"2001:db8::1").expect("valid").groups(), [0x2001, 0x0db8, 0, 0, 0, 0, 0, 1]);
	assert_eq!(Ipv6Addr::parse(b"fe80::5054:ff:fe12:3456").expect("valid").groups(), [0xfe80, 0, 0, 0, 0x5054, 0x00ff, 0xfe12, 0x3456]);
	assert_eq!(Ipv6Addr::parse(b"2001:0db8:0000:0000:0000:0000:0000:0001").expect("valid").groups(), [0x2001, 0x0db8, 0, 0, 0, 0, 0, 1]);

	// A trailing dotted quad occupies the last two groups, and only the last two.
	assert_eq!(Ipv6Addr::parse(b"::ffff:10.0.2.15").expect("valid").groups(), [0, 0, 0, 0, 0, 0xffff, 0x0a00, 0x020f]);
	assert_eq!(Ipv6Addr::parse(b"::10.0.2.15:1"), None, "a quad is last or it is nothing");
}

#[test]
fn what_is_not_an_address_is_refused_rather_than_guessed() {
	assert_eq!(Ipv6Addr::parse(b"2001:db8::1::2"), None, "two compressions cannot both be the zeros");
	assert_eq!(Ipv6Addr::parse(b"2001:db8:0:0:0:0:0:0:1"), None, "nine groups");
	assert_eq!(Ipv6Addr::parse(b"2001:db8:0:0:0:0:0"), None, "seven groups and no compression");
	// `::` STANDS FOR AT LEAST ONE GROUP. Eight groups with a compression in them is a second
	// spelling of a complete address, and RFC 5952 refuses it.
	assert_eq!(Ipv6Addr::parse(b"1:2:3:4::5:6:7:8"), None);
	assert_eq!(Ipv6Addr::parse(b"2001:db8:"), None, "a trailing single colon");
	assert_eq!(Ipv6Addr::parse(b":1::"), None, "a leading single colon");
	assert_eq!(Ipv6Addr::parse(b"2001:dg8::1"), None, "g is not a hex digit");
	assert_eq!(Ipv6Addr::parse(b"20011:db8::1"), None, "a group is at most four digits");
	assert_eq!(Ipv6Addr::parse(b""), None);
}

#[test]
fn the_rendering_is_rfc_5952s_and_there_is_only_one_of_them() {
	// The longest run is compressed, leading zeros go, and the digits are lowercase.
	assert_eq!(render6("2001:0DB8:0000:0000:0000:0000:0000:0001"), "2001:db8::1");
	assert_eq!(render6("::"), "::");
	assert_eq!(render6("::1"), "::1");
	assert_eq!(render6("fe80:0:0:0:5054:ff:fe12:3456"), "fe80::5054:ff:fe12:3456");

	// A RUN OF ONE IS NOT COMPRESSED: `::` is two characters where `0` is one.
	assert_eq!(render6("2001:db8:0:1:1:1:1:1"), "2001:db8:0:1:1:1:1:1");

	// THE LONGEST RUN WINS, and the LEFTMOST of two equal runs.
	assert_eq!(render6("2001:0:0:1:0:0:0:1"), "2001:0:0:1::1");
	assert_eq!(render6("1:0:0:1:0:0:1:1"), "1::1:0:0:1:1");

	// An IPv4-mapped address is written the way RFC 5952 recommends, which is also the form that
	// makes it obvious what it is.
	assert_eq!(render6("::ffff:10.0.2.15"), "::ffff:10.0.2.15");
}

#[test]
fn every_rendering_parses_back_to_what_produced_it() {
	// The property that matters more than any single spelling: a value written by this host is a
	// value this host reads back unchanged.
	for text in ["::", "::1", "2001:db8::1", "fe80::5054:ff:fe12:3456", "2001:db8:0:1:1:1:1:1", "2001:0:0:1::1", "1::1:0:0:1:1", "ff02::1:ff00:1", "::ffff:10.0.2.15"] {
		let rendered = render6(text);
		let again = Ipv6Addr::parse(rendered.as_bytes()).expect("its own rendering");
		assert_eq!(again, Ipv6Addr::parse(text.as_bytes()).expect("the original"), "{text} rendered as {rendered}");
	}
}

#[test]
fn an_address_knows_what_class_it_is() {
	assert!(Ipv6Addr::parse(b"fe80::1").expect("valid").is_link_local());
	assert!(Ipv6Addr::parse(b"febf::1").expect("valid").is_link_local(), "the whole fe80::/10");
	assert!(!Ipv6Addr::parse(b"fec0::1").expect("valid").is_link_local());
	assert!(Ipv6Addr::parse(b"ff02::1").expect("valid").is_multicast());
	assert!(Ipv6Addr::parse(b"::ffff:1.2.3.4").expect("valid").is_ipv4_mapped());
	assert!(!Ipv6Addr::parse(b"2001:db8::1").expect("valid").is_ipv4_mapped());
	assert!(Ipv6Addr::parse(b"::").expect("valid").is_unspecified());
}

#[test]
fn the_family_is_decided_by_the_text_and_scope_is_needed_only_where_it_means_something() {
	assert!(IpAddress::parse(b"10.0.2.15").expect("valid").is_v4());
	assert!(IpAddress::parse(b"2001:db8::1").expect("valid").is_v6());
	assert!(IpAddress::parse(b"::").expect("valid").is_v6());
	assert_eq!(IpAddress::parse(b"nonsense"), None);

	// SCOPE IS LOAD-BEARING FOR EXACTLY TWO CLASSES. A global address with an interface is
	// over-specified; a link-local one without it is not an address at all.
	assert!(IpAddress::parse(b"fe80::1").expect("valid").needs_scope());
	assert!(IpAddress::parse(b"ff02::1").expect("valid").needs_scope());
	assert!(!IpAddress::parse(b"2001:db8::1").expect("valid").needs_scope());
	assert!(!IpAddress::parse(b"10.0.2.15").expect("valid").needs_scope());
	assert!(IpAddress::unspecified_v4().is_unspecified() && IpAddress::unspecified_v4().is_v4());
	assert!(IpAddress::unspecified_v6().is_unspecified() && IpAddress::unspecified_v6().is_v6());
}

#[test]
fn a_scoped_address_carries_its_interface_into_the_text() {
	let scoped = ScopedAddress { addr: IpAddress::parse(b"fe80::1").expect("valid"), scope: Some(crate::generated::liber::network::v1::InterfaceId { index: 0, generation: 1 }) };
	let mut out = [0u8; 64];
	let len = scoped.render(&mut out);
	assert_eq!(text(&out, len), "fe80::1%if0.1");

	let global = ScopedAddress::global(IpAddress::parse(b"2001:db8::1").expect("valid"));
	let len = global.render(&mut out);
	assert_eq!(text(&out, len), "2001:db8::1");
	assert_eq!(global.scope, None);
}

#[test]
fn a_hardware_address_round_trips() {
	let mac = MacAddr::from_octets([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
	assert_eq!(mac.octets(), [0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
	let mut out = [0u8; 32];
	let len = mac.render(&mut out);
	assert_eq!(text(&out, len), "52:54:00:12:34:56");
}

#[test]
fn the_ui_grammar_takes_a_zone_and_only_where_one_means_something() {
	// RFC 4007's `%zone` IS LOCAL UI SCOPE: it names an interface on THIS machine, so it is
	// meaningless anywhere else - which is why it lives in the input grammar and nowhere else.
	let (addr, zone) = parse_scoped_input(b"fe80::1%if0").expect("a scoped link-local");
	assert!(addr.is_v6() && zone == Some(&b"if0"[..]));
	let (addr, zone) = parse_scoped_input(b"2001:db8::1").expect("an unscoped global");
	assert!(addr.is_v6() && zone.is_none());

	// A ZONE ON AN ADDRESS THAT DOES NOT NEED ONE HIDES A MISTAKE: the writer believed the interface
	// mattered, and it does not.
	assert_eq!(parse_scoped_input(b"2001:db8::1%if0"), None);
	assert_eq!(parse_scoped_input(b"10.0.2.15%if0"), None);
	// And an empty or absurd zone is not a zone.
	assert_eq!(parse_scoped_input(b"fe80::1%"), None);
	assert_eq!(parse_scoped_input(b"fe80::1%0123456789abcdefg"), None);
}

#[test]
fn the_authority_grammar_needs_brackets_where_the_address_has_colons() {
	// `fe80::1:80` CANNOT BE READ: the colon before the port is indistinguishable from the ones
	// inside the address, so the unbracketed form is ambiguous and is refused rather than guessed at.
	let (addr, port) = parse_authority(b"[2001:db8::1]:443").expect("a bracketed authority");
	assert!(addr.is_v6() && port == 443);
	let (addr, port) = parse_authority(b"10.0.2.15:80").expect("an IPv4 authority");
	assert!(addr.is_v4() && port == 80);
	assert_eq!(parse_authority(b"2001:db8::1:443"), None, "ambiguous");
	assert_eq!(parse_authority(b"fe80::1"), None, "an address is not an authority");

	// BRACKETS ARE FOR THE FAMILY THAT NEEDS THEM: one address with two spellings is one too many.
	assert_eq!(parse_authority(b"[10.0.2.15]:80"), None);
}

#[test]
fn a_zone_is_refused_in_a_url_authority() {
	// RFC 6874'S ZONE-IN-URI EXTENSION IS DEPRECATED. Following its `%25` advice would produce
	// non-standard URLs, make two origins compare unequal that name the same service, and leak a
	// local interface name into something meant to travel.
	assert_eq!(parse_authority(b"[fe80::1%if0]:80"), None);
	assert_eq!(parse_authority(b"[fe80::1%25if0]:80"), None);
}

#[test]
fn a_port_is_refused_rather_than_wrapped_or_read_two_ways() {
	assert_eq!(parse_authority(b"10.0.2.15:"), None);
	assert_eq!(parse_authority(b"10.0.2.15:65536"), None);
	assert_eq!(parse_authority(b"10.0.2.15:080"), None, "a leading zero is two readings of one string");
	assert_eq!(parse_authority(b"10.0.2.15:8o"), None);
	assert_eq!(parse_authority(b"10.0.2.15:80").map(|(_, port)| port), Some(80));
	assert_eq!(parse_authority(b"10.0.2.15:65535").map(|(_, port)| port), Some(65535));
}

#[test]
fn every_authority_this_host_writes_parses_back_to_what_wrote_it() {
	// THE PROPERTY THAT MATTERS ACROSS ALL THREE GRAMMARS: a value this host renders is a value this
	// host reads back unchanged, within the grammar it was rendered in.
	for (text, port) in [("2001:db8::1", 443u16), ("fe80::1", 80), ("::1", 8080)] {
		let addr = IpAddress::parse(text.as_bytes()).expect("an address");
		let mut out = [0u8; 96];
		let len = render_authority(&addr, port, &mut out);
		let rendered = &out[..len];
		assert_eq!(parse_authority(rendered), Some((addr, port)), "{}", core::str::from_utf8(rendered).expect("ascii"));
	}
	let v4 = IpAddress::parse(b"10.0.2.15").expect("an address");
	let mut out = [0u8; 32];
	let len = render_authority(&v4, 80, &mut out);
	assert_eq!(&out[..len], b"10.0.2.15:80");
	assert_eq!(parse_authority(&out[..len]), Some((v4, 80)));
}
