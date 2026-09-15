#!/usr/bin/env bash
# check-qemu-2d-demo.sh - the LIVE half of the 2D demo's proof: frames off a real screen.
#
# WHAT THE GUEST TEST CANNOT SHOW. `kernel.services.the_2d_demo_draws_a_real_scene_with_real_damage`
# reads what the demo SAYS it did - which phase, how many rectangles, which damage travelled to the
# driver - and a renderer that drew nothing at all would say exactly the same things. What needs a
# screen is the picture: that it MOVES, that a shallow edge is antialiased rather than a staircase,
# that a blend is a mix rather than one of its operands, that a filtered region is filtered, and that
# the line of text carries a colour glyph drawn in its OWN palette rather than in the run's paint.
#
# THREE FRAMES, TIMED, and the first check is that they differ: one frame proves a drawing and three
# seconds apart prove a drawing that is running.
#
# IT BOOTS ITS OWN GUEST unless one is already up, and takes down only what it started - a gate that
# quit somebody's development instance would be a gate nobody runs twice.

SCRIPT_NAME=check-qemu-2d-demo.sh
source "$(dirname "${BASH_SOURCE[0]}")/../../lib.sh"

FRAMES_DIR="$(mktemp -d "${TMPDIR:-/tmp}/2d-demo-frames.XXXXXX")"
BOOTED=0
cleanup() {
	if ((BOOTED)); then
		"$REPO_ROOT/lab.sh" quit >/dev/null 2>&1 || true
	fi
	rm -rf "$FRAMES_DIR"
}
trap cleanup EXIT

# THE PACKAGE AUDIT FIRST, because it needs no guest and answers a different question: what the
# ARTIFACT is. A program carrying its own copy of the rasteriser draws the same picture as one using
# the shared library, and a program holding capabilities it has no business with draws it too.
python3 "$REPO_ROOT/src/tools/check-2d-demo-package.py" || die "the demo's package is not what its manifest says it is"

# AN INSTANCE THAT IS ALREADY UP IS USED AS IT IS. The probe is a shell command rather than a socket
# test: a socket that exists and answers nothing is the case a gate has to survive, and `uname` is
# the cheapest question this system answers.
if ! "$REPO_ROOT/lab.sh" sh uname >/dev/null 2>&1; then
	note "no instance is up - booting one"
	"$REPO_ROOT/lab.sh" boot >/dev/null || die "the guest did not boot"
	BOOTED=1
fi

# THE DEMO RUNS WHILE THE FRAMES ARE TAKEN, which is why it is started in the background here: the
# shell command does not return until the demo ends, and a capture after it ended is a capture of
# whatever was left on the screen.
# A TIMEOUT OF ITS OWN, because `lab sh` defaults to thirty seconds and reports a partial run as a
# failure - and six hundred frames at a display's own pace is ten seconds of drawing, which is what
# the captures below are taken from. The demo ALSO paces itself down to nothing whenever the console
# takes the screen back, so the wall-clock a run takes is not the frame count divided by a rate.
"$REPO_ROOT/lab.sh" sh --timeout 180 "test2d-sw --frames=600 --phase-frames=60" >"$FRAMES_DIR/demo.log" 2>&1 &
DEMO_PID=$!

# LONG ENOUGH FOR THE FIRST PHASE TO BE DRAWING, and then spaced so the captures land in different
# frames of an animation that runs at the display's own pace.
sleep 6
captured=0
for index in 1 2 3; do
	if "$REPO_ROOT/lab.sh" shot "$FRAMES_DIR/frame-$index.ppm" >/dev/null 2>&1; then
		captured=$((captured + 1))
	fi
	sleep 2
done
wait "$DEMO_PID" 2>/dev/null || true

((captured == 3)) || die "only $captured of three frames were captured"
grep -q "test2d-sw: done" "$FRAMES_DIR/demo.log" || die "the demo did not run to completion; its output was: $(tail -n 3 "$FRAMES_DIR/demo.log")"

python3 "$REPO_ROOT/src/tools/check-2d-demo-frames.py" "$FRAMES_DIR/frame-1.ppm" "$FRAMES_DIR/frame-2.ppm" "$FRAMES_DIR/frame-3.ppm" || die "the captured frames do not show the scene the demo draws"
note "three live frames prove animation, analytic coverage, blending, filtering and a colour glyph"
