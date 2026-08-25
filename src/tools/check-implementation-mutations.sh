#!/usr/bin/env bash
# Five deliberate defects in the KERNEL, and the fixture that must catch each one.
#
# `check-model-mutations.sh` breaks the specification and requires TLC to name the invariant that
# should have caught it. This is the other half, and the one that says something about the code: it
# breaks the implementation - the five ways a capability system fails that the model spends its state
# space on - and requires the QEMU conformance fixture to fail. A test that still passes its
# corresponding mutation does not support the claim that it checks anything.
#
# WHY A COPY OF THE TREE, at a fixed path under `.build`. The kernel is compiled from source by the
# suite itself, so a mutation has to be in a source file somewhere; it must not be in the working
# tree, which somebody may be editing while this runs. The path is fixed rather than temporary so
# cargo's fingerprints - which embed absolute paths - stay valid between runs and the second mutation
# onwards is an incremental kernel build.
#
# THE COPY IS SEEDED WITH THE STAGED ARTIFACTS RATHER THAN BUILDING THEM. Userspace, the packages and
# the volume are identical under every mutation - only `src/kernel` changes - so they are copied from
# whatever this tree has already built, and only the kernel is compiled per mutation.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../.."
REPO="$PWD"
WORK="$REPO/.build/mutations/tree"
# ONE TEST, NAMED. The suite stops at its first failure, and several of these defects are also
# caught by tests that run earlier - which is good news about the suite and no evidence at all about
# the fixture this milestone built. `TEST_SELECTION` is an exact list of stable ids, read ahead of
# the tags, so what runs here is the conformance fixture and nothing else.
TAGS="object,handle,channel"
FIXTURE="kernel.object.capability_tcb_conformance_trace"

fail() {
	echo "implementation-mutations: $*" >&2
	exit 1
}

for artifact in .build/boot/system-volume-x86_64.img .build/image .build/cache; do
	[[ -e "$artifact" ]] || fail "$artifact is not here - build this tree first: ./build.sh --arch x86_64"
done

echo "implementation-mutations: preparing the copy at ${WORK#"$REPO/"}"
mkdir -p "$WORK"
rsync -a --delete --exclude '.build' --exclude '.git' --exclude 'states' "$REPO/" "$WORK/"
for part in boot state image cache; do
	mkdir -p "$WORK/.build/$part"
	rsync -a --delete "$REPO/.build/$part/" "$WORK/.build/$part/"
done

# THE STAMP IS OVER PATHS, so a copy never matches the original's however identical its bytes.
# Rewriting it here is not a way around the staleness check: the sources under the copy ARE the ones
# the copied artifacts were built from, and `src/kernel` - the only thing a mutation touches - is not
# among the inputs it covers.
(
	cd "$WORK"
	# shellcheck disable=SC1091
	source ./lib.sh
	source_digest "${VOLUME_SOURCES[@]}" >".build/state/built-x86_64-volume"
)

# The preflight enumerates the tree with `git ls-files`, so the copy needs a repository of its own.
# A FRESH ONE, INSIDE A BUILD DIRECTORY: nothing here reads, writes or otherwise touches the
# repository this script was run from. With no commit and nothing added, `-co` lists the whole tree
# as untracked, which is exactly the enumeration the preflight wants.
[[ -d "$WORK/.git" ]] || git init -q "$WORK"

pristine="$(mktemp -d)"
trap 'rm -rf "$pristine"' EXIT

# `mutate <file> <old> <new>`: restore the file from this tree and apply one literal replacement,
# which must match exactly once. A mutation that no longer matches is a gate that has quietly stopped
# testing what it names, so it is an error rather than a skip.
mutate() {
	local file="$1" old="$2" new="$3"
	cp "$REPO/$file" "$WORK/$file"
	python3 "$HERE/model-mutate.py" "$WORK/$file" "$old" "$new"
}

restore_all() {
	cp "$REPO/src/kernel/object/handle/mod.rs" "$WORK/src/kernel/object/handle/mod.rs"
	cp "$REPO/src/kernel/object/channel/mod.rs" "$WORK/src/kernel/object/channel/mod.rs"
}

# `run_mutation <name> <expected assertion text>`: the suite must FAIL, and it must fail by the
# assertion this mutation was written against. A suite that fails for another reason - a mutation
# that does not compile, most likely - proves nothing about the test that was supposed to catch it.
failures=0
run_mutation() {
	local name="$1" expected="$2"
	local log="$pristine/$3.log"
	echo "implementation-mutations: $name"
	if (cd "$WORK" && TEST_SELECTION="$FIXTURE" timeout 900 ./test.sh --arch x86_64 --tags "$TAGS") >"$log" 2>&1; then
		echo "implementation-mutations: the suite PASSED with '$name' applied - the fixture does not test it" >&2
		failures=$((failures + 1))
		return
	fi
	if ! grep -aqF "$expected" "$log"; then
		echo "implementation-mutations: '$name' failed the suite, but not by the assertion it was written against" >&2
		echo "    wanted: $expected" >&2
		grep -aE -m 5 "panicked|error(\[|:)|assertion" "$log" >&2 || true
		failures=$((failures + 1))
		return
	fi
	echo "implementation-mutations:   caught: $expected"
}

# 1. A DUPLICATE MAY WIDEN. The model's `AuthorityNeverWidens`.
mutate src/kernel/object/handle/mod.rs \
	'			if !cap.rights.contains(new_rights) {
				return Err(HandleError::AccessDenied);
			}
' \
	''
run_mutation "a duplicate may carry rights its source does not" "a duplicate may not carry rights its source does not" widening
restore_all

# 2. A STALE HANDLE IS LIVE AGAIN. The model's `StaleHandlesStayDead`.
mutate src/kernel/object/handle/mod.rs \
	'		if slot.generation != handle.generation() {
			return Err(HandleError::BadHandle);
		}
' \
	''
# The assertion that catches it is the SECOND of that scenario's, and the difference is the defect
# itself: a closed slot holds no capability, so a stale handle fails to resolve whether or not the
# generation is checked. It is the slot's REUSE that brings the old handle back to life, and only an
# assertion made after the reuse can see it.
run_mutation "a slot answers to a handle from a previous generation" "and still not to the old one" generation
restore_all

# 3. A TRANSFER COPIES INSTEAD OF MOVING. The model's `TransferIsLinear`, and the defect the
#    `take_for_transfer` path exists to prevent - its comment describes the clone-and-close it
#    replaced.
mutate src/kernel/object/handle/mod.rs \
	'		let cap = slot.cap.take().ok_or(HandleError::BadHandle)?;
		slot.reserved = true;' \
	'		let cap = slot.cap.as_ref().map(|held| Capability::new(held.object.clone(), held.rights)).ok_or(HandleError::BadHandle)?;
		slot.reserved = true;'
run_mutation "a take for transfer clones rather than moves" "a completed transfer leaves exactly one capability" clone
restore_all

# 4. A CLOSED TABLE ACCEPTS A CAPABILITY BACK. The model's `ClosedProcessCannotResurrect`.
mutate src/kernel/object/handle/mod.rs \
	'		if self.closed {
			drop(cap);
			if let Some(index) = self.slots.get_mut(handle.index() as usize) {
				index.reserved = false;
			}
			if let Some(domain) = &self.domain {
				domain.uncharge_handles(1);
			}
			return;
		}
' \
	''
run_mutation "a restore into a closed table produces a live handle" "a closed table holds nothing" resurrect
restore_all

# 5. A RECEIVE TAKES WHATEVER IS AT THE HEAD. The model's `MessageIdentityStable`, and the reason
#    `recv_identified` takes an id at all.
mutate src/kernel/object/channel/mod.rs \
	'				Some(msg) if msg.id != id => return Err(RecvRefusal::Superseded),' \
	'				Some(msg) if msg.id != id && false => return Err(RecvRefusal::Superseded),'
run_mutation "a receive takes the head rather than the message it named" "a receive naming a message that is not at the head takes nothing" identity
restore_all

if ((failures > 0)); then
	fail "$failures of 5 mutations were not caught"
fi
echo "implementation-mutations: 5 deliberate kernel defects, each caught by the assertion written against it"
