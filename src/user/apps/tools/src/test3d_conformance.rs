// test3d-conformance-sw - `Render3D Core Profile 1` AND `Scene3D Core Profile 1`, walked entry by
// entry inside a booted guest.
//
// WHY THIS IS A PROGRAM AND NOT ONLY A HOST TEST. "The profiles are implemented on all three
// architectures" has no executable meaning on the machine that built the image: a host test runs on
// one architecture, once, with the host's own floating point. What the claim is ABOUT is the same
// rasteriser arithmetic and the same scene-layer ordering running on x86_64, aarch64 and riscv64, and
// the only way to make it checkable is to run the same scenes there. So the suite is a library and
// this is the few lines around it.
//
// WHAT IT NEEDS, AND IT IS NOTHING. No display, no input, no volume: every scene draws into memory it
// allocated itself and reads the pixels back, and the scene-layer half touches no backend at all. A
// conformance run that needed a service would be a test of the service too, and would fail for
// reasons that are not about the profile.
//
// TWO PROFILES, TALLIED SEPARATELY. An implementation may carry the command layer and not the
// retained layer above it, so one number that mixed them could not say which conformed. The run
// fails when EITHER does.
//
// THE RESULT FORMAT IS ONE LINE PER FEATURE, printed AS IT FINISHES. This runs inside a guest whose
// output is a serial log, so a suite that collected its results and printed them at the end would
// print nothing at all when a scene hung - and which scene it was is the whole of what a reader
// needs. The last line is the verdict a harness greps for.
//
// AND `Unsupported` IS A FAILURE HERE. The profile is a CLOSED list: a backend that refuses a
// Profile 1 frame is not a backend with a gap, it is a backend that does not conform. The two are
// reported separately because a reader needs to tell "this is wrong" from "this is missing", and the
// run fails for either.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use render3d_conformance::{Summary, Verdict, run};
use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	inherit_stdout(bootstrap);
	let _ = recv_launch_bytes(bootstrap);
	print(b"test3d-conformance: start\n");

	let summary: Summary = run(|name, group, verdict| {
		let line = match verdict {
			Verdict::Pass => format!("test3d-conformance: pass {group}/{name}\n"),
			Verdict::Fail(why) => format!("test3d-conformance: FAIL {group}/{name}: {why}\n"),
			Verdict::Unsupported(why) => format!("test3d-conformance: UNSUPPORTED {group}/{name}: {why}\n"),
		};
		print(line.as_bytes());
	});

	// A FEATURE WITH NO SCENE IS A FAILURE OF THE SUITE, reported by name and by the profile it is
	// in. Without this a feature added to a profile is a feature nobody tests, and every run still
	// says everything passed.
	for name in &summary.render3d.untested {
		print(format!("test3d-conformance: UNTESTED render3d/{name}\n").as_bytes());
	}
	for name in &summary.scene3d.untested {
		print(format!("test3d-conformance: UNTESTED scene3d/{name}\n").as_bytes());
	}
	for name in &summary.extended.untested {
		print(format!("test3d-conformance: UNTESTED scene3d-extended/{name}\n").as_bytes());
	}
	print(format!("test3d-conformance: render3d {} passed, {} failed, {} unsupported, {} untested\n", summary.render3d.passed, summary.render3d.failed, summary.render3d.unsupported, summary.render3d.untested.len()).as_bytes());
	print(format!("test3d-conformance: scene3d {} passed, {} failed, {} unsupported, {} untested\n", summary.scene3d.passed, summary.scene3d.failed, summary.scene3d.unsupported, summary.scene3d.untested.len()).as_bytes());
	print(format!("test3d-conformance: scene3d-extended {} passed, {} failed, {} unsupported, {} untested\n", summary.extended.passed, summary.extended.failed, summary.extended.unsupported, summary.extended.untested.len()).as_bytes());
	print(format!("test3d-conformance: {} passed, {} failed, {} unsupported, {} untested\n", summary.passed(), summary.failed(), summary.unsupported(), summary.untested()).as_bytes());
	// THE TWO CLAIMS ARE PRINTED SEPARATELY BECAUSE THEY ARE TWO CLAIMS. `Scene3D Extended Profile 1`
	// is optional as a whole, so "conforms" is about the two CORE profiles; a layer that carries the
	// extended part says so on its own line, and a reader can tell a core-conforming implementation
	// without the part from one that claims it and fails it.
	print(if summary.complete() { b"test3d-conformance: conforms\n".as_slice() } else { b"test3d-conformance: DOES NOT CONFORM\n".as_slice() });
	print(if summary.complete_with_extended() { b"test3d-conformance: and carries Scene3D Extended Profile 1\n".as_slice() } else { b"test3d-conformance: DOES NOT CARRY Scene3D Extended Profile 1\n".as_slice() });
	exit()
}
