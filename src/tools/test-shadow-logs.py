#!/usr/bin/env python3
"""Exercise the shadow shell route with host fixtures in place of builds and guests."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]

# The comparison fixture opens exactly the arguments the real comparison opens. Planner decisions
# and execution are fixtures here; model evidence and guest behavior have their own suites.
PLANNER = r'''#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys

args = sys.argv[sys.argv.index("--") + 1:]
if args[0] == "--candidate":
    assert Path(args[1]).read_text() == "frozen candidate fixture\n"
    args = args[2:]
command = args[0]
if command in ("source-digest", "model-hash"):
    print("stable-fixture")
elif command in ("booted", "built"):
    print("x86_64")
elif command == "guest-selection":
    print("kernel.fixture")
elif command in ("host-checks", "dev-checks", "build-checks"):
    name, arch, environment = {
        "host-checks": ("gate.fixture", "host", "host"),
        "dev-checks": ("dev.fixture", "x86_64", "dev-guest"),
        "build-checks": ("build.kernel", "x86_64", "host-build"),
    }[command]
    print(f"{name}\t{arch}\t{environment}\tdefault\ttrue")
elif command == "build-steps":
    print("kernel\t./build.sh --arch x86_64 --part kernel\tbuild.kernel x86_64 host-build default")
elif command == "shadow":
    assert Path.cwd().name == "src", "exercise the production working-directory transition"
    logs = {}
    for index, arg in enumerate(args):
        if arg.endswith("-log"):
            path = Path(args[index + 1])
            logs[arg] = {"path": str(path), "text": path.read_text()}
    assert logs, "a comparison must consume evidence"
    with Path(os.environ["SHADOW_OBSERVED"]).open("a") as output:
        output.write(json.dumps(logs) + "\n")
else:
    raise AssertionError(f"unexpected planner command: {args}")
'''

GUEST = r'''#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")" && pwd)"
kind=full
[[ -n "${TEST_SELECTION:-}" ]] && kind=scoped
run="$root/.build/logs/$kind-run.log"
guest="$root/.build/logs/$kind-guest.log"
printf 'firmware only\n' >"$run"
printf 'firmware only\n' >"$guest"
result="$guest"
[[ "${SHADOW_SUITE_IN_RUN:-0}" == 1 ]] && result="$run"
printf 'running 1 selected tests\n%s evidence\ntest suite complete\n' "$kind" >"$result"
printf '[test-x86_64] RESULT-LOGS %s %s\n' "$run" "$guest"
'''


class ShadowLogPaths(unittest.TestCase):
    def exercise(self, execute, suite_in_run, source=None):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "src/tools").mkdir(parents=True)
            (root / ".build/logs").mkdir(parents=True)
            (root / "bin").mkdir()
            shutil.copy2(ROOT / "lib.sh", root / "lib.sh")
            shutil.copy2(ROOT / "src/tools/result-logs.sh", root / "src/tools/result-logs.sh")
            scripts = {
                "verify.sh": source if source is not None else (ROOT / "verify.sh").read_text(),
                "test.sh": GUEST,
                "build.sh": "#!/usr/bin/env bash\necho 'build.sh: built: kernel for x86_64'\n",
                "bin/cargo": PLANNER,
            }
            for name, text in scripts.items():
                path = root / name
                path.write_text(text)
                path.chmod(0o755)
            observed = root / "observed.jsonl"
            candidate = root / "candidate.toml"
            candidate.write_text("frozen candidate fixture\n")
            env = dict(os.environ, PATH=f"{root / 'bin'}:{os.environ['PATH']}",
                       SHADOW_OBSERVED=str(observed), SHADOW_SUITE_IN_RUN=str(int(suite_in_run)))
            result = subprocess.run(["./verify.sh", "--for", "src/kernel/elf.rs",
                                     "--candidate", str(candidate) if execute else candidate.name,
                                     "--shadow-exec" if execute else "--shadow"],
                                    cwd=root, env=env, capture_output=True, text=True, timeout=10)
            logs = [json.loads(line) for line in observed.read_text().splitlines()] if observed.exists() else []
            return result, logs

    def assert_logs(self, execute, suite_in_run):
        result, logs = self.exercise(execute, suite_in_run)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(len(logs), 4, "guest, host, development and build comparisons must all run")
        evidence = {name: value for comparison in logs for name, value in comparison.items()}
        expected = {"--guest-log", "--host-log", "--dev-log", "--build-log"}
        if execute:
            expected.update(("--scoped-log", "--host-scoped-log", "--dev-scoped-log"))
        self.assertEqual(set(evidence), expected)
        for name, entry in evidence.items():
            self.assertTrue(Path(entry["path"]).is_absolute(), name)
            if name in ("--guest-log", "--scoped-log"):
                self.assertTrue(entry["path"].endswith("-run.log" if suite_in_run else "-guest.log"))
                self.assertIn("scoped evidence" if name == "--scoped-log" else "full evidence", entry["text"])
            else:
                self.assertIn("\tPASS\ntotal 1\n", entry["text"], name)

    def test_dry_shadow_reads_every_absolute_evidence_path(self):
        for suite_in_run in (False, True):
            with self.subTest(suite_in_run=suite_in_run):
                self.assert_logs(False, suite_in_run)

    def test_shadow_exec_reads_scoped_and_full_evidence_paths(self):
        for suite_in_run in (False, True):
            with self.subTest(suite_in_run=suite_in_run):
                self.assert_logs(True, suite_in_run)


if __name__ == "__main__":
    unittest.main(verbosity=2)
