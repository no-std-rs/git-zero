import contextlib
import os
from pathlib import Path
import selectors
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

from benchmark import measure
from benchmark_harness import Server
from benchmark_memory import LIMITS, ProcessTreeRSS, SamplingError, TreeReader
from interop import environment


requires_children = unittest.skipUnless(
    Path(f"/proc/{os.getpid()}/task/{os.getpid()}/children").exists(),
    "live sampling requires the Linux proc children interface")


def line(process):
    with selectors.DefaultSelector() as selector:
        selector.register(process.stdout, selectors.EVENT_READ)
        if not selector.select(timeout=5):
            raise AssertionError("fixture process did not reach its barrier")
    value = process.stdout.readline()
    if not value:
        raise AssertionError("fixture process exited before its barrier")
    return value.strip()


def send(process, value):
    process.stdin.write(value + "\n")
    process.stdin.flush()


@contextlib.contextmanager
def program(source):
    process = subprocess.Popen([sys.executable, "-c", source], stdin=subprocess.PIPE,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                               start_new_session=True)
    try:
        yield process
    finally:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait(timeout=5)
        for stream in (process.stdin, process.stdout, process.stderr):
            stream.close()


def alive(pid):
    try:
        state = Path(f"/proc/{pid}/stat").read_bytes().rpartition(b")")[2].split()[0]
        return state not in (b"Z", b"X")
    except (FileNotFoundError, ProcessLookupError):
        return False


def await_dead(pid):
    deadline = time.monotonic() + 5
    while alive(pid) and time.monotonic() < deadline:
        time.sleep(0.01)
    if alive(pid):
        raise AssertionError("fixture descendant survived process-group cleanup")


class ProcessLivenessTests(unittest.TestCase):
    def test_liveness_probe_handles_exit_during_stat_read(self):
        for error in (FileNotFoundError(), ProcessLookupError()):
            with self.subTest(error=type(error).__name__), \
                    mock.patch.object(Path, "read_bytes", side_effect=error):
                self.assertFalse(alive(123))
        with mock.patch.object(Path, "read_bytes", side_effect=PermissionError()):
            with self.assertRaises(PermissionError):
                alive(123)


def proc_record(root, pid, parent, pages, children=(), start=1, task=None):
    directory = root / str(pid)
    directory.mkdir(exist_ok=True)
    fields = ["S", str(parent)] + ["0"] * 17 + [str(start), "0", str(pages)]
    stat = f"{pid} (name with ) parentheses) " + " ".join(fields)
    (directory / "stat").write_text(stat)
    thread = directory / "task" / str(task or pid)
    thread.mkdir(parents=True, exist_ok=True)
    (thread / "stat").write_text(stat)
    (thread / "children").write_text(" ".join(map(str, children)))


@unittest.skipUnless(sys.platform == "linux", "Linux /proc resource observer")
class FakeProcessTreeMemoryTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)

    def test_aggregate_is_one_sweep_and_does_not_add_independent_peaks(self):
        root = self.root
        proc_record(root, 10, 1, 100, (20,), start=10)
        proc_record(root, 20, 10, 50, start=20)
        reader = TreeReader(10, proc=root)
        sampler = ProcessTreeRSS(10, 1, reader=reader)
        with sampler:
            proc_record(root, 10, 1, 40, (20,), start=10)
            proc_record(root, 20, 10, 200, start=20)
        report = sampler.report()
        self.assertEqual(report["peak_rss_kib"], 240 * reader.page_kib)
        self.assertNotEqual(report["peak_rss_kib"], 300 * reader.page_kib)
        self.assertEqual(report["processes_at_peak"], 2)
        self.assertFalse(sampler.thread.is_alive())

    def test_short_lived_descendants_between_sweeps_are_explicitly_partial(self):
        root = self.root
        proc_record(root, 10, 1, 100)
        with ProcessTreeRSS(10, 1, expect_children=True, reader=TreeReader(10, proc=root)) as sampler:
            # A CGI child can finish entirely between the initial/final
            # sweeps. Its historical peak must not fill this missing sum.
            proc_record(root, 10, 1, 100, (20,))
            proc_record(root, 20, 10, 100)
            (root / "20/stat").unlink()
            proc_record(root, 10, 1, 100)
        report = sampler.report()
        self.assertEqual(report["status"], "partial")
        self.assertIsNone(report["peak_rss_kib"])
        self.assertGreater(report["observed_peak_rss_kib"], 0)
        self.assertEqual(report["samples_with_descendants"], 0)

    def test_process_task_byte_and_wall_limits_fail_without_a_partial_success(self):
        root = self.root
        proc_record(root, 10, 1, 100, (20,))
        proc_record(root, 20, 10, 100)
        for limits in ({**LIMITS, "processes": 1}, {**LIMITS, "tasks": 0},
                       {**LIMITS, "read_bytes": 1}, {**LIMITS, "sample_wall_s": -1}):
            sampler = ProcessTreeRSS(10, 1, reader=TreeReader(10, proc=root, limits=limits))
            with self.subTest(limits=limits), self.assertRaisesRegex(SamplingError, "budget"), sampler:
                pass
            with self.assertRaisesRegex(SamplingError, "unavailable"):
                sampler.report()

    def test_pid_reuse_reparenting_and_vanished_children_do_not_mix_foreign_rss(self):
        root = self.root
        proc_record(root, 10, 1, 100, (20, 30))
        proc_record(root, 20, 99, 1000000)
        reader = TreeReader(10, proc=root)
        self.assertEqual(reader.sample(), (100 * reader.page_kib, 1))
        proc_record(root, 10, 1, 100, start=2)
        with self.assertRaisesRegex(SamplingError, "PID changed"):
            reader.sample()
        final = ProcessTreeRSS(10, 1, reader=TreeReader(10, proc=root))
        with self.assertRaisesRegex(SamplingError, "PID changed"), final:
            proc_record(root, 10, 1, 100, start=3)
        with self.assertRaisesRegex(SamplingError, "unavailable"):
            final.report()

    def test_missing_children_interface_fails_but_vanished_threads_are_ignored(self):
        root = self.root
        proc_record(root, 10, 1, 100)
        (root / "10/task/10/children").unlink()
        sampler = ProcessTreeRSS(10, 1, reader=TreeReader(10, proc=root))
        with self.assertRaisesRegex(SamplingError, "children interface unavailable"), sampler:
            self.fail("missing kernel interface was accepted as an empty tree")
        with self.assertRaisesRegex(SamplingError, "unavailable"):
            sampler.report()
        proc_record(root, 10, 1, 100)
        (root / "10/task/99").mkdir()  # A task disappeared after scandir.
        self.assertEqual(TreeReader(10, proc=root).sample(), (100 * sampler.reader.page_kib, 1))


@unittest.skipUnless(sys.platform == "linux", "Linux /proc resource observer")
class LiveProcessTreeMemoryTests(unittest.TestCase):
    @requires_children
    def test_sampling_failure_and_cancellation_stop_the_observer(self):
        with program("import time; print('ready', flush=True); time.sleep(60)") as process:
            self.assertEqual(line(process), "ready")
            sampler = ProcessTreeRSS(process.pid, 0.01)
            with self.assertRaisesRegex(SamplingError, "observation failed"), sampler:
                with mock.patch.object(sampler.reader, "sample", side_effect=PermissionError("denied")):
                    self.assertTrue(sampler.stop.wait(timeout=2))
            self.assertFalse(sampler.thread.is_alive())
            with self.assertRaisesRegex(SamplingError, "unavailable"):
                sampler.report()
            cancelled = ProcessTreeRSS(process.pid, 0.01)
            with self.assertRaisesRegex(RuntimeError, "cancelled"), cancelled:
                raise RuntimeError("cancelled")
            self.assertFalse(cancelled.thread.is_alive())
            with self.assertRaisesRegex(SamplingError, "unavailable"):
                cancelled.report()

    @requires_children
    def test_server_exit_invalidates_the_observation(self):
        with program("import time; print('ready', flush=True); time.sleep(60)") as process:
            self.assertEqual(line(process), "ready")
            sampler = ProcessTreeRSS(process.pid, 0.01)
            with self.assertRaisesRegex(SamplingError, "server exited"), sampler:
                process.kill()
                process.wait(timeout=5)
                self.assertTrue(sampler.stop.wait(timeout=2))
            self.assertFalse(sampler.thread.is_alive())

    @requires_children
    def test_live_thread_forked_child_and_grandchild_are_observed_together(self):
        leaf = "buffer=bytearray(16*1024*1024); buffer[::4096]=b'x'*4096; print('ready',flush=True); input()"
        child = ("import subprocess,sys\n"
                 f"grand=subprocess.Popen([sys.executable,'-c',{leaf!r}],stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True)\n"
                 "buffer=bytearray(16*1024*1024); buffer[::4096]=b'x'*4096\n"
                 "assert grand.stdout.readline().strip()=='ready'\n"
                 "print('ready',flush=True); input()\n"
                 "grand.stdin.write('exit\\n'); grand.stdin.flush(); grand.wait(timeout=5)\n")
        host = ("import subprocess,sys,threading\n"
                "def worker():\n"
                f" child=subprocess.Popen([sys.executable,'-c',{child!r}],stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True)\n"
                " assert child.stdout.readline().strip()=='ready'\n"
                " print('ready',flush=True); input()\n"
                " child.stdin.write('exit\\n'); child.stdin.flush(); child.wait(timeout=5)\n"
                "thread=threading.Thread(target=worker); thread.start(); thread.join()\n")
        with program(host) as process:
            self.assertEqual(line(process), "ready")
            with ProcessTreeRSS(process.pid, 0.01, expect_children=True) as sampler:
                time.sleep(0.03)
            report = sampler.report()
            self.assertEqual(report["status"], "sampled")
            self.assertGreaterEqual(report["max_process_count"], 3)
            self.assertGreaterEqual(report["peak_rss_kib"], 32 * 1024)
            self.assertGreater(report["samples_with_descendants"], 0)
            self.assertGreater(report["observer_cpu_s"], 0)
            send(process, "exit")
            process.wait(timeout=5)

    def test_startup_partial_line_timeout_cleans_up_sigterm_resistant_descendant(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            script = root / "host"
            script.write_text(f"#!{sys.executable}\nimport pathlib,subprocess,sys,time\n"
                              "child=subprocess.Popen([sys.executable,'-c','import signal,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); print(1,flush=True); time.sleep(60)'],stdout=subprocess.PIPE)\n"
                              "child.stdout.readline()\n"
                              "(pathlib.Path(sys.argv[1])/'child.pid').write_text(str(child.pid))\n"
                              "print('http://127.0.0.1:',end='',flush=True); time.sleep(60)\n")
            script.chmod(0o755)
            server = Server(str(script), root, "unused", "zero", "smart", environment(), root, 1024,
                            startup_timeout=1)
            with self.assertRaisesRegex(TimeoutError, "startup timed out"):
                with server:
                    self.fail("partial startup line was accepted")
            await_dead(int((root / "child.pid").read_text()))
            self.assertIsNotNone(server.process.returncode)
            self.assertTrue(server.errors.closed)

    def test_command_timeout_and_success_reap_the_root_and_kill_background_children(self):
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "pid"
            spawn = ("import pathlib,subprocess,sys,time; "
                     "child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(60)']); "
                     f"pathlib.Path({str(marker)!r}).write_text(str(child.pid)); ")
            with self.assertRaisesRegex(TimeoutError, "exceeded"):
                measure([sys.executable, "-c", spawn + "time.sleep(60)"], environment(), "/usr/bin/time", timeout_s=1)
            await_dead(int(marker.read_text()))
            output, _ = measure([sys.executable, "-c", spawn + "print('complete')"], environment(), "/usr/bin/time")
            self.assertEqual(output.strip(), b"complete")
            await_dead(int(marker.read_text()))


if __name__ == "__main__":
    unittest.main()
