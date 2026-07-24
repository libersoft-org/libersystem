use super::*;

#[test]
fn forward_and_inverse_preserve_an_unquantized_block() {
	let source = [38, 6, 210, 107, 42, 125, 185, 151, 241, 224, 125, 233, 227, 8, 57, 96];
	let mut transformed = source;
	forward(&mut transformed);
	inverse(&mut transformed);
	assert_eq!(transformed, source);
}
