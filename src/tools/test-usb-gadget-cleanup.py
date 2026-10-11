#!/usr/bin/env python3
"""Run the real gadget cleanup shell against private sysfs and module-command fixtures."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
SOURCE = HERE.parent / 'harness/usb-gadget.sh'


class UsbGadgetCleanupTests(unittest.TestCase):
    def exercise(self, *, old=('oldpcm',), explicit=('function',), dependencies=None,
                 new_driver=False, busy=(), stuck=False, deny=False, deny_module='', missing_old=False,
                 mutate_cascade=False, entry='unload_owned_modules', mountinfo=''):
        with tempfile.TemporaryDirectory(prefix='gadget-cleanup-') as temporary:
            root = Path(temporary)
            state = root / 'state with spaces'
            state.mkdir()
            modules = root / 'sys/module'
            modules.mkdir(parents=True)
            dependencies = dependencies or {'function': ['dependency'], 'dependency': ['oldpcm']}
            names = set(old) | set(explicit) | set(dependencies)
            names.update(dep for values in dependencies.values() for dep in values)
            if new_driver:
                names.add('newhid')
            for name in names:
                module = modules / name
                (module / 'holders').mkdir(parents=True)
                (module / 'initstate').write_text('live\n')
                (module / 'refcnt').write_text('1\n' if name in busy else '0\n')
            for holder, deps in dependencies.items():
                for dependency in deps:
                    (modules / dependency / 'holders' / holder).symlink_to(modules / holder)
                    (modules / dependency / 'refcnt').write_text(str(len(list((modules / dependency / 'holders').iterdir())))+'\n')
            # Current Linux also exposes this regular attribute and builtin module directories.
            (modules / 'compression').write_text('zstd\n')
            (modules / 'builtin/parameters').mkdir(parents=True)
            for bus in ('usb', 'hid'):
                (root / f'sys/bus/{bus}/drivers').mkdir(parents=True)
            if new_driver:
                driver = root / 'sys/bus/hid/drivers/new-device'
                driver.mkdir()
                (driver / 'module').symlink_to(modules / 'newhid')
            (state / 'modules-before').write_text(''.join(name+'\n' for name in old))
            (state / 'modules').write_text(''.join(name+'\n' for name in explicit))
            (state / 'usb-drivers-before').write_text('')
            (state / 'rfkill-socket-before').write_text('inactive\n')
            (state / 'name').write_text('liber-fixture\n')
            (state / 'plan').write_text('')
            (root / 'config/usb_gadget').mkdir(parents=True)
            (root / 'mountinfo').write_text(mountinfo.replace('{state}',str(state).replace(' ',r'\040')))
            if missing_old:
                import shutil
                shutil.rmtree(modules / old[0])
            binary = root / 'bin'
            binary.mkdir()
            command = binary / 'module-command'
            command.write_text('''#!/usr/bin/env python3
import json,os,pathlib,shutil,sys,time
root=pathlib.Path(os.environ['FIXTURE']); name=sys.argv[-1]
with (root/'commands').open('a') as f:f.write(json.dumps([pathlib.Path(sys.argv[0]).name]+sys.argv[1:])+'\\n')
(root/'child-pid').write_text(str(os.getpid()))
if os.environ.get('STUCK')=='1':time.sleep(30)
if os.environ.get('DENY')=='1' or os.environ.get('DENY_MODULE')==name:sys.exit(1)
modules=root/'sys/module'
def remove(name,cascade):
 deps=[module.name for module in modules.iterdir() if (module/'holders'/name).is_symlink()]
 shutil.rmtree(modules/name)
 for dependency in deps:
  (modules/dependency/'holders'/name).unlink()
  refs=len(list((modules/dependency/'holders').iterdir()))
  (modules/dependency/'refcnt').write_text(str(refs)+'\\n')
  if cascade and refs==0:remove(dependency,True)
remove(name,pathlib.Path(sys.argv[0]).name=='modprobe')
''')
            command.chmod(0o755)
            for name in ('rmmod', 'modprobe'):
                (binary / name).symlink_to(command)
            for name in ('lsmod',):
                fail = binary / name
                fail.write_text('#!/bin/sh\necho forbidden-proc-reader >&2\nexit 99\n')
                fail.chmod(0o755)
            source = SOURCE.read_text()
            source = source[:source.index('case "${1:-}" in\n')]
            source = source.replace('CONFIGFS=/sys/kernel/config', 'CONFIGFS='+str(root/'config'))
            source = source.replace('/sys/module',str(modules)).replace('/sys/bus',str(root/'sys/bus'))
            source = source.replace("Path('/proc/self/mountinfo')",'Path('+repr(str(root/'mountinfo'))+')')
            if mutate_cascade:
                anchor='timeout --signal=TERM --kill-after=1 5 rmmod "$1"'
                self.assertEqual(source.count(anchor),1)
                source=source.replace(anchor,'modprobe -r "$1"')
            # Do not spend host time on fixture refcount retries; the timeout test uses actual timeout.
            source += '\nsleep() { :; }\n'+entry+'\n'
            env={**os.environ,'LIBER_GADGET_STATE':str(state),'FIXTURE':str(root),
                 'PATH':str(binary)+os.pathsep+os.environ['PATH'],'STUCK':str(int(stuck)),
                 'DENY':str(int(deny)),'DENY_MODULE':deny_module}
            start=time.monotonic()
            result=subprocess.run(['bash','-c',source],env=env,capture_output=True,text=True,timeout=12)
            elapsed=time.monotonic()-start
            calls=[json.loads(line) for line in (root/'commands').read_text().splitlines()] if (root/'commands').exists() else []
            pending={name for name in names if (modules/name/'initstate').exists()}
            ledger=(state/'modules').read_text().splitlines() if (state/'modules').exists() else None
            child=int((root/'child-pid').read_text()) if (root/'child-pid').exists() else None
            if child is not None:
                self.assertFalse(Path(f'/proc/{child}').exists(),'the fixture module-command child must be reaped')
            return result,pending,ledger,calls,elapsed

    def exercise_setup(self, scenario, *, busy=False, kind='monitor', fail_write=None):
        """Exercise the real setup and its EXIT handler, with no caller-owned cleanup."""
        with tempfile.TemporaryDirectory(prefix='gadget-setup-') as temporary:
            root = Path(temporary)
            state = root / 'state'
            state.mkdir()
            modules = root / 'sys/module'
            modules.mkdir(parents=True)
            old = modules / 'oldpcm'
            (old / 'holders').mkdir(parents=True)
            (old / 'initstate').write_text('live\n')
            (old / 'refcnt').write_text('0\n')
            for bus in ('usb', 'hid'):
                (root / f'sys/bus/{bus}/drivers').mkdir(parents=True)
            udcs = root / 'sys/class/udc'
            udcs.mkdir(parents=True)
            if scenario in ('occupied', 'success'):
                udc = udcs / 'dummy_udc.0'
                udc.mkdir()
                (udc / 'function').write_text('foreign-owner\n' if scenario == 'occupied' else '')
            gadget_root = root / 'config/usb_gadget'
            gadget_root.mkdir(parents=True)
            if scenario == 'foreign-state':
                (state / 'name').write_text('liber-foreign\n')
                (state / 'modules').write_text('foreign ownership\n')
            if fail_write:
                (state / fail_write).mkdir()
            prior = {p.name: p.read_bytes() for p in state.iterdir() if p.is_file()}
            binary = root / 'bin'
            binary.mkdir()
            command = binary / 'module-command'
            command.write_text('''#!/usr/bin/env python3
import json, os, pathlib, shutil, sys
root = pathlib.Path(os.environ['FIXTURE'])
modules = root / 'sys/module'
state = root / 'state'
name = sys.argv[-1]
action = pathlib.Path(sys.argv[0]).name
before = (state / 'modules-before').read_text().splitlines() if (state / 'modules-before').is_file() else []
ledger = (state / 'modules').read_text().splitlines() if (state / 'modules').is_file() else []
owned = (state / 'name').is_file() and (state / 'plan').is_file() and (name in before or name in ledger)
with (root / 'commands').open('a') as stream:
    stream.write(json.dumps({'action': action, 'module': name, 'ownership_before_side_effect': owned}) + '\\n')
dependencies = {'udc_core': ['oldpcm'], 'dummy_hcd': ['udc_core'], 'usb_f_hid': ['udc_core'], 'gadgetfs': ['udc_core']}
def refs(module):
    count = len(list((module / 'holders').iterdir()))
    if module.name == 'udc_core' and os.environ['BUSY'] == '1':
        count += 1
    (module / 'refcnt').write_text(str(count) + '\\n')
def load(module_name):
    module = modules / module_name
    if module.exists():
        return
    (module / 'holders').mkdir(parents=True)
    (module / 'initstate').write_text('live\\n')
    refs(module)
    for dependency in dependencies.get(module_name, []):
        load(dependency)
        (modules / dependency / 'holders' / module_name).symlink_to(module)
        refs(modules / dependency)
if action == 'modprobe':
    if name == 'dummy_hcd' and os.environ['SCENARIO'] == 'dummy-fails':
        sys.exit(1)
    load(name)
else:
    deps = [p for p in modules.iterdir() if (p / 'holders' / name).is_symlink()]
    shutil.rmtree(modules / name)
    for dependency in deps:
        (dependency / 'holders' / name).unlink()
        refs(dependency)
''')
            command.chmod(0o755)
            for name in ('modprobe', 'rmmod'):
                (binary / name).symlink_to(command)
            source = SOURCE.read_text()
            source = source[:source.index('case "${1:-}" in\n')]
            source = source.replace('CONFIGFS=/sys/kernel/config', 'CONFIGFS=' + str(root / 'config'))
            source = source.replace('/sys/module', str(modules)).replace('/sys/bus', str(root / 'sys/bus'))
            source = source.replace('/sys/class/udc', str(udcs))
            # These model only kernel-created configfs parent directories and host queries. No
            # real module, UDC, mount, service manager or identity query reaches the host.
            source += '''
id() { printf '0\\n'; }
mountpoint() { return 0; }
systemctl() { return 1; }
sleep() { :; }
mkdir() {
    command mkdir "$@" || return
    local path
    for path in "$@"; do
        if [[ "$path" == "$GADGET_ROOT"/liber* ]]; then
            if [[ "$path" != "$GADGET_ROOT"/liber*/* ]]; then
                command mkdir "$path/strings" "$path/configs" "$path/functions"
            elif [[ "$path" == "$GADGET_ROOT"/liber*/configs/c.* && "$path" != */strings* ]]; then
                command mkdir "$path/strings"
            fi
        fi
    done
}
start_gadgetfs() { :; }
ALL_MODULES=(udc_core dummy_hcd usb_f_hid oldpcm)
'''
            if scenario == 'foreign-name':
                source += 'command mkdir "$GADGET_ROOT/liber$$"; printf foreign >"$GADGET_ROOT/liber$$/foreign"\n'
            if busy:
                # Distinguish the original setup error from teardown's ordinary status1.
                source += 'refuse() { say "$*"; exit 73; }\n'
            source += 'cmd_setup "' + kind + '"\n'
            source += 'trap -p EXIT >"$FIXTURE/trap-after-success"\nexit 29\n'
            env = {**os.environ, 'FIXTURE': str(root), 'LIBER_GADGET_STATE': str(state),
                   'PATH': str(binary) + os.pathsep + os.environ['PATH'],
                   'SCENARIO': scenario, 'BUSY': str(int(busy))}
            result = subprocess.run(['bash', '-c', source], env=env, capture_output=True, text=True, timeout=12)
            calls = [json.loads(line) for line in (root / 'commands').read_text().splitlines()] if (root / 'commands').exists() else []
            pending = {p.name for p in modules.iterdir() if (p / 'initstate').exists()}
            final_state = {p.name: p.read_bytes() for p in state.iterdir() if p.is_file()} if state.exists() else {}
            udc_state = {p.name: (p / 'function').read_bytes() for p in udcs.iterdir()}
            foreign = [p.read_bytes() for p in gadget_root.glob('*/foreign')]
            trap = (root / 'trap-after-success').read_text() if (root / 'trap-after-success').exists() else None
            return result, pending, calls, prior, final_state, udc_state, foreign, trap

    def test_failed_dummy_setup_cleans_modules_without_caller_teardown(self):
        result, pending, calls, _, state, _, _, _ = self.exercise_setup('dummy-fails')
        self.assertEqual(result.returncode, 1)
        self.assertIn('dummy_hcd did not load', result.stderr)
        self.assertEqual(pending, {'oldpcm'})
        self.assertEqual(state, {})
        self.assertTrue(all(c['ownership_before_side_effect'] for c in calls if c['action'] == 'modprobe'))
        self.assertEqual({c['module'] for c in calls if c['action'] == 'rmmod'}, {'udc_core', 'usb_f_hid'})

    def test_missing_udc_setup_cleans_loaded_controller(self):
        result, pending, calls, _, state, _, _, _ = self.exercise_setup('missing-udc')
        self.assertEqual(result.returncode, 1)
        self.assertIn('no UDC appeared', result.stderr)
        self.assertEqual(pending, {'oldpcm'})
        self.assertEqual(state, {})
        self.assertIn('dummy_hcd', {c['module'] for c in calls if c['action'] == 'rmmod'})

    def test_occupied_udc_setup_preserves_foreign_controller(self):
        result, pending, _, _, state, udcs, _, _ = self.exercise_setup('occupied')
        self.assertEqual(result.returncode, 1)
        self.assertIn("already driving 'foreign-owner'", result.stderr)
        self.assertEqual(pending, {'oldpcm'})
        self.assertEqual(state, {})
        self.assertEqual(udcs, {'dummy_udc.0': b'foreign-owner\n'})

    def test_failed_setup_preserves_original_error_and_busy_module_ledger(self):
        result, pending, calls, _, state, _, _, _ = self.exercise_setup('dummy-fails', busy=True)
        self.assertEqual(result.returncode, 73)
        self.assertEqual(pending, {'oldpcm', 'udc_core'})
        self.assertIn('udc_core', state['modules'].decode().splitlines())
        self.assertIn('name', state)
        self.assertIn('cleanup is incomplete', result.stderr)
        self.assertNotIn('oldpcm', {c['module'] for c in calls if c['action'] == 'rmmod'})

    def test_setup_collisions_never_claim_or_clean_foreign_state(self):
        for scenario in ('foreign-state', 'foreign-name'):
            with self.subTest(scenario=scenario):
                result, pending, calls, prior, state, _, foreign, _ = self.exercise_setup(scenario)
                self.assertEqual(result.returncode, 1)
                self.assertEqual(pending, {'oldpcm'})
                self.assertEqual(calls, [])
                self.assertEqual(state, prior)
                self.assertEqual(foreign, [b'foreign'] if scenario == 'foreign-name' else [])
                self.assertNotIn('torn down', result.stderr)

    def test_setup_record_write_failures_precede_all_module_effects(self):
        for filename in ('modules-before', 'usb-drivers-before', 'rfkill-socket-before', 'plan', 'modules', 'name'):
            with self.subTest(filename=filename):
                result, pending, calls, _, _, _, _, _ = self.exercise_setup('missing-udc', fail_write=filename)
                self.assertEqual(result.returncode, 1)
                self.assertEqual(pending, {'oldpcm'})
                self.assertEqual(calls, [])

    def test_both_success_paths_cancel_failure_cleanup(self):
        for kind in ('monitor', 'uvc'):
            with self.subTest(kind=kind):
                result, pending, calls, _, state, _, _, trap = self.exercise_setup('success', kind=kind)
                self.assertEqual(result.returncode, 29, result.stderr)
                self.assertIn('1d6b:0104', result.stdout)
                self.assertIn('dummy_hcd', pending)
                self.assertIn('name', state)
                self.assertEqual(trap, '')
                self.assertFalse(any(c['action'] == 'rmmod' for c in calls))
                self.assertNotIn('cleanup is incomplete', result.stderr)

    def test_explicit_dependency_closure_preserves_preexisting_modules(self):
        result,pending,ledger,calls,_=self.exercise()
        self.assertEqual(result.returncode,0,result.stderr)
        self.assertEqual(pending,{'oldpcm'})
        self.assertEqual({call[-1] for call in calls},{'function','dependency'})
        self.assertTrue(all(call[0]=='rmmod' for call in calls))
        self.assertIn('dependency',ledger)

    def test_retry_retains_real_orphan_dependency_ownership(self):
        # The first call removes the function, then the dependency refuses removal. Without the
        # persisted closure, the second call cannot rediscover the now-orphaned dependency.
        entry = """
if unload_owned_modules; then exit 90; fi
[[ ! -e "$FIXTURE/sys/module/function/initstate" ]] || exit 91
[[ -e "$FIXTURE/sys/module/dependency/initstate" ]] || exit 92
unset DENY_MODULE
unload_owned_modules
"""
        result,pending,ledger,calls,_=self.exercise(deny_module='dependency',entry=entry)
        self.assertEqual(result.returncode,0,result.stderr)
        self.assertEqual(pending,{'oldpcm'})
        self.assertIn('dependency',ledger)
        self.assertEqual(sum(call[-1]=='function' for call in calls),1)
        self.assertGreater(sum(call[-1]=='dependency' for call in calls),1)

    def test_successful_teardown_removes_ledger_and_is_idempotent(self):
        result,pending,ledger,calls,_=self.exercise(entry='cmd_teardown; cmd_teardown')
        self.assertEqual(result.returncode,0,result.stderr)
        self.assertEqual(pending,{'oldpcm'})
        self.assertIsNone(ledger)
        self.assertEqual(len(calls),2)
        self.assertIn("nothing of this harness's is set up",result.stderr)

    def test_failed_ownership_recording_prevents_any_removal(self):
        entry = """
printf() {
    if [[ "$(readlink "/proc/$$/fd/1")" == "$FIXTURE/state with spaces/modules" ]]; then
        return 1
    fi
    builtin printf "$@"
}
unload_owned_modules
"""
        result,pending,ledger,calls,_=self.exercise(entry=entry)
        self.assertNotEqual(result.returncode,0)
        self.assertEqual(pending,{'function','dependency','oldpcm'})
        self.assertEqual(ledger,['function'])
        self.assertFalse(calls)
        self.assertIn('ownership ledger could not be extended',result.stderr)

    def test_new_host_driver_joins_owned_set(self):
        result,pending,_,calls,_=self.exercise(new_driver=True)
        self.assertEqual(result.returncode,0,result.stderr)
        self.assertEqual(pending,{'oldpcm'})
        self.assertIn('newhid',{call[-1] for call in calls})

    def test_cascade_mutation_reproduces_loss_and_fails_preservation(self):
        result,pending,_,calls,_=self.exercise(mutate_cascade=True)
        self.assertNotEqual(result.returncode,0)
        self.assertNotIn('oldpcm',pending)
        self.assertIn('preexisting module oldpcm is missing',result.stderr)
        self.assertTrue(any(call[0]=='modprobe' for call in calls))

    def test_busy_owned_module_is_left_and_failure_keeps_ledger(self):
        result,pending,ledger,calls,_=self.exercise(busy=('function',),entry='cmd_teardown')
        self.assertNotEqual(result.returncode,0)
        self.assertEqual(pending,{'function','dependency','oldpcm'})
        self.assertIn('dependency',ledger)
        self.assertFalse(calls)
        self.assertIn('is in use - it was left loaded',result.stderr)

    def test_ledger_cannot_authorize_removing_a_preexisting_module(self):
        result,pending,_,calls,_=self.exercise(explicit=('function','oldpcm'))
        self.assertNotEqual(result.returncode,0)
        self.assertIn('oldpcm',pending)
        self.assertFalse(calls)

    def test_missing_preexisting_module_is_a_failure(self):
        result,_,_,_,_=self.exercise(missing_old=True)
        self.assertNotEqual(result.returncode,0)
        self.assertIn('preexisting module oldpcm is missing',result.stderr)

    def test_real_timeout_stops_before_another_removal_and_keeps_ledger(self):
        result,pending,ledger,calls,elapsed=self.exercise(stuck=True,entry='cmd_teardown')
        self.assertNotEqual(result.returncode,0)
        self.assertEqual(len(calls),1)
        self.assertIn('timed out; no further module removal',result.stderr)
        self.assertEqual(pending,{'function','dependency','oldpcm'})
        self.assertIn('dependency',ledger)
        self.assertGreaterEqual(elapsed,5)
        self.assertLess(elapsed,9)

    def test_loaded_list_excludes_builtins_and_regular_sysfs_attributes(self):
        result,_,_,calls,_=self.exercise(entry='loaded_modules')
        self.assertEqual(result.returncode,0,result.stderr)
        self.assertEqual(set(result.stdout.split()),{'function','dependency','oldpcm'})
        self.assertFalse(calls)

    def test_mount_verify_decodes_escaped_state_and_refuses_owned_mount(self):
        row='21 1 0:1 / {state}/ffs-reader rw - functionfs reader rw\n'
        result,_,_,_,_=self.exercise(entry='rm -f "$(state_file name)"; cmd_verify',mountinfo=row)
        self.assertNotEqual(result.returncode,0)
        self.assertIn('mount of this harness',result.stderr)

    def test_mount_verify_ignores_sibling_and_refuses_broken_inventory(self):
        row='21 1 0:1 / {state}-other/ffs-reader rw - functionfs reader rw\n'
        result,_,_,_,_=self.exercise(entry='rm -f "$(state_file name)"; cmd_verify',mountinfo=row)
        self.assertEqual(result.returncode,0,result.stderr)
        result,_,_,_,_=self.exercise(entry='rm -f "$(state_file name)"; cmd_verify',mountinfo='broken\n')
        self.assertNotEqual(result.returncode,0)
        self.assertIn('inventory could not be read',result.stderr)


if __name__=='__main__':
    unittest.main(verbosity=2)
