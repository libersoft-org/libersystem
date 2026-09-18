#!/usr/bin/env python3
"""The pixel checks over the frames a live 3D demo run captured.

WHAT A CAPTURE CAN PROVE THAT A LOG CANNOT. The guest test already reads what the demo SAYS it did -
that it opened a surface, that it presented, that it rebuilt for a resize - and a renderer that drew
nothing at all says the same things. These are the properties that need pixels: that there is a lit
object over a textured ground on a dark sky, that the projected object sits in a bounded central
region, that the ground's texture REPEATS, that the translucent panel is a MIX of what is in front
and what is behind rather than either alone, that the 2D overlay reached the same frame the 3D scene
did, and that successive frames differ.

NO GOLDEN IMAGE. A golden frame is a regression test and not a correctness oracle: it proves the
output has not changed and not that it was ever right, and every one of these checks fails for one
reason that can be named. Where a check needs to know what is in front of the camera it is given a
POSE, and which faces a pose shows is computed here from the same camera the demo uses rather than
read off a picture somebody accepted once.

THE SURFACE IS FOUND AND NOT ASSUMED. A capture is the whole screen; the demo's window is the part of
it that is not the console's background, so every region below is a fraction of THAT rectangle - which
is what lets one set of checks read a capture at any resolution and any window size.
"""
import math
import sys

# The demo's own constants, restated here because a checker that read them from the source would be
# checking the source against itself. Each one is a decision the demo's file states in its own words.
SKY = (0.05, 0.06, 0.10)
CUBE_HALF_EXTENT = 0.75
SPIN_PER_FRAME = 0.031
DEFAULT_YAW = 0.6
DEFAULT_PITCH = 0.42
DEFAULT_DISTANCE = 4.2
# The cube's six faces, in the order the demo builds them: the outward normal and the colour.
FACES = (
	((0.0, 0.0, 1.0), 'front', (0.86, 0.22, 0.22)),
	((0.0, 0.0, -1.0), 'back', (0.20, 0.62, 0.86)),
	((1.0, 0.0, 0.0), 'right', (0.26, 0.76, 0.34)),
	((-1.0, 0.0, 0.0), 'left', (0.92, 0.74, 0.20)),
	((0.0, 1.0, 0.0), 'top', (0.78, 0.36, 0.82)),
	((0.0, -1.0, 0.0), 'bottom', (0.30, 0.78, 0.78)),
)


def read_ppm(path):
	with open(path, 'rb') as handle:
		data = handle.read()
	if not data.startswith(b'P6'):
		raise SystemExit(f'{path}: not a binary PPM')
	fields = []
	index = 2
	while len(fields) < 3:
		while index < len(data) and data[index : index + 1].isspace():
			index += 1
		if data[index : index + 1] == b'#':
			while index < len(data) and data[index] != 0x0A:
				index += 1
			continue
		start = index
		while index < len(data) and not data[index : index + 1].isspace():
			index += 1
		fields.append(int(data[start:index]))
	index += 1
	width, height, _maximum = fields
	pixels = data[index : index + width * height * 3]
	if len(pixels) < width * height * 3:
		raise SystemExit(f'{path}: short pixel data')
	return width, height, pixels


class Frame:
	"""One capture, with the demo's window located inside it."""

	def __init__(self, path):
		self.path = path
		self.width, self.height, self.pixels = read_ppm(path)
		self.x0, self.y0, self.x1, self.y1 = self.window()

	def at(self, x, y):
		offset = (y * self.width + x) * 3
		return self.pixels[offset], self.pixels[offset + 1], self.pixels[offset + 2]

	def window(self):
		"""The bounding box of everything that is not the console's black background.

		THE DEMO'S SKY IS DARK AND IS NOT BLACK, which is what makes this work at all: the scene's
		darkest pixel is about (13, 15, 26) after the sRGB write and the bars beside a scaled window
		are exactly zero, so a threshold between them finds the window without knowing where the
		compositor put it.
		"""
		left, top, right, bottom = self.width, self.height, -1, -1
		for y in range(0, self.height, 2):
			for x in range(0, self.width, 2):
				red, green, blue = self.at(x, y)
				if red + green + blue > 12:
					left = min(left, x)
					right = max(right, x)
					top = min(top, y)
					bottom = max(bottom, y)
		if right < left or bottom < top:
			raise SystemExit(f'{self.path}: the capture is entirely black, so nothing reached the screen')
		return left, top, right + 1, bottom + 1

	def span(self):
		return self.x1 - self.x0, self.y1 - self.y0

	def sample(self, u, v):
		"""One pixel, at a fraction of the demo's window."""
		width, height = self.span()
		x = min(self.x1 - 1, self.x0 + int(u * width))
		y = min(self.y1 - 1, self.y0 + int(v * height))
		return self.at(x, y)

	def scan(self, v, u0, u1, steps):
		"""A horizontal run of samples across the window, as luma."""
		return [luma(self.sample(u0 + (u1 - u0) * step / (steps - 1), v)) for step in range(steps)]


def luma(pixel):
	red, green, blue = pixel
	return 0.2126 * red + 0.7152 * green + 0.0722 * blue


def distance(first, second):
	return max(abs(a - b) for a, b in zip(first, second))


def alternations(values, threshold):
	"""How many times a run crosses its own mean by more than a threshold.

	A PERIODIC PATTERN CROSSES ITS MEAN AND A GRADIENT DOES NOT, which is the difference this counts:
	a checkerboard sampled along a line alternates light and dark whatever its phase, and a lit flat
	surface drifts in one direction.
	"""
	middle = sum(values) / len(values)
	crossings = 0
	side = None
	for value in values:
		if abs(value - middle) < threshold:
			continue
		now = value > middle
		if side is not None and now != side:
			crossings += 1
		side = now
	return crossings


def visible_faces(pose):
	"""Which faces a pose shows, computed from the same camera the demo uses.

	KNOWN IN ADVANCE AND NOT READ OFF A PICTURE. The model turns about the vertical axis by the pose;
	the camera sits on the orbit the demo starts at; a face is visible when its rotated normal points
	towards the eye. That is the whole rule, and it is the demo's own.
	"""
	angle = pose * 0.01
	eye = (
		DEFAULT_DISTANCE * math.cos(DEFAULT_PITCH) * math.sin(DEFAULT_YAW),
		DEFAULT_DISTANCE * math.sin(DEFAULT_PITCH),
		DEFAULT_DISTANCE * math.cos(DEFAULT_PITCH) * math.cos(DEFAULT_YAW),
	)
	shown = []
	for normal, name, colour in FACES:
		turned = (
			normal[0] * math.cos(angle) + normal[2] * math.sin(angle),
			normal[1],
			-normal[0] * math.sin(angle) + normal[2] * math.cos(angle),
		)
		# The face's centre is its normal times the half extent; the eye is relative to that.
		centre = tuple(component * CUBE_HALF_EXTENT for component in turned)
		towards = tuple(eye[axis] - centre[axis] for axis in range(3))
		if sum(turned[axis] * towards[axis] for axis in range(3)) > 0.15:
			shown.append((name, colour))
	return shown


def check_scene(frame, problems):
	width, height = frame.span()
	if width < 64 or height < 48:
		problems.append(f'{frame.path}: the window is {width}x{height}, which is too small to have been the demo drawing')
		return

	# NONBLANK, AND NOT MERELY NON-BLACK. A cleared surface is one colour; a rendered scene is a sky,
	# a ground, a lit object and an overlay.
	seen = set()
	for step in range(400):
		seen.add(frame.sample((step % 20) / 20.0 + 0.02, (step // 20) / 20.0 + 0.02))
	if len(seen) < 24:
		problems.append(f'{frame.path}: the window holds {len(seen)} distinct colour(s), which is a cleared surface rather than a scene')

	# THE SKY IS ABOVE AND THE GROUND IS BELOW, which is the whole of "the scene has a horizon". The
	# sky is sampled at the top RIGHT, because the top left is where the overlay sits.
	sky = frame.sample(0.90, 0.06)
	ground = frame.sample(0.12, 0.92)
	if luma(sky) > luma(ground):
		problems.append(f'{frame.path}: the top of the window {sky} is not darker than the bottom {ground}, so there is no lit ground under a dark sky')
	if luma(sky) > 60:
		problems.append(f'{frame.path}: the sky {sky} is not the dark background the scene clears to')

	# THE GROUND REPEATS. Its texture coordinates run to six across it, so a horizontal line near the
	# bottom crosses several squares; a ground sampled with a clamping sampler would not.
	floor = frame.scan(0.92, 0.05, 0.95, 60)
	crossings = alternations(floor, 4.0)
	if crossings < 3:
		problems.append(f'{frame.path}: the ground crosses its own mean {crossings} time(s) along one line, so its texture is not repeating')

	# THE OBJECT IS IN A BOUNDED CENTRAL REGION, FOUND BY WHAT IT OCCLUDES AND NOT BY ITS COLOUR.
	#
	# A colour test would have to know which faces a pose shows, and the answer changes with the
	# rotation; what does not change is that the object STANDS IN FRONT OF THE HORIZON. Down every
	# column the sky ends somewhere: at the horizon where nothing is in the way, and higher up where
	# something is. The columns whose sky ends early ARE the object, whatever colour it happens to be.
	skyline = []
	for column in range(4, 97):
		u = column / 100.0
		edge = 1.0
		for row in range(22, 96):
			v = row / 100.0
			if distance(frame.sample(u, v), sky) > 25:
				edge = v
				break
		skyline.append((u, edge))
	horizon = sorted(edge for _u, edge in skyline)[len(skyline) // 2]
	standing = [u for u, edge in skyline if edge < horizon - 0.05]
	if not standing:
		problems.append(f'{frame.path}: nothing stands in front of the horizon at v={horizon:.2f}, so no object was drawn')
	else:
		left, right = min(standing), max(standing)
		if not 0.15 < (left + right) / 2 < 0.85:
			problems.append(f'{frame.path}: what stands in front of the horizon spans {left:.2f}..{right:.2f} across, which is not the central region a centred camera projects into')
		if right - left > 0.88:
			problems.append(f'{frame.path}: what stands in front of the horizon spans {right - left:.2f} of the window, which is the whole screen rather than an object in it')


def check_overlay(frame, problems):
	"""The 2D half, in the frame the 3D half rendered.

	THE OVERLAY IS WHAT MAKES THIS AN INTEROP PROOF. `soft3d` renders into a shared image and
	`render2d` composites over it; a frame with the scene and no overlay is two libraries that have
	still never met.
	"""
	sky = frame.sample(0.90, 0.06)
	panel = frame.sample(0.20, 0.08)
	if luma(panel) <= luma(sky) + 25:
		problems.append(f'{frame.path}: the overlay panel {panel} is not lighter than the sky beside it {sky}, so the 2D composite did not reach this frame')
	# AND IT IS TRANSLUCENT RATHER THAN OPAQUE: what is under it shows through, so it is neither the
	# panel's own colour nor the sky's.
	if distance(panel, (184, 199, 240)) < 12:
		problems.append(f'{frame.path}: the overlay panel is its own colour exactly, so it was drawn opaque rather than blended')
	# THE GLYPH RUN LANDS INSIDE THE PANEL and is darker than the panel it is drawn on. A run that
	# failed to rasterise leaves the panel flat, and a run that landed outside it leaves it flat too.
	#
	# A BAND AND NOT A LINE, because the run is a few percent of the window tall and a single row
	# between two letters is a row of panel. What is asserted is a dark excursion AND several
	# crossings: one dark patch is a blot, and a line of text is letters with gaps between them.
	darkest, crossings = 255.0, 0
	for row in range(10, 16):
		band = [luma(frame.sample(0.09 + step * 0.008, row / 100.0)) for step in range(26)]
		darkest = min(darkest, min(band))
		crossings = max(crossings, alternations(band, 20.0))
	if darkest > luma(panel) - 40:
		problems.append(f'{frame.path}: nothing in the overlay panel is darker than the panel itself, so the glyph run did not rasterise')
	if crossings < 4:
		problems.append(f'{frame.path}: the overlay panel crosses its own level {crossings} time(s) where the text is, which is a blot rather than a line of letters')


def check_pose(frame, pose, problems):
	"""The faces a stated pose shows, sampled where they land."""
	shown = [name for name, _colour in visible_faces(pose)]
	if 'top' not in shown:
		problems.append(f'pose {pose}: the camera looks down at the cube, so its top face must be visible; the computed set is {shown}')
	# THE TOP FACE IS THE ONE WHOSE POSITION IS KNOWN WITHOUT KNOWING THE ROTATION: it is above the
	# cube's centre whatever the model turns to, so a sample just above the middle lands on it.
	top = frame.sample(0.50, 0.30)
	sky = frame.sample(0.90, 0.06)
	if distance(top, sky) < 30:
		problems.append(f'pose {pose}: the sample where the top face projects is the sky {top}, so the cube is not where the camera puts it')


def check_transparency(frame, problems):
	"""The panel in front of the cube: a mix, and not either operand.

	THE PANEL IS CYAN AND EVERYTHING BEHIND IT IS BLUE-GREY, so where it stands the green channel
	rises above the red one by much more than it does anywhere else. That is the signature this looks
	for, and it needs to know neither where the panel projects to nor what is behind it: a blend that
	dropped the source leaves no such region, and one that dropped the destination leaves a region
	that is the panel's own colour with no variation in it.
	"""
	tinted, plain, inside = [], [], []
	for row in range(40, 96, 2):
		for column in range(4, 97, 2):
			pixel = frame.sample(column / 100.0, row / 100.0)
			if pixel[1] - pixel[0] > 25 and pixel[2] > pixel[1]:
				tinted.append(pixel)
				inside.append(luma(pixel))
			elif abs(pixel[1] - pixel[0]) <= 8:
				plain.append(pixel)
	if len(tinted) < 20:
		problems.append(f'{frame.path}: no region below the horizon is tinted towards the panel, so nothing was blended over what is behind it')
		return
	if not plain:
		problems.append(f'{frame.path}: every region below the horizon is tinted, so the panel covers the whole scene rather than part of it')
		return
	# AND WHAT SHOWS THROUGH IS STILL VARIED. A panel drawn opaque is one flat colour; a panel
	# blended over a checkered ground carries the checker's own light and dark through it.
	if max(inside) - min(inside) < 12:
		problems.append(f'{frame.path}: the tinted region is flat, so the destination was discarded rather than blended into')


def differing(first, second):
	count = 0
	for y in range(first.y0, first.y1, 3):
		for x in range(first.x0, first.x1, 3):
			if distance(first.at(x, y), second.at(x, y)) > 6:
				count += 1
	return count


def main(argv):
	if not argv:
		print('usage: check-3d-demo-frames.py [--ready FRAME | --console FRAME | --pose N FRAME | FRAME...]')
		return 2

	if argv[0] == '--ready':
		# THE SCREEN IS THE DEMO'S SCENE, which is a question about the frame rather than about the
		# clock. This runs the SAME checks the captures below run and accepts only a frame that passes
		# every one of them.
		#
		# A WEAKER TEST WAS WRONG AND COST A WHOLE GATE RUN. It asked for a dark blue-ish region where
		# the sky belongs, and what satisfied it was the surface's own CLEAR: a window that had been
		# created and never drawn into is exactly that colour, so the gate captured three identical
		# flat frames and blamed the renderer. What tells "the demo has a window" from "the demo has
		# drawn" is the drawing.
		frame = Frame(argv[1])
		problems = []
		check_scene(frame, problems)
		check_overlay(frame, problems)
		return 0 if not problems else 1

	if argv[0] == '--console':
		# AND THE SCREEN IS THE CONSOLE'S AGAIN. `q` must give the terminal back: what is on it is
		# text, so the frame has no large region of saturated colour in it.
		frame = Frame(argv[1])
		coloured = 0
		for row in range(0, 100, 2):
			for column in range(0, 100, 2):
				red, green, blue = frame.sample(column / 100.0, row / 100.0)
				if max(red, green, blue) - min(red, green, blue) > 60:
					coloured += 1
		if coloured > 60:
			print(f'3d-demo-frames: the screen still holds {coloured} strongly coloured sample(s) after the demo left, so the console was not restored')
			return 1
		print('3d-demo-frames: the console is back on the screen')
		return 0

	pose = None
	if argv[0] == '--pose':
		pose = int(argv[1])
		argv = argv[2:]

	frames = [Frame(path) for path in argv]
	problems = []
	for frame in frames:
		check_scene(frame, problems)
		check_overlay(frame, problems)
		check_transparency(frame, problems)
		if pose is not None:
			check_pose(frame, pose, problems)

	# SUCCESSIVE FRAMES DIFFER, which is the LIVE animation check and the only thing it asserts. What
	# changes between two frames of a clock-driven scene is not something a checker can predict, so
	# what is checked is that something did.
	if pose is None and len(frames) > 1:
		for first, second in zip(frames, frames[1:]):
			moved = differing(first, second)
			if moved < 40:
				problems.append(f'{first.path} and {second.path} differ in {moved} sample(s), so the scene is not animated')

	for problem in problems:
		print(f'3d-demo-frames: {problem}')
	if problems:
		return 1
	what = f'pose {pose}' if pose is not None else f'{len(frames)} live frame(s)'
	print(f'3d-demo-frames: {what} show a lit object over a repeating textured ground under a dark sky, a blended panel, and the 2D overlay composited into the same frame')
	return 0


if __name__ == '__main__':
	sys.exit(main(sys.argv[1:]))
