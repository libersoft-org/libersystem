#!/usr/bin/env python3
"""The pixel checks over the frames a live 2D demo run captured.

WHAT A CAPTURE CAN PROVE THAT A LOG CANNOT. The guest gate already reads what the demo SAYS it did -
which phase, how many rectangles, which damage - and a renderer that draws nothing at all says the
same things. These are the properties that need pixels: that the picture MOVES, that an edge at a
shallow angle is antialiased rather than a staircase, that a blend is a mix rather than one of its
operands, that a filtered region is filtered, and that the line of text is drawn with a colour glyph
in it that is NOT the run's paint.

THE REGIONS ARE COMPUTED FROM THE FRAME'S OWN SIZE, in the fractions the scene lays itself out by, so
a capture at any resolution is checked at the same places in the drawing.
"""
import sys


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
	def __init__(self, path):
		self.path = path
		self.width, self.height, self.pixels = read_ppm(path)

	def at(self, x, y):
		offset = (y * self.width + x) * 3
		return self.pixels[offset], self.pixels[offset + 1], self.pixels[offset + 2]

	def row(self, y, x0, x1):
		return [self.at(x, y) for x in range(x0, x1)]


def differing(first, second):
	"""How many pixels differ by more than a quantisation step."""
	count = 0
	for y in range(0, first.height, 4):
		for x in range(0, first.width, 4):
			left = first.at(x, y)
			right = second.at(x, y)
			if max(abs(a - b) for a, b in zip(left, right)) > 2:
				count += 1
	return count


def main():
	if len(sys.argv) < 4:
		raise SystemExit('usage: check-2d-demo-frames.py FRAME.ppm FRAME.ppm FRAME.ppm...')
	frames = [Frame(path) for path in sys.argv[1:]]
	width, height = frames[0].width, frames[0].height
	for frame in frames:
		if (frame.width, frame.height) != (width, height):
			raise SystemExit(f'{frame.path}: {frame.width}x{frame.height} where the first frame is {width}x{height}')
	problems = []

	# ANIMATION: the picture is not the same picture. Sampled every fourth pixel, so a difference has
	# to be an area rather than a stray pixel.
	moved = max(differing(frames[0], other) for other in frames[1:])
	if moved < 32:
		problems.append(f'the frames barely differ ({moved} sampled pixels), so nothing is animating')

	# ANTIALIASING: the sliver crosses the frame at a shallow angle a little above the middle, and an
	# antialiased edge means INTERMEDIATE values - a staircase has only the two flat colours.
	band_top = int(height * 0.48)
	band_bottom = int(height * 0.58)
	intermediate = 0
	for y in range(band_top, band_bottom):
		for (red, green, blue) in frames[0].row(y, int(width * 0.2), int(width * 0.8)):
			level = (red + green + blue) / 3.0
			if 80.0 < level < 210.0:
				intermediate += 1
	if intermediate < 16:
		problems.append(f'the shallow edge has {intermediate} partially covered pixel(s), which is a staircase rather than analytic coverage')

	# BLENDING: the group-opacity layer's two children overlap, and the overlap is neither child's
	# colour - a renderer that drew the second child opaquely would show it exactly.
	overlap = frames[0].at(int(width * 0.645), int(height * 0.70))
	if overlap[0] < 20 and overlap[1] < 20:
		problems.append(f'the group-opacity overlap is {overlap}, which is neither of its children blended')

	# FILTERING: the projective image is bilinearly sampled, so its checkerboard carries values
	# BETWEEN black and white; nearest sampling would give only the two.
	shades = set()
	for y in range(int(height * 0.10), int(height * 0.24)):
		for (red, _green, _blue) in frames[0].row(y, int(width * 0.53), int(width * 0.58)):
			shades.add(red // 16)
	if len(shades) < 4:
		problems.append(f'the transformed image has {len(shades)} distinct shade band(s), so it is not being filtered')

	# TEXT, AND THE COLOUR GLYPH IN IT. The line is drawn in white; the last glyph brings its OWN
	# colours, so the row holds both a white run and a saturated one.
	white = 0
	coloured = 0
	for y in range(int(height * 0.07), int(height * 0.12)):
		for (red, green, blue) in frames[0].row(y, 0, int(width * 0.2)):
			if red > 200 and green > 200 and blue > 200:
				white += 1
			elif red > 150 and blue < 90 and green > 80:
				coloured += 1
	if white < 20:
		problems.append(f'the line of text has {white} white pixel(s), so it is not being drawn')
	if coloured < 4:
		problems.append(f'the colour glyph contributes {coloured} pixel(s) of its own palette, so it is drawn in the run\'s paint or not at all')

	for problem in problems:
		print(f'2d-demo-frames: {problem}')
	if problems:
		return 1
	print(f'2d-demo-frames: {len(frames)} frame(s) at {width}x{height}: animation, analytic coverage, blending, filtering and a colour glyph all present')
	return 0


if __name__ == '__main__':
	sys.exit(main())
