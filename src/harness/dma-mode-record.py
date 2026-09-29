#!/usr/bin/env python3
# THE HARNESS'S HALF OF THE DMA-MODE CARRIER, in one place for every input the boot contract names.
#
# The kernel's admission decision needs to know, before any driver is admitted, whether this machine
# translates DMA (`enforcing-required`) or is the explicit degraded profile (`no-iommu`). On a public
# x86_64 boot that value is a SIGNED manifest field; on every test, development, gate and
# device-tree boot it is asserted by the harness - the one component that assembled the machine and
# is trusted to say what it built. Three inputs carry it, and they carry ONE FORMAT, frozen in
# `src/boot/protocol/src/dma_mode.rs` and repeated here byte for byte:
#
#   offset  size  field
#   0       4     magic, the ASCII bytes `LSDM`
#   4       1     format version, 1
#   5       1     DMA mode: 1 = enforcing-required, 2 = no-iommu. 0 and everything else is MALFORMED;
#                   there is deliberately no encoding for absence
#   6       1     provenance: 2 = harness. 1 (`signed`) is malformed on a harness carrier
#   7       1     reserved, zero
#
#   `record MODE`                 print the eight bytes (for a fw_cfg file or an ESP file)
#   `dtb IN OUT MODE`             copy the flattened tree IN to OUT with a `/libersystem` node
#                                 (compatible "libersystem,boot-policy") carrying the record as the
#                                 byte-string property `libersystem,dma-mode`
#   `dtb-check DTB`               print the record a tree carries, or fail - the consumer's rule
#                                 mirrored, so a gate can ask a tree what it says without booting it
#
# AND THE WRONG SHAPES, for the fixtures that prove the consumer refuses them. `dtb` and `record`
# take `--shape SHAPE` where SHAPE is one of:
#   ok                    the record above (the default)
#   signed-provenance     provenance byte 1 - a replaceable medium claiming authentication
#   short                 seven bytes
#   wrong-compatible      (`dtb` only) the node with `compatible = "libersystem,something-else"`
#   other-node-name       (`dtb` only) the right record and compatible under a node named `policy`
#                         - which the consumer must still FIND, since it matches on `compatible`
#
# A pure-Python flattened-device-tree editor rather than `dtc`, which the machines this runs on do
# not all have: the tree's structure block gains one node before the root's end, its strings block
# gains two names, and the header's offsets are recomputed. The memory reservation block and every
# existing node are copied verbatim.
import os
import struct
import sys

MAGIC = b"LSDM"
VERSION = 1
MODES = {"enforcing-required": 1, "no-iommu": 2}
PROVENANCE_HARNESS = 2
NODE = b"libersystem"
COMPATIBLE = b"libersystem,boot-policy"
PROPERTY = b"libersystem,dma-mode"

FDT_MAGIC = 0xD00DFEED
FDT_BEGIN_NODE = 1
FDT_END_NODE = 2
FDT_PROP = 3
FDT_NOP = 4
FDT_END = 9


def die(message):
	print(f"dma-mode-record: {message}", file=sys.stderr)
	sys.exit(1)


def record(mode, shape="ok"):
	code = MODES.get(mode)
	if code is None:
		die(f"unknown DMA mode '{mode}' (enforcing-required or no-iommu)")
	if shape == "signed-provenance":
		return MAGIC + bytes([VERSION, code, 1, 0])
	if shape == "short":
		return MAGIC + bytes([VERSION, code, PROVENANCE_HARNESS])
	if shape not in ("ok", "wrong-compatible", "other-node-name"):
		die(f"unknown shape '{shape}'")
	return MAGIC + bytes([VERSION, code, PROVENANCE_HARNESS, 0])


# THE TREE EDITING IS THE SHARED EDITOR'S (`fdt_edit`): this program adds one node and reads it back.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import fdt_edit  # noqa: E402


def align4(n):
	return fdt_edit.align4(n)


def read_header(blob):
	try:
		return fdt_edit.read_header(blob)
	except fdt_edit.TreeError as error:
		die(str(error))


def add_node(blob, rec, shape):
	try:
		tree = fdt_edit.parse(blob)
		compatible = b"libersystem,something-else" if shape == "wrong-compatible" else COMPATIBLE
		name = "policy" if shape == "other-node-name" else NODE.decode()
		tree.root.add(fdt_edit.Node(name, [("compatible", compatible + b"\0"), (PROPERTY.decode(), rec)]))
		return fdt_edit.serialize(tree)
	except fdt_edit.TreeError as error:
		die(str(error))


def find_record(blob):
	# THE CONSUMER'S RULE, MIRRORED: the node is found by its `compatible` and not by its name, and
	# the property is returned as the bytes it holds - the caller decides whether eight is eight.
	h = read_header(blob)
	struct_block = blob[h["off_dt_struct"] : h["off_dt_struct"] + h["size_dt_struct"]]
	strings = blob[h["off_dt_strings"] : h["off_dt_strings"] + h["size_dt_strings"]]
	at = 0
	stack = []
	found = None
	while at < len(struct_block):
		token = struct.unpack(">I", struct_block[at : at + 4])[0]
		at += 4
		if token == FDT_BEGIN_NODE:
			end = struct_block.index(b"\0", at)
			stack.append({"compatible": False, "record": None})
			at = align4(end + 1)
		elif token == FDT_END_NODE:
			node = stack.pop()
			if node["compatible"] and node["record"] is not None and found is None:
				found = node["record"]
		elif token == FDT_PROP:
			length, nameoff = struct.unpack(">II", struct_block[at : at + 8])
			at += 8
			value = struct_block[at : at + length]
			at += align4(length)
			name = strings[nameoff : strings.index(b"\0", nameoff)]
			if name == b"compatible" and COMPATIBLE in value.split(b"\0"):
				stack[-1]["compatible"] = True
			if name == PROPERTY:
				stack[-1]["record"] = value
		elif token == FDT_NOP:
			pass
		elif token == FDT_END:
			break
		else:
			die(f"unknown structure token {token:#x}")
	return found


def main(argv):
	shape = "ok"
	if len(argv) >= 2 and argv[-2] == "--shape":
		shape = argv[-1]
		argv = argv[:-2]
	if len(argv) == 2 and argv[0] == "record":
		sys.stdout.buffer.write(record(argv[1], shape))
		return
	if len(argv) == 4 and argv[0] == "dtb":
		with open(argv[1], "rb") as source:
			blob = source.read()
		rec = record(argv[3], shape)
		out = add_node(blob, rec, shape)
		if shape in ("ok", "other-node-name") and find_record(out) != rec:
			die("the tree written does not read back the record it was given")
		with open(argv[2], "wb") as destination:
			destination.write(out)
		return
	if len(argv) == 2 and argv[0] == "dtb-check":
		with open(argv[1], "rb") as source:
			found = find_record(source.read())
		if found is None:
			die("the tree carries no boot-policy node with a DMA-mode property")
		if len(found) != 8:
			die(f"the record is {len(found)} bytes, not 8 - malformed")
		sys.stdout.buffer.write(found)
		return
	die("usage: record MODE [--shape S] | dtb IN OUT MODE [--shape S] | dtb-check DTB")


if __name__ == "__main__":
	main(sys.argv[1:])
