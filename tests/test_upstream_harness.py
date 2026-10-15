"""Coverage, no-fallback and Linux daemon ownership contracts of the adapter."""
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import upstream_cgi
import upstream_harness as harness
from upstream_process import process
import upstream_process


TAP = """ok 1 - enable client-side http/2 # SKIP HTTP2
ok 2 - setup repository
ok 3 - create http-accessible bare repository
ok 4 - setup askpass helper
ok 5 - clone http repository
ok 6 - fetch changes via http
ok 7 - used upload-pack service
ok 8 - follow redirects (301) # SKIP --run
1..8
"""


class CoverageTests(unittest.TestCase):
    def test_missing_live_proc_children_is_an_error(self):
        with patch.object(Path, "read_text", side_effect=FileNotFoundError), \
             patch.object(upstream_process, "process", return_value={"state": "S"}):
            with self.assertRaisesRegex(RuntimeError, "children interface"):
                upstream_process.descendants()

    def test_only_exact_executed_selected_cases_count(self):
        self.assertEqual(len(harness.selected_cases(TAP)), 6)
        for before, after in (("ok 5 -", "not ok 5 -"),
                              ("clone http repository", "clone http repository # SKIP dependency"),
                              ("ok 5 - clone http repository\n", ""),
                              ("ok 5 - clone http repository", "ok 5 - changed case")):
            with self.subTest(after=after), self.assertRaises(ValueError):
                harness.selected_cases(TAP.replace(before, after))
        with self.assertRaises(ValueError):
            harness.selected_cases(TAP + "ok 5 # skip clone http repository (--run)\n")

    def test_trace_requires_native_discovery_post_protocol_and_two_heads(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "trace"
            rows = [{"backend": "git-zero", "native_sha256": "native", "method": method,
                     "path": endpoint, "protocol": "version=2", "head": head}
                    for method, endpoint, head in (("GET", "info/refs", "a"),
                                                   ("POST", "git-upload-pack", "a"),
                                                   ("POST", "git-upload-pack", "b"))]
            def write(value):
                path.write_text("".join(json.dumps(row) + "\n" for row in value))
            write(rows)
            self.assertEqual(len(harness.trace_proof(path, "native", 2)), 3)
            for field, value in (("backend", "git-reference"), ("native_sha256", "other"),
                                 ("protocol", ""), ("head", "a")):
                broken = [dict(row, **{field: value}) for row in rows]
                write(broken)
                with self.subTest(field=field), self.assertRaises(ValueError):
                    harness.trace_proof(path, "native", 2)
            for broken in ([], rows[:1], rows[1:]):
                write(broken)
                with self.assertRaises(ValueError):
                    harness.trace_proof(path, "native", 2)


class CgiTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "repo.git").mkdir()
        (self.root / "snapshots").mkdir()
        self.config = {"document_root": str(self.root), "git": "/usr/bin/git",
                       "native": "/native-upstream-cgi", "native_sha256": "native",
                       "trace": str(self.root / "trace"), "snapshots": str(self.root / "snapshots")}
        self.env = {"DOCUMENT_ROOT": str(self.root), "PATH_INFO": "/repo.git/git-upload-pack",
                    "REQUEST_METHOD": "POST", "CONTENT_LENGTH": "4"}

    def test_rejects_path_escape_wrong_document_and_body_limits(self):
        for field, value in (("PATH_INFO", "/repo.git/../other/info/refs"),
                             ("DOCUMENT_ROOT", str(self.root.parent)),
                             ("CONTENT_LENGTH", "65537"), ("CONTENT_LENGTH", "-1"),
                             ("REQUEST_METHOD", "PUT")):
            with self.subTest(field=field), self.assertRaises(ValueError):
                upstream_cgi.request(self.config, dict(self.env, **{field: value}), io.BytesIO(b"0000"))
        with self.assertRaises(ValueError):
            upstream_cgi.request(self.config, self.env, io.BytesIO(b"0"))
        (self.root / "repo.git").rmdir()
        (self.root / "repo.git").symlink_to(self.root.parent)
        with self.assertRaises(ValueError):
            upstream_cgi.request(self.config, self.env, io.BytesIO(b"0000"))

    def test_failed_native_never_falls_back_or_writes_success_trace(self):
        failure = subprocess.CalledProcessError(1, [self.config["native"]])
        with patch.object(upstream_cgi, "export_bounded", return_value={}), \
             patch.object(upstream_cgi.subprocess, "run", side_effect=failure) as native:
            with self.assertRaises(subprocess.CalledProcessError):
                upstream_cgi.serve(self.config, self.env, io.BytesIO(b"0000"))
        self.assertEqual(native.call_count, 1)
        self.assertEqual(native.call_args.args[0][0], "/native-upstream-cgi")
        self.assertFalse(Path(self.config["trace"]).exists())


@unittest.skipUnless(sys.platform == "linux", "Linux subreaper and /proc ownership")
class LifecycleTests(unittest.TestCase):
    def run_owned(self, timeout):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source = root / "source"
            (source / "t").mkdir(parents=True)
            output = root / "output"
            output.mkdir()
            pid_file = root / "owned.json"
            # A detached daemon and CGI descendant outlive the original shell.
            # The CGI ignores TERM so teardown must exercise its bounded KILL.
            program = f'''import json, os, signal, time
pid = os.fork()
if pid == 0:
    os.setsid()
    ready, notify = os.pipe()
    child = os.fork()
    if child == 0:
        os.close(ready)
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        os.write(notify, b"1")
        os.close(notify)
        time.sleep(60)
        os._exit(0)
    os.close(notify)
    assert os.read(ready, 1) == b"1"
    os.close(ready)
    with open({str(pid_file)!r}, "w") as f:
        json.dump([os.getpid(), child], f)
    time.sleep(60)
    os._exit(0)
while not os.path.exists({str(pid_file)!r}): time.sleep(0.01)
print({TAP!r}, flush=True)
time.sleep({60 if timeout else 0})
'''
            child_source = root / "daemon.py"
            child_source.write_text(program)
            (source / f"t/{harness.SCRIPT}.sh").write_text(
                f"#!/bin/sh\nexec {sys.executable} {child_source}\n")
            if timeout:
                with self.assertRaises(subprocess.TimeoutExpired):
                    harness.run_shell(source, output, os.environ.copy(), 2)
            else:
                self.assertEqual(len(harness.run_shell(source, output, os.environ.copy(), 5)), 6)
            for pid in json.loads(pid_file.read_text()):
                self.assertIsNone(process(pid), f"owned CGI/daemon {pid} leaked")

    def test_success_reaps_detached_daemon_and_term_resistant_cgi(self):
        self.run_owned(False)

    def test_timeout_reaps_detached_daemon_and_term_resistant_cgi(self):
        self.run_owned(True)


if __name__ == "__main__":
    unittest.main()
