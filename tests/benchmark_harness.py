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

    def measurements(self, start):
        records = self.records()[start:]
        assert records and all(r["event"] == "request" and r["backend"] == self.implementation for r in records)
        assert all(r["status"] in (200, 404) for r in records)
        assert all(r["wall_s"] > 0 and r["frontend_cpu_s"] >= 0 and r["backend_children_cpu_s"] >= 0
                   and 0 < r["frontend_rss_kib"] <= r["frontend_peak_rss_kib"] for r in records)
        assert all(0 <= r["sendfile_bytes"] + r["buffered_file_bytes"] <= r["response_bytes"] for r in records)
        if self.arguments[7] == "buffered":
            assert all(r["sendfile_bytes"] == 0 for r in records)
        if self.implementation == "zero" or self.arguments[4] == "dumb":
            assert all(r["backend_children_cpu_s"] == 0 and r["backend_child_peak_rss_kib"] == 0 for r in records)
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
        try:
            os.killpg(self.process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(self.process.pid, signal.SIGKILL)
            self.process.wait()
        # The frontend may have exited before a descendant ignoring SIGTERM.
        # Keep cleanup scoped to the session we created, even after reaping it.
        try:
            os.killpg(self.process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        self.process.stdout.close()
        self.errors.seek(0)
        errors = self.errors.read().decode(errors="replace")
        self.errors.close()
        if failed:
            raise RuntimeError(f"Rust benchmark host failed: {errors}")
