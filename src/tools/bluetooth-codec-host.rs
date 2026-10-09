//! Host-only adapter for differential codec checks. It compiles the production leaves unchanged;
//! independent libraries are loaded only by the Python driver, never by this program or an image.
use service_logic::{lc3, sbc};

use std::io::{Read, Write};

fn main() {
	let args: Vec<String> = std::env::args().collect();
	let number = |i: usize| args[i].parse::<u32>().unwrap();
	let encode = args[2] == "encode";
	let mut input = Vec::new();
	std::io::stdin().read_to_end(&mut input).unwrap();
	let mut output = Vec::new();
	if args[1] == "lc3" {
		let config = lc3::Config::new(number(3), number(4)).unwrap();
		let mut encoder = lc3::Encoder::new(config);
		let mut decoder = lc3::Decoder::new(config);
		let mut pcm = vec![0i16; config.samples()];
		for packet in packets(&input) {
			if encode {
				assert_eq!(packet.len(), 2 + 2 * pcm.len());
				let mut frame = vec![0u8; u16::from_le_bytes(packet[..2].try_into().unwrap()) as usize];
				read_pcm(&packet[2..], &mut pcm);
				encoder.encode(&pcm, &mut frame).unwrap();
				write_packet(&mut output, &frame);
			} else {
				decoder.decode(if packet.is_empty() { None } else { Some(packet) }, &mut pcm).unwrap();
				write_pcm(&mut output, &pcm);
			}
		}
	} else {
		let config = if args[1] == "msbc" { sbc::Config::MSBC } else { sbc::Config { frequency: number(3), blocks: number(4) as u8, mode: [sbc::Mode::Mono, sbc::Mode::DualChannel, sbc::Mode::Stereo, sbc::Mode::JointStereo][number(5) as usize], allocation: [sbc::Allocation::Loudness, sbc::Allocation::Snr][number(6) as usize], subbands: number(7) as u8, bitpool: number(8) as u8 } };
		let mut encoder = sbc::Encoder::new(config).unwrap();
		let mut decoder = sbc::Decoder::new();
		let mut pcm = vec![0i16; config.samples() * config.mode.channels()];
		let mut frame = vec![0u8; config.frame_length()];
		for packet in packets(&input) {
			if encode {
				read_pcm(packet, &mut pcm);
				assert_eq!(encoder.encode(&pcm, &mut frame), frame.len());
				write_packet(&mut output, &frame);
			} else {
				let (actual, count) = decoder.decode(packet, &mut pcm).unwrap();
				assert_eq!(actual, config);
				assert_eq!(count, config.samples());
				write_pcm(&mut output, &pcm);
			}
		}
	}
	std::io::stdout().write_all(&output).unwrap();
}

fn packets(mut data: &[u8]) -> Vec<&[u8]> {
	let mut result = Vec::new();
	while !data.is_empty() {
		let n = u16::from_le_bytes(data[..2].try_into().unwrap()) as usize;
		result.push(&data[2..2 + n]);
		data = &data[2 + n..];
	}
	result
}

fn read_pcm(data: &[u8], pcm: &mut [i16]) {
	assert_eq!(data.len(), pcm.len() * 2);
	for (sample, bytes) in pcm.iter_mut().zip(data.chunks_exact(2)) {
		*sample = i16::from_le_bytes(bytes.try_into().unwrap());
	}
}

fn write_packet(output: &mut Vec<u8>, packet: &[u8]) {
	output.extend_from_slice(&(packet.len() as u16).to_le_bytes());
	output.extend_from_slice(packet);
}

fn write_pcm(output: &mut Vec<u8>, pcm: &[i16]) {
	let data: Vec<u8> = pcm.iter().flat_map(|sample| sample.to_le_bytes()).collect();
	write_packet(output, &data);
}
