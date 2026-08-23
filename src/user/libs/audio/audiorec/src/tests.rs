use super::*;
use alloc::string::ToString;

#[test]
fn a_destination_is_required_and_is_the_only_positional() {
	assert_eq!(parse_args(b""), Err(Error::MissingOutput));
	assert_eq!(parse_args(b"--rate 8000"), Err(Error::MissingOutput));
	assert_eq!(parse_args(b"one.wav two.wav"), Err(Error::InvalidOptions));
	let config = parse_args(b"vol://system/take.wav").unwrap();
	assert_eq!(config.output, "vol://system/take.wav".to_string());
	assert_eq!((config.rate, config.channels, config.seconds, config.force), (48_000, 2, None, false));
}

#[test]
fn options_are_read_on_either_side_of_the_destination() {
	let before = parse_args(b"-r 8000 -c 1 -s 5 -f take.wav").unwrap();
	let after = parse_args(b"take.wav --rate 8000 --channels 1 --seconds 5 --force").unwrap();
	assert_eq!(before, after);
	assert_eq!((before.rate, before.channels, before.seconds, before.force), (8_000, 1, Some(5), true));
}

#[test]
fn a_format_the_service_would_refuse_is_refused_here() {
	// The same bound `pcm::Format` applies, so the tool never reports a service refusal it could
	// have explained itself.
	assert_eq!(parse_args(b"-r 96000 take.wav"), Err(Error::UnsupportedFormat));
	assert_eq!(parse_args(b"-r 7999 take.wav"), Err(Error::UnsupportedFormat));
	assert_eq!(parse_args(b"-c 3 take.wav"), Err(Error::UnsupportedFormat));
	assert_eq!(parse_args(b"-c 0 take.wav"), Err(Error::UnsupportedFormat));
}

#[test]
fn malformed_options_are_refused_rather_than_guessed_at() {
	// The next word IS the value, so a destination written where a number belongs is a malformed
	// option rather than a missing destination - which is the mistake that was actually made.
	assert_eq!(parse_args(b"--rate take.wav"), Err(Error::InvalidOptions));
	assert_eq!(parse_args(b"--rate"), Err(Error::InvalidOptions), "and a missing value is not zero");
	assert_eq!(parse_args(b"--rate 12x0 take.wav"), Err(Error::InvalidOptions));
	assert_eq!(parse_args(b"--seconds 0 take.wav"), Err(Error::InvalidOptions), "a recording of no length is a mistake");
	assert_eq!(parse_args(b"--nonsense take.wav"), Err(Error::InvalidOptions));
}

#[test]
fn help_is_answered_before_anything_else_is_judged() {
	// `--help` with no destination is a help request, not a missing-destination error.
	assert_eq!(parse_args(b"--help").unwrap().mode, Mode::Help);
	assert_eq!(parse_args(b"-h --rate 96000").unwrap().mode, Mode::Help);
	assert!(help_text().contains("--seconds"));
}

#[test]
fn a_recording_longer_than_riff_can_describe_is_refused_before_it_starts() {
	// 48 kHz stereo is 192000 bytes a second, so RIFF's 32-bit `data` length runs out after about
	// six hours and a quarter. Asking for seven is refused here rather than at the ceiling.
	let seconds_that_fit = (MAX_DATA_BYTES / (48_000 * 4)) as u32;
	assert!(parse_args(alloc::format!("-s {seconds_that_fit} take.wav").as_bytes()).is_ok());
	assert_eq!(parse_args(alloc::format!("-s {} take.wav", seconds_that_fit + 1).as_bytes()), Err(Error::TooLong));
}

#[test]
fn the_frame_limit_is_the_shorter_of_the_two_bounds() {
	let bounded = parse_args(b"-r 8000 -c 1 -s 10 take.wav").unwrap();
	assert_eq!(bounded.frame_limit(), 80_000, "ten seconds at 8 kHz");
	let unbounded = parse_args(b"-r 8000 -c 1 take.wav").unwrap();
	assert_eq!(unbounded.frame_limit(), MAX_DATA_BYTES / 2, "no --seconds still stops where RIFF does");
	assert_eq!(unbounded.frame_bytes(), 2);
}
