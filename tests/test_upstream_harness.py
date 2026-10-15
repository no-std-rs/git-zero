"""Coverage, no-fallback and Linux daemon ownership contracts of the adapter."""
import io
import json
import os
from pathlib import Path
import select
import shlex
import signal
import subprocess
import sys
import tempfile
import threading
from types import SimpleNamespace
import unittest
from unittest.mock import MagicMock, patch

import upstream_cgi
import upstream_harness as harness
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
    def test_failed_trial_still_checks_source_and_retains_both_errors(self):
        with tempfile.TemporaryDirectory() as temp:
            args = SimpleNamespace(output=Path(temp), source=Path("source"), git=Path("git"), timeout=1)
            with patch.object(harness, "arguments", return_value=args), \
                 patch.object(harness.sys, "platform", "linux"), \
                 patch.object(harness.os, "geteuid", return_value=1000), \
                 patch.object(harness, "verify_source", side_effect=[None, ValueError("source changed")]) as verify, \
                 patch.object(harness, "matrix", side_effect=ValueError("native failed")), \
                 patch("builtins.print"):
                self.assertEqual(harness.main(), 1)
            report = json.loads((args.output / "report.json").read_text())
            self.assertEqual(report["status"], "failed")
            self.assertEqual(report["errors"], ["native failed", "source changed"])
            self.assertEqual(verify.call_count, 2)

    def test_missing_live_proc_children_is_an_error(self):
        with patch.object(Path, "read_text", side_effect=FileNotFoundError):
            with self.assertRaisesRegex(RuntimeError, "children interface"):
                upstream_process.children()

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

    def test_unattested_or_oversized_native_response_never_publishes_success(self):
        header = b"Status: 200 OK\r\n"
        proof = b"git-zero-upstream-cgi: served POST git-upload-pack\n"
        oversized = header + b"x" * (upstream_cgi.BYTE_LIMIT + 4097 - len(header))
        cases = ((header, b"git-reference served request\n", "attest successful"),
                 (b"Status: 500 Internal Server Error\r\n", proof, "attest successful"),
                 (oversized, proof, "response exceeds limit"))
        for stdout, stderr, error in cases:
            result = SimpleNamespace(stdout=stdout, stderr=stderr)
            with self.subTest(error=error, response_bytes=len(stdout)), \
                 patch.object(upstream_cgi, "export_bounded", return_value={}), \
                 patch.object(upstream_cgi.subprocess, "run", return_value=result) as native:
                with self.assertRaisesRegex(ValueError, error):
                    upstream_cgi.serve(self.config, self.env, io.BytesIO(b"0000"))
            native.assert_called_once()
            self.assertEqual(native.call_args.args[0][0], self.config["native"])
            self.assertFalse(Path(self.config["trace"]).exists())


class PortHandoffTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.args = SimpleNamespace(output=self.root)

    def bind_failure(self, root, port):
        root.mkdir()
        (root / "tap.log").write_text(
            f"(98)Address already in use: AH00072: make_sock: could not bind to address 127.0.0.1:{port}\n")
        raise harness.UpstreamFailure("Apache startup failed")

    def test_kernel_reused_candidates_stay_reserved_until_new_port(self):
        sockets = [MagicMock() for _ in range(3)]
        for port, reservation in zip((41001, 41002, 41003), sockets):
            reservation.__enter__.return_value = reservation
            reservation.getsockname.return_value = ("127.0.0.1", port)
        def bind_new(address):
            for reservation in sockets[:2]:
                reservation.__exit__.assert_not_called()
        sockets[2].bind.side_effect = bind_new
        with patch.object(harness.socket, "socket", side_effect=sockets):
            self.assertEqual(harness.select_port({41001, 41002}), 41003)
        for reservation in sockets:
            reservation.__exit__.assert_called_once()

    def test_recovers_only_startup_collision_in_new_root_and_port(self):
        calls = []
        def attempt(args, root, backend, protocol, object_format, port):
            calls.append((root, port))
            if len(calls) == 1:
                self.bind_failure(root, port)
            root.mkdir()
            return {"artifacts": str(root)}
        failures = []
        with patch.object(harness, "select_port", side_effect=[41001, 41002]), \
             patch.object(harness, "run_trial", side_effect=attempt):
            result = harness.trial(self.args, "git-reference", 0, "sha1", failures)
        self.assertEqual([port for _, port in calls], [41001, 41002])
        self.assertNotEqual(calls[0][0], calls[1][0])
        self.assertEqual(result["artifacts"], str(calls[1][0]))
        self.assertEqual(failures[0]["artifacts"], str(calls[0][0]))
        self.assertTrue((calls[0][0] / "tap.log").is_file())

    def test_collision_retry_has_three_attempt_bound(self):
        failures = []
        def attempt(args, root, backend, protocol, object_format, port):
            self.bind_failure(root, port)
        with patch.object(harness, "select_port", side_effect=[41001, 41002, 41003]), \
             patch.object(harness, "run_trial", side_effect=attempt) as trial:
            with self.assertRaises(harness.UpstreamFailure):
                harness.trial(self.args, "git-reference", 0, "sha1", failures)
        self.assertEqual(trial.call_count, 3)
        self.assertEqual(len(failures), 3)

    def test_selected_case_or_native_trace_disallows_retry(self):
        root = self.root / "attempt"
        with self.assertRaises(harness.UpstreamFailure):
            self.bind_failure(root, 41001)
        self.assertTrue(harness.apache_bind_collision(root, 41001))
        self.assertFalse(harness.apache_bind_collision(root, 4100))
        original = (root / "tap.log").read_text()
        (root / "tap.log").write_text(original + "not ok 5 - clone http repository\n")
        self.assertFalse(harness.apache_bind_collision(root, 41001))
        (root / "tap.log").write_text(original)
        (root / "cgi.jsonl").write_text('{"backend":"git-zero"}\n')
        self.assertFalse(harness.apache_bind_collision(root, 41001))

    def test_semantic_and_timeout_failures_do_not_retry(self):
        for error in (ValueError("selected case missing"), subprocess.TimeoutExpired("shell", 1)):
            with self.subTest(error=type(error).__name__), \
                 patch.object(harness, "select_port", return_value=41001), \
                 patch.object(harness, "run_trial", side_effect=error) as trial:
                with self.assertRaises(type(error)):
                    harness.trial(self.args, "git-zero", 0, "sha1", [])
            self.assertEqual(trial.call_count, 1)


@unittest.skipUnless(sys.platform == "linux", "Linux subreaper and /proc ownership")
class LifecycleTests(unittest.TestCase):
    def test_threaded_host_is_rejected_before_launch(self):
        release = threading.Event()
        worker = threading.Thread(target=release.wait)
        worker.start()
        try:
            with self.assertRaisesRegex(RuntimeError, "single-threaded"):
                upstream_process.require_ownership()
        finally:
            release.set()
            worker.join()

    def test_pidfd_kernel_ownership_rejects_nonchild_before_signalling(self):
        with patch.object(upstream_process.signal, "pidfd_send_signal") as send:
            upstream_process.signal_child(os.getpid(), signal.SIGCONT)
        send.assert_not_called()

    def test_pidfd_signals_live_owned_child(self):
        child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
        self.addCleanup(child.wait, timeout=2)
        self.addCleanup(child.kill)
        upstream_process.signal_child(child.pid, signal.SIGTERM)
        self.assertEqual(child.wait(timeout=2), -signal.SIGTERM)

    def test_pidfd_zombie_validation_does_not_reap_child(self):
        child = subprocess.Popen([sys.executable, "-c", "pass"])
        self.addCleanup(child.wait, timeout=2)
        self.addCleanup(child.kill)
        with upstream_process.pid_handle(child.pid) as fd:
            self.assertTrue(select.select([fd], [], [], 2)[0])
            flags = os.WEXITED | os.WNOHANG | os.WNOWAIT
            self.assertEqual(os.waitid(os.P_PIDFD, fd, flags).si_pid, child.pid)
            self.assertEqual(os.waitid(os.P_PIDFD, fd, flags).si_pid, child.pid)
        self.assertEqual(child.wait(timeout=2), 0)

    def run_owned(self, timeout):
        adoption = upstream_process.adoption_state()
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source = root / "source"
            (source / "t").mkdir(parents=True)
            output = root / "output"
            output.mkdir()
            pid_file = root / "owned.json"
            tap_file = root / "tap-input"
            tap_file.write_text(TAP)
            child_source = Path(__file__).parent / "fixtures/upstream_daemon.py"
            arguments = [sys.executable, str(child_source), "shell", str(pid_file), str(tap_file), str(int(timeout))]
            (source / f"t/{harness.SCRIPT}.sh").write_text(
                "#!/bin/sh\nexec " + shlex.join(arguments) + "\n")
            if timeout:
                with self.assertRaises(subprocess.TimeoutExpired):
                    harness.run_shell(source, output, os.environ.copy(), 2)
            else:
                self.assertEqual(len(harness.run_shell(source, output, os.environ.copy(), 5)), 6)
            self.assertEqual(upstream_process.adoption_state(), adoption)
            for pid in json.loads(pid_file.read_text()):
                self.assertFalse(Path(f"/proc/{pid}").exists(), f"owned CGI/daemon {pid} leaked")

    def test_success_reaps_detached_daemon_and_term_resistant_cgi(self):
        self.run_owned(False)

    def test_timeout_reaps_detached_daemon_and_term_resistant_cgi(self):
        self.run_owned(True)


if __name__ == "__main__":
    unittest.main()
