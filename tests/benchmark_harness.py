"""Lifecycle and metric collection for the shared Rust benchmark host."""
import contextlib
import json
import os
from pathlib import Path
import selectors
import signal
import subprocess
import tempfile
import time
import urllib.request

from benchmark_memory import ProcessTreeRSS


def signal_group(pid, kind):
    """Signal only an owned session, tolerating an already-exited group."""
    try:
        os.killpg(pid, kind)
        return True
    except ProcessLookupError:
        return False


def read_origin(process, timeout):
    deadline, data = time.monotonic() + timeout, bytearray()
    with selectors.DefaultSelector() as selector:
        selector.register(process.stdout, selectors.EVENT_READ)
        while b"\n" not in data:
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not selector.select(timeout=remaining):
                raise TimeoutError("Rust benchmark host startup timed out")
            chunk = os.read(process.stdout.fileno(), 4096)
            if not chunk or len(data) + len(chunk) > 4096:
                raise RuntimeError("Rust benchmark host returned an invalid listening socket")
            data.extend(chunk)
    return data.split(b"\n", 1)[0].decode().strip()


class Server:
    def __init__(self, harness, fixture, backend, implementation, protocol, env, root, response_limit,
                 file_transfer="auto", zero_loose_cache=False, zero_selection=False, startup_timeout=60):
        self.arguments = [harness, str(fixture), backend, implementation, protocol,
                          str(root / f"{implementation}-metrics.jsonl"), str(response_limit),
                          file_transfer, "cached" if zero_loose_cache else "generated"]
        self.path = Path(self.arguments[5])
        self.implementation = implementation
        self.startup_timeout = startup_timeout
        self.env = dict(env)
        self.env.pop("GIT_ZERO_BENCH_SELECTION", None)
        self.selection = zero_selection and implementation == "zero"
        if self.selection:
            self.env["GIT_ZERO_BENCH_SELECTION"] = "1"

    def __enter__(self):
        self.errors = tempfile.TemporaryFile()
        try:
            self.process = subprocess.Popen(self.arguments, env=self.env, stdin=subprocess.DEVNULL,
                                            stdout=subprocess.PIPE, stderr=self.errors, start_new_session=True)
        except BaseException:
            self.errors.close()
            raise
        try:
            self.origin = read_origin(self.process, self.startup_timeout)
            if not self.origin.startswith("http://127.0.0.1:"):
                raise RuntimeError("Rust benchmark host did not report its listening socket")
            self.url = f"{self.origin}/{self.implementation}/repo.git"
            self.startup = self.records()[0]
            assert self.startup["event"] == "ready"
            assert self.startup.get("selection_enabled", False) == self.selection
            return self
        except BaseException:
            self.__exit__(None, None, None)
            raise

    def records(self):
        # Control requests run outside timed intervals, and produce no metric
        # records. The single-threaded host has finished earlier requests here.
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        with opener.open(self.origin + "/__barrier", timeout=10) as response:
            assert response.status == 204
        return [json.loads(line) for line in self.path.read_text().splitlines()]

    def validate_request(self, record):
        assert record["event"] == "request" and record["backend"] == self.implementation
        assert record["status"] in (200, 404)
        assert record["wall_s"] > 0 and record["frontend_cpu_s"] >= 0 and record["backend_children_cpu_s"] >= 0
        assert 0 < record["frontend_rss_kib"] <= record["frontend_peak_rss_kib"]
        assert 0 <= record["sendfile_bytes"] + record["buffered_file_bytes"] <= record["response_bytes"]
        if self.arguments[7] == "buffered":
            assert record["sendfile_bytes"] == 0
        if self.implementation == "zero" or self.arguments[4] == "dumb":
            assert record["backend_children_cpu_s"] == 0 and record["backend_child_peak_rss_kib"] == 0

    def measurements(self, start):
        records = self.records()[start:]
        assert records
        for record in records:
            self.validate_request(record)
        result = {"served_by": self.implementation, "request_count": len(records), "requests": records}
        for key in ("wall_s", "frontend_cpu_s", "backend_children_cpu_s", "response_bytes",
                    "sendfile_bytes", "buffered_file_bytes", "minor_faults", "major_faults"):
            result[key] = sum(r[key] for r in records)
        for key in ("frontend_rss_kib", "frontend_peak_rss_kib", "backend_child_peak_rss_kib"):
            result[key] = max(r[key] for r in records)
        result["cpu_s"] = result["frontend_cpu_s"] + result["backend_children_cpu_s"]
        return result

    def observe_rss(self, interval_s):
        if not interval_s:
            return contextlib.nullcontext()
        return ProcessTreeRSS(self.process.pid, interval_s,
                              expect_children=self.implementation == "git" and self.arguments[4] == "smart")

    def __exit__(self, *unused):
        failed = self.process.poll() not in (None, 0)
        signal_group(self.process.pid, signal.SIGTERM)
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            signal_group(self.process.pid, signal.SIGKILL)
            self.process.wait()
        # The frontend may have exited before a descendant ignoring SIGTERM.
        # Keep cleanup scoped to the session we created, even after reaping it.
        signal_group(self.process.pid, signal.SIGKILL)
        self.process.stdout.close()
        self.errors.seek(0)
        errors = self.errors.read().decode(errors="replace")
        self.errors.close()
        if failed:
            raise RuntimeError(f"Rust benchmark host failed: {errors}")
