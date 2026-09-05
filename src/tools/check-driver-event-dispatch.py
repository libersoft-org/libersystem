#!/usr/bin/env python3
"""P02M0177: prove admission controls production effects and regressions reject old defects."""
import argparse
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "src/user/services/core/src/device_manager.rs"


def code(source):
    # Comments and literals cannot satisfy a wiring requirement or change brace depth.
    return re.sub(r'//[^\n]*|/\*.*?\*/|b?"(?:\\.|[^"\\])*"', ' ', source, flags=re.S)


def block(source, opening):
    depth = 0
    for at in range(opening, len(source)):
        if source[at] == '{':
            depth += 1
        elif source[at] == '}':
            depth -= 1
            if depth == 0:
                return source[opening + 1:at], at + 1
    raise ValueError("unterminated source block")


def compact(source):
    return re.sub(r'\s+', '', source)


def check(source):
    source = code(source)
    start = source.index('unsafe fn advance(')
    advance, _ = block(source, source.index('{', start))
    normalized = compact(advance)
    expected = 'unsafe{loop{letdecision={'
    if not normalized.startswith(expected):
        raise ValueError("advance must enter one unconditional scoped dispatch before its effects")
    decision_at = advance.index('let decision =')
    dispatch, end = block(advance, advance.index('{', decision_at))
    if compact(dispatch) != (
        'letSome(popped)=node.pop()else{break};'
        'ifletSome(teardown)=node.teardown.as_mut(){'
        'teardown.pending.note(popped);continue;}'
        'driver_binding::reduce_event(node.record.state,popped)'
    ):
        raise ValueError("dispatch must consume the normal popped event after the teardown redirect")
    if advance.count('reduce_event(') != 1:
        raise ValueError("one event must have exactly one reducer decision")
    remainder = advance[end:]
    admitted = ';letdriver_binding::EventDecision::Admitted{event,next_state,cause,planned_stop}=decisionelse{'
    if not compact(remainder).startswith(admitted):
        raise ValueError("only an admitted result can provide the effect event and decision fields")
    refusal_at = remainder.index('else')
    refusal, refusal_end = block(remainder, remainder.index('{', refusal_at))
    if not compact(refusal).endswith('continue;'):
        raise ValueError("refusal must skip every effect")
    if not compact(remainder[refusal_end:]).startswith(';matchevent{'):
        raise ValueError("the admitted event must be the effect match's scrutinee")
    if 'popped' in remainder or 'node.pop(' in remainder:
        raise ValueError("the raw event must be out of scope before effects")
    effect_at = remainder.index('match event')
    effects, _ = block(remainder, remainder.index('{', effect_at))
    terminal = {'Ready', 'Failed', 'TimedOut', 'Stopped', 'Exited', 'Closed', 'Wedged'}
    arms = list(re.finditer(r'BindingEvent::(\w+)\s*\{[^}]*\}', effects))
    for index, arm in enumerate(arms):
        if arm.group(1) not in terminal:
            continue
        body = effects[arm.end():arms[index + 1].start() if index + 1 < len(arms) else len(effects)]
        if re.search(r'node\.record\.state|accepts_terminal_frame|BindingState::|FailureCause::|stop_intent|planned_stop\s*=', body):
            raise ValueError(f"{arm.group(1)} recomputes a reducer decision")
        for move in re.finditer(r'move_to\(([^)]*)\)', body):
            if compact(move.group(1)) != 'next,cause':
                raise ValueError(f"{arm.group(1)} chooses its own transition")
    if 'ifletSome(next)=next_state{' not in compact(effects) or 'move_to(next,cause)' not in compact(effects):
        raise ValueError("READY must consume the carried transition")
    if 'letSome(cause)=causeelse{continue};' not in compact(remainder):
        raise ValueError("the ending path must consume the carried cause")
    if 'move_to(next,Some(cause))' not in compact(remainder):
        raise ValueError("the ending path must consume the carried transition")
    if 'teardown.planned_stop=planned_stop;' not in compact(remainder):
        raise ValueError("teardown must consume the carried planned-stop classification")
    if 'driver_binding::sort_probe_slots(&mutblock_entries,&catalogue.entries,|provider|provider.id);' not in compact(source):
        raise ValueError("production disk probing must use the tested probe derivation")
    if 'driver_binding::next_handoff_slot(&self.entries,|provider|provider.kind==kind&&provider.handle!=0,|provider|provider.id)' not in compact(source):
        raise ValueError("production role handoff must use its own tested derivation")


def rejected_mutations(source):
    start = source.index('\t\t\tlet decision = {', source.index('unsafe fn advance('))
    # Operate on exact source spans separately: stripping comments changes offsets.
    prefix_end = source.index('\t\t\tmatch event {', start)
    prefix = source[start:prefix_end]
    call = 'driver_binding::reduce_event(node.record.state, popped)'
    raw = '\t\t\tlet Some(event) = node.pop() else { break };\n'
    mutations = {
        'deleted dispatch': source.replace(prefix, raw, 1),
        'dispatch inside one arm': source.replace(prefix, raw, 1).replace('BindingEvent::Ready { .. } => {\n', 'BindingEvent::Ready { .. } => {\n let _ = driver_binding::reduce_event(node.record.state, event);\n', 1),
        'discarded result and raw-event bypass': source.replace(prefix, raw + '\t\t\tlet _ = driver_binding::reduce_event(node.record.state, event);\n', 1),
        'arm-local state predicate': source.replace('BindingEvent::TimedOut { .. } => {\n', 'BindingEvent::TimedOut { .. } => {\n if !node.record.state.accepts_terminal_frame() { continue; }\n', 1),
        'arm-local transition': source.replace('node.record.move_to(next, cause)', 'node.record.move_to(BindingState::Online, cause)', 1),
    }
    assert call in prefix
    with tempfile.TemporaryDirectory(prefix='liber-dispatch-wiring-') as directory:
        for number, (name, mutant) in enumerate(mutations.items(), 1):
            path = Path(directory) / f'device_manager-{number}.rs'
            path.write_text(mutant)
            try:
                check(path.read_text())
            except ValueError:
                print(f'driver-event-dispatch: rejected {name}')
            else:
                raise ValueError(f"source check accepted {name}")


def regression_mutations():
    binding = ROOT / 'src/user/libs/driver/binding'
    original = (binding / 'src/lib.rs').read_text()
    timeout = original.index('BindingEvent::TimedOut { .. } => {', original.index('pub fn reduce_event('))
    guard_end = original.index('\n\t\t\t(Some(', timeout)
    guard_start = original.index('\n\t\t\tif !state.accepts_terminal_frame()', timeout)
    stale_timeout = original[:guard_start] + original[guard_end:]
    sort = 'slots.sort_unstable_by_key(|&slot| (entries[slot].as_ref().map(|entry| provider_address(id_of(entry))), slot));'
    assert sort in original
    unsorted_probes = original.replace(sort, 'let _ = (slots, entries, id_of);', 1)
    with tempfile.TemporaryDirectory(prefix='liber-driver-regressions-') as directory:
        root = Path(directory)
        shutil.copytree(binding / 'src', root / 'src')
        protocol = (binding.parent / 'protocol').as_posix()
        (root / 'Cargo.toml').write_text('[package]\nname="driver-binding-regression"\nedition="2024"\n[dependencies]\ndriver-protocol = { path = "' + protocol + '" }\n')
        for name, mutant, test in [
            ('missing timeout admission', stale_timeout, 'a_queued_timeout_cannot_disconnect_an_already_ready_driver'),
            ('publication-order probes', unsorted_probes, 'disk_probes_and_role_handoff_name_the_same_provider_at_every_index'),
        ]:
            (root / 'src/lib.rs').write_text(mutant)
            result = subprocess.run(['cargo', 'test', '--offline', '--manifest-path', str(root / 'Cargo.toml'), '--target-dir', str(root / 'target'), '--lib', test], cwd=ROOT / 'src', text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            if result.returncode != 101 or 'test result: FAILED' not in result.stdout or f'tests::{test} ... FAILED' not in result.stdout:
                raise ValueError(f'{name}: expected the named assertion to fail, not a build failure:\n{result.stdout}')
            print(f'driver-event-dispatch: host regression rejected {name}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, help='validate one source copy without self-tests')
    args = parser.parse_args()
    try:
        source = (args.source or SOURCE).read_text()
        check(source)
        if args.source is None:
            rejected_mutations(source)
            regression_mutations()
        print('driver-event-dispatch: passed')
    except (ValueError, OSError) as error:
        parser.exit(1, f'driver-event-dispatch: {error}\n')


if __name__ == '__main__':
    main()
