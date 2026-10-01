use super::*;
use alloc::vec;

fn ints(values: &[u64]) -> Value {
	Value::Package(values.iter().map(|value| Value::Integer(*value)).collect())
}

#[test]
fn fif_says_whether_the_speed_is_the_operating_system_s() {
	assert_eq!(fif(&ints(&[0, 1, 5, 0])), Ok(Interface { fine_grain: true, step: 5 }));
	assert_eq!(fif(&ints(&[0, 0, 0, 0])), Ok(Interface { fine_grain: false, step: 1 }), "a step of zero is one percent");
	assert_eq!(fif(&ints(&[0, 1])), Err(Refusal::Shape("_FIF")));
}

#[test]
fn fps_reads_each_level_and_an_unknown_speed_as_none() {
	let value = Value::Package(vec![Value::Integer(0), ints(&[0, 0, 0, 0, 0]), ints(&[40, 3332, 2000, 30, 1500]), ints(&[100, 3532, 0xFFFF_FFFF, 45, 3000])]);
	let levels = fps(&value).unwrap();
	assert_eq!(levels.len(), 3);
	assert_eq!(levels[1], Level { control: 40, speed_rpm: 2000, noise: 30, power_mw: 1500 });
	assert_eq!(levels[2].speed_rpm, 0, "0xFFFFFFFF is the specification's unknown");
	assert_eq!(fps(&Value::Package(vec![Value::Integer(0), ints(&[1, 2, 3])])), Err(Refusal::Shape("an _FPS level")));
}

#[test]
fn fst_reads_the_control_in_force_and_the_speed() {
	assert_eq!(fst(&ints(&[0, 40, 2000])), Ok(State { control: 40, speed_rpm: 2000 }));
	assert_eq!(fst(&ints(&[0, 40, 0xFFFF_FFFF])), Ok(State { control: 40, speed_rpm: 0 }));
	assert_eq!(fst(&Value::Integer(3)), Err(Refusal::Shape("_FST")));
}
