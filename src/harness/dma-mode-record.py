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


def align4(n):
	return (n + 3) & ~3


def read_header(blob):
	if len(blob) < 40:
		die("not a flattened device tree: shorter than its header")
	fields = struct.unpack(">10I", blob[:40])
	if fields[0] != FDT_MAGIC:
		die("not a flattened device tree: bad magic")
	names = ["magic", "totalsize", "off_dt_struct", "off_dt_strings", "off_mem_rsvmap", "version", "last_comp_version", "boot_cpuid_phys", "size_dt_strings", "size_dt_struct"]
	return dict(zip(names, fields))


def string_offset(strings, name):
	# The strings block is NUL-terminated names; an existing one is reused, a new one appended.
	at = 0
	while at < len(strings):
		end = strings.index(b"\0", at)
		if strings[at:end] == name:
			return at, strings
		at = end + 1
	return len(strings), strings + name + b"\0"


def prop_token(nameoff, value):
	return struct.pack(">III", FDT_PROP, len(value), nameoff) + value + b"\0" * (align4(len(value)) - len(value))


def node_token(name):
	return struct.pack(">I", FDT_BEGIN_NODE) + name + b"\0" + b"\0" * (align4(len(name) + 1) - len(name) - 1)


def reserved_block(blob, header):
	# Every (address, size) pair through the (0, 0) terminator, wherever the header put the block.
	at = header["off_mem_rsvmap"]
	out = b""
	while at + 16 <= len(blob):
		pair = blob[at : at + 16]
		out += pair
		at += 16
		if pair == b"\0" * 16:
			return out
	die("the memory reservation block has no terminator")


def add_node(blob, rec, shape):
	h = read_header(blob)
	struct_block = blob[h["off_dt_struct"] : h["off_dt_struct"] + h["size_dt_struct"]]
	strings = blob[h["off_dt_strings"] : h["off_dt_strings"] + h["size_dt_strings"]]
	# The root node's END_NODE is where the new node goes. Found by walking the tokens, so a tree
	# whose tail carries NOPs is handled rather than guessed at.
	at = 0
	depth = 0
	root_end = None
	while at < len(struct_block):
		token = struct.unpack(">I", struct_block[at : at + 4])[0]
		at += 4
		if token == FDT_BEGIN_NODE:
			depth += 1
			end = struct_block.index(b"\0", at)
			at = align4(end + 1)
		elif token == FDT_END_NODE:
			depth -= 1
			if depth == 0:
				root_end = at - 4
		elif token == FDT_PROP:
			length = struct.unpack(">I", struct_block[at : at + 4])[0]
			at += 8 + align4(length)
		elif token == FDT_NOP:
			pass
		elif token == FDT_END:
			break
		else:
			die(f"unknown structure token {token:#x}")
	if root_end is None:
		die("the tree has no root node to append to")
	compat_off, strings = string_offset(strings, b"compatible")
	prop_off, strings = string_offset(strings, PROPERTY)
	compatible = b"libersystem,something-else" if shape == "wrong-compatible" else COMPATIBLE
	name = b"policy" if shape == "other-node-name" else NODE
	node = node_token(name) + prop_token(compat_off, compatible + b"\0") + prop_token(prop_off, rec) + struct.pack(">I", FDT_END_NODE)
	new_struct = struct_block[:root_end] + node + struct_block[root_end:]
	rsv = reserved_block(blob, h)
	# Layout: header, the reservation block (8-byte aligned), the structure block (4-byte aligned),
	# the strings block. Every offset is recomputed from the blocks rather than adjusted, which is
	# what keeps this correct for a tree whose blocks were not in this order.
	off_rsv = 40
	off_struct = align4(off_rsv + len(rsv))
	off_strings = off_struct + len(new_struct)
	total = off_strings + len(strings)
	header = struct.pack(">10I", FDT_MAGIC, total, off_struct, off_strings, off_rsv, h["version"], h["last_comp_version"], h["boot_cpuid_phys"], len(strings), len(new_struct))
	return header + rsv + b"\0" * (off_struct - off_rsv - len(rsv)) + new_struct + strings


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
