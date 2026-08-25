# One literal replacement, which must match exactly once.
#
# A MUTATION THAT MATCHES NOTHING IS A GATE THAT STOPPED TESTING. The specification it breaks is
# edited by people, and a mutation written against an older wording would silently become a no-op -
# which is the same failure the mutations exist to catch, in the tool that catches it.
import io
import sys

path, old, new = sys.argv[1], sys.argv[2], sys.argv[3]
text = io.open(path).read()
found = text.count(old)
if found != 1:
	sys.stderr.write("model-mutate: matches %d places, and a mutation must match exactly one\n" % found)
	raise SystemExit(1)
io.open(path, "w").write(text.replace(old, new))
