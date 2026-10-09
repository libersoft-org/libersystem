#!/usr/bin/env bash
# check-qemu-3d-demo.sh - the LIVE half of the 3D demo's proof: frames off a real screen.
#
# WHAT THE GUEST TEST CANNOT SHOW. `kernel.applications.the_3d_demo_renders_a_lit_scene_and_survives_a_resize`
# reads what the demo SAYS it did - that it opened a surface, that it presented, that it rebuilt for a
# resize, that a key ended it - and a renderer that drew nothing at all says exactly the same things.
# What needs a screen is the picture: that there is a lit object standing in front of a horizon, that
# the ground's texture REPEATS across it, that the translucent panel is a mix of what is in front and
# what is behind rather than either alone, that the 2D overlay reached the same frame the 3D scene
# did, and that successive frames differ.
#
# FOUR RUNS, EACH ANSWERING ONE QUESTION. A live animated run for the frames that must differ; a run
# at a STATED POSE for the checks that need to know what is in front of the camera; two runs at
# different aspect ratios for the geometry that must survive one; and the console, which `q` must give
# back.
#
# IT BOOTS ITS OWN GUEST unless one is already up, and takes down only what it started - a gate that
# quit somebody's development instance would be a gate nobody runs twice.

SCRIPT_NAME=check-qemu-3d-demo.sh
source "$(dirname "${BASH_SOURCE[0]}")/../../lib.sh"

FRAMES_DIR="$(mktemp -d "${TMPDIR:-/tmp}/3d-demo-frames.XXXXXX")"
mkdir -p "$REPO_ROOT/.build/logs"
PROOF_DIR="$(mktemp -d "$REPO_ROOT/.build/logs/qemu-3d-demo.XXXXXX")"
BOOTED=0
cleanup() {
	if ((BOOTED)); then
		"$REPO_ROOT/lab.sh" quit >/dev/null 2>&1 || true
	fi
	cp -a "$FRAMES_DIR/." "$PROOF_DIR/"
	rm -rf "$FRAMES_DIR"
	note "3D frame and performance proof: $PROOF_DIR"
}
trap cleanup EXIT

CHECK="$REPO_ROOT/src/tools/check-3d-demo-frames.py"

# THE PACKAGE AUDIT FIRST, because it needs no guest and answers a different question: what the
# ARTIFACT is. A program carrying its own copy of the rasteriser draws the same picture as one using
# the shared library, and a program holding capabilities it has no business with draws it too.
python3 "$REPO_ROOT/src/tools/check-3d-demo-package.py" || die "the demo's package is not what its manifest says it is"

# AN INSTANCE THAT IS ALREADY UP IS USED AS IT IS. The probe is a shell command rather than a socket
# test: a socket that exists and answers nothing is the case a gate has to survive.
if ! "$REPO_ROOT/lab.sh" sh uname >/dev/null 2>&1; then
	note "no instance is up - booting one"
	"$REPO_ROOT/lab.sh" boot >/dev/null || die "the guest did not boot"
	BOOTED=1
fi

# The visual fixtures use a fixed 320x240 scene; the performance runs below use all three
# required physical render sizes without scaling the scene down.
SCENE="--scene-width 320 --scene-height 240"
VISUAL_RUN=0

# Run the demo in the background and wait until the screen is its. WAITED FOR RATHER THAN SLEPT
# THROUGH: a capture taken before the first present shows the CONSOLE, which is not a blank screen -
# it is white text on black, and a checker that read it would blame the renderer for its own timing.
#
# NO FRAME LIMIT, AND THE KEY ENDS IT. A frame count would have to be chosen against a rate nobody
# measured on the machine the gate happens to run on: too few and the demo has left before the
# captures, too many and the run outlives the harness's patience. What the captures need is a demo
# that is still drawing, and what ends it is the same `q` a person presses - which is also the exit
# the last check reads.
start_demo() {
	local arguments="$1"
	VISUAL_RUN=$((VISUAL_RUN + 1))
	DEMO_LOG="$FRAMES_DIR/demo-$VISUAL_RUN.log"
	printf 'test3d-sw %s %s\n' "$SCENE" "$arguments" >"$DEMO_LOG"
	"$REPO_ROOT/lab.sh" sh --timeout 900 "test3d-sw $SCENE $arguments" >>"$DEMO_LOG" 2>&1 &
	DEMO_PID=$!
	local ready=0
	for _ in $(seq 1 90); do
		if "$REPO_ROOT/lab.sh" shot "$FRAMES_DIR/ready.ppm" >/dev/null 2>&1 && python3 "$CHECK" --ready "$FRAMES_DIR/ready.ppm"; then
			ready=1
			break
		fi
		sleep 2
	done
	((ready == 1)) || die "the demo never reached the screen: its output so far was $(tail -n 3 "$DEMO_LOG" 2>/dev/null)"
}

finish_demo() {
	"$REPO_ROOT/lab.sh" key q >/dev/null 2>&1 || die "the quit key was not delivered"
	wait "$DEMO_PID" 2>/dev/null || true
	grep -q "test3d-sw: presented" "$DEMO_LOG" || die "the demo did not run to completion; its output was: $(tail -n 3 "$DEMO_LOG")"
}

# 1. THE LIVE RUN: three timed frames, and the first thing asked of them is that they DIFFER. One
#    frame proves a drawing; three seconds apart prove a drawing that is running.
start_demo "--width 1280 --height 800"
captured=0
for index in 1 2 3; do
	if "$REPO_ROOT/lab.sh" shot "$FRAMES_DIR/frame-$index.ppm" >/dev/null 2>&1; then
		captured=$((captured + 1))
	fi
	sleep 3
done
finish_demo
((captured == 3)) || die "only $captured of three live frames were captured"
python3 "$CHECK" "$FRAMES_DIR/frame-1.ppm" "$FRAMES_DIR/frame-2.ppm" "$FRAMES_DIR/frame-3.ppm" || die "the live frames do not show the scene the demo draws"

# 2. THE DETERMINISTIC POSE. A live capture cannot say which face is in front, because which faces a
#    rotation shows depends on the rotation - so the rotation is STATED, and the checker computes
#    what that pose must show from the same camera the demo uses.
for pose in 0 90; do
	start_demo "--pose $pose --width 1280 --height 800"
	"$REPO_ROOT/lab.sh" shot "$FRAMES_DIR/pose-$pose.ppm" >/dev/null 2>&1 || die "the pose $pose frame was not captured"
	finish_demo
	python3 "$CHECK" --pose "$pose" "$FRAMES_DIR/pose-$pose.ppm" || die "the frame at pose $pose is not what that pose puts in front of the camera"
done

# 3. TWO ASPECT RATIOS, because a projection that is right at one and wrong at the other is a
#    projection that divides by the wrong extent - and a window is not always a desktop's shape.
for shape in "1024 768 desktop" "480 800 mobile"; do
	set -- $shape
	start_demo "--pose 0 --width $1 --height $2"
	"$REPO_ROOT/lab.sh" shot "$FRAMES_DIR/$3.ppm" >/dev/null 2>&1 || die "the $3 frame was not captured"
	finish_demo
	python3 "$CHECK" --pose 0 "$FRAMES_DIR/$3.ppm" || die "the scene at the $3 aspect ratio is not the scene"
done

# 4. AND `q` GAVE THE SCREEN BACK. An application that took the console and left it holding a
#    rendered frame is a machine a person sees as dead. Every run above ended on that key; this reads
#    what the screen holds now that the last one has.
for _ in $(seq 1 20); do
	sleep 1
	"$REPO_ROOT/lab.sh" shot "$FRAMES_DIR/after.ppm" >/dev/null 2>&1 || continue
	if python3 "$CHECK" --console "$FRAMES_DIR/after.ppm" >/dev/null 2>&1; then
		break
	fi
done
python3 "$CHECK" --console "$FRAMES_DIR/after.ppm" || die "the console was not restored after the demo left"

note "live frames prove animation, a bounded central object, a repeating ground texture, a blended panel, the 2D overlay in the same frame, two stated poses, two aspect ratios and a restored console"

# Full-size release measurements. The first five presents warm the frame path; thirty subsequent
# intervals include pacing, acquire, rendering, HUD, present and worker-thread allocation activity.
{
	date -u +%Y-%m-%dT%H:%M:%SZ
	qemu-system-x86_64 --version | head -n 1
	LC_ALL=C lscpu
	"$REPO_ROOT/lab.sh" monitor "info kvm"
	"$REPO_ROOT/lab.sh" monitor "info cpus"
	python3 - "$REPO_ROOT" <<'ENVIRONMENT'
import hashlib,json,os,pathlib,re,shlex,sys
root=pathlib.Path(sys.argv[1])
state=pathlib.Path(os.environ.get("LIBER_DEV_STATE") or root/".build/boot")
try:
    pgid=int((state/"lab-guest.pgid").read_text().strip())
except (OSError,ValueError):
    pgid=int(json.loads((state/"dev-instance.lock").read_text())["pgid"])
found=False
for process in pathlib.Path("/proc").iterdir():
    if not process.name.isdecimal(): continue
    try:
        if os.getpgid(int(process.name))!=pgid: continue
        args=[arg.decode() for arg in (process/"cmdline").read_bytes().split(b"\0") if arg]
    except (OSError,UnicodeError):
        continue
    if args and pathlib.Path(args[0]).name.startswith("qemu-system-"):
        print("guest-command="+shlex.join(args))
        found=True
assert found, "cannot identify the live lab guest's QEMU command"
artifact=root/".build/image/x86_64-unknown-none/bin/test3d-sw"
data=artifact.read_bytes()
print("staged-demo-sha256="+hashlib.sha256(data).hexdigest())
for key in ("target","profile","rustc-commit","rustflags","features"):
    value=re.search(rb"(?:^|\n)"+key.encode()+rb"=([^\n\0]+)",data)
    assert value, "missing built artifact identity field: "+key
    print(key+"="+value.group(1).decode())
print("frame-clock=rt::clock_ns / SYS_CLOCK_MONO_NS (guest calibrated monotonic nanoseconds)")
print("measurement=5 warmup presents, 30 complete frame intervals; animated scene; fixed physical/render extent")
ENVIRONMENT
} >"$FRAMES_DIR/environment.log" 2>&1
failed=0
for phase in core extended; do
	repeat=0
	row=0
	for size in "320 240" "800 600" "640 480" "640 480" "640 480"; do
		read -r width height <<<"$size"
		row=$((row + 1))
		if [[ "$width" == 640 ]]; then repeat=$((repeat + 1)); fi
		[[ "$phase" == extended && "$width" == 640 && "$repeat" -gt 1 ]] && continue
		extra=""
		[[ "$phase" == extended ]] && extra="--postprocess"
		log="$FRAMES_DIR/perf-$phase-${width}x${height}-$row.log"
		"$REPO_ROOT/lab.sh" sh --timeout 900 "test3d-sw --no-input --report --frames 35 --width $width --height $height --scene-width $width --scene-height $height $extra" >"$log" 2>&1 || failed=1
		cat "$log"
		if python3 - "$log" "$phase" "$width" "$height" <<'PERF'; then :; else failed=1; fi
import pathlib,re,sys
path,phase,width,height=sys.argv[1:]
text=pathlib.Path(path).read_text()
assert "test3d-sw: presented 35 frame(s)" in text, "all requested frames must finish"
assert "allocation counter observes shared-library preparation" in text, "the actual allocator counter must observe library allocations"
match=re.search(r"steady samples (\d+) median_us (\d+) p99_us (\d+) render_alloc_max (\d+) loop_alloc_max (\d+)",text)
assert match, "missing steady-state timing/allocation report"
samples,median,p99,render_alloc,loop_alloc=map(int,match.groups())
assert samples==30 and render_alloc==0 and loop_alloc==0, "steady-state frames must allocate nothing"
assert "test3d-sw: colour " in text and "test3d-sw: present " in text and "heap live_bytes " in text and "scene prepared bytes " in text and "present_max_us " in text, "memory and present stages must be reported"
if phase=="core" and width=="640" and height=="480":
    elapsed=re.search(r"steady elapsed_ns (\d+) max_us (\d+)",text)
    assert elapsed, "missing complete frame-window elapsed time"
    elapsed_ns,maximum=map(int,elapsed.groups())
    assert median<=33333 and elapsed_ns<=samples*1_000_000_000//30, f"stable 30 FPS floor missed: median={median}us, window={elapsed_ns}ns, p99/max={p99}/{maximum}us"
if phase=="extended":
    assert "HDR chain executed six downsamples, five upsamples and resolve" in text
    assert "test3d-sw: postprocess " in text and "test3d-sw: shadow " in text and "HDR prepared bytes " in text
PERF
	done
done
((failed == 0)) || die "3D performance/allocation criteria failed; proof retained in $PROOF_DIR"
note "all three core/Extended sizes measured, 640x480 core stable 30 FPS across three windows and steady-state zero allocations passed"
