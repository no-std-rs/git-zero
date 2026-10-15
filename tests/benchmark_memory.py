"""Bounded Linux /proc sampling of one live server and its descendants.

Each peak is a sum from one sampling sweep, never a sum of lifetime peaks.
Sweeps are not atomic and can miss short-lived processes or between-sample
peaks. Shared pages count in each process's RSS; this is not physical/PSS memory.
"""
import os
from pathlib import Path
import threading
import time


LIMITS = {"processes": 256, "tasks": 1024, "read_bytes": 1_048_576, "sample_wall_s": 0.25}


class SamplingError(RuntimeError):
    pass


class Budget:
    def __init__(self, limits):
        self.remaining = dict(limits)
        self.deadline = time.monotonic() + limits["sample_wall_s"]

    def charge(self, kind, amount=1):
        self.remaining[kind] -= amount
        if self.remaining[kind] < 0:
            raise SamplingError(f"process-tree RSS observer exceeded {kind} budget")
        if time.monotonic() > self.deadline:
            raise SamplingError("process-tree RSS observer exceeded sample wall-time budget")

    def read(self, path):
        self.charge("read_bytes", 0)
        try:
            with path.open("rb") as source:
                data = source.read(min(self.remaining["read_bytes"], 65536) + 1)
        except (FileNotFoundError, ProcessLookupError):
            return None
        self.charge("read_bytes", len(data))
        if len(data) > 65536:
            raise SamplingError("process-tree RSS observer exceeded per-file read budget")
        return data


class TreeReader:
    def __init__(self, pid, proc=Path("/proc"), limits=LIMITS):
        self.pid = pid
        self.proc = proc
        self.limits = limits
        self.start_time = None
        self.page_kib = os.sysconf("SC_PAGE_SIZE") // 1024

    def parse_stat(self, data):
        if data is None:
            return None
        fields = data.rpartition(b")")[2].split()
        if len(fields) < 22:
            raise SamplingError("invalid /proc stat during process-tree RSS sampling")
        return fields[0], int(fields[1]), int(fields[19]), int(fields[21]) * self.page_kib

    def stat(self, pid, budget):
        return self.parse_stat(budget.read(self.proc / str(pid) / "stat"))

    def task_children(self, path, budget):
        data = budget.read(path / "children")
        if data is not None:
            return (int(value) for value in data.split())
        live = self.parse_stat(budget.read(path / "stat"))
        if live is not None and live[0] not in (b"Z", b"X"):
            raise SamplingError("Linux proc children interface unavailable for live task")
        return ()

    def children(self, pid, budget):
        # Linux child lists are per thread; reading only the thread-group
        # leader misses children forked by another host/backend thread.
        try:
            with os.scandir(self.proc / str(pid) / "task") as tasks:
                for task in tasks:
                    if not task.name.isdecimal():
                        continue
                    budget.charge("tasks")
                    yield from self.task_children(Path(task.path), budget)
        except (FileNotFoundError, ProcessLookupError):
            return

    def memory(self, pid, parent, budget):
        record = self.stat(pid, budget)
        if record is None or record[0] in (b"Z", b"X"):
            if parent is None:
                raise SamplingError("server exited during process-tree RSS sampling")
            return None
        _, ppid, start, memory = record
        if parent is None:
            if self.start_time is not None and start != self.start_time:
                raise SamplingError("server PID changed during process-tree RSS sampling")
            self.start_time = start
        elif ppid != parent or start < self.start_time:
            return None  # The child exited/reparented, or its PID was reused.
        if memory < 0:
            raise SamplingError("invalid negative process RSS")
        return memory

    def walk(self, budget):
        pending, seen = [(self.pid, None)], set()
        while pending:
            pid, parent = pending.pop()
            if pid in seen:
                continue
            seen.add(pid)
            budget.charge("processes")
            memory = self.memory(pid, parent, budget)
            if memory is None:
                continue
            yield memory
            for child in self.children(pid, budget):
                if child in seen:
                    continue
                pending.append((child, pid))
                if len(pending) + len(seen) > self.limits["processes"]:
                    raise SamplingError("process-tree RSS observer exceeded processes budget")

    def sample(self):
        budget = Budget(self.limits)
        # At most limits["processes"] scalar observations are retained.
        observations = list(self.walk(budget))
        budget.charge("read_bytes", 0)
        return sum(observations), len(observations)


class ProcessTreeRSS:
    def __init__(self, pid, interval_s, expect_children=False, reader=None):
        if not 0.01 <= interval_s <= 1:
            raise ValueError("RSS sampling interval must be between 10 and 1000 ms")
        self.reader = reader or TreeReader(pid)
        self.interval_s = interval_s
        self.expect_children = expect_children
        self.stop = threading.Event()
        self.thread = None
        self.error = None
        self.completed = False
        self.samples = self.with_children = self.peak = self.peak_count = self.max_count = 0
        self.wall_s = self.cpu_s = self.max_wall_s = self.max_gap_s = 0
        self.previous = None

    def observe(self):
        self.completed = False
        start, cpu = time.monotonic(), time.thread_time()
        try:
            rss, count = self.reader.sample()
        finally:
            elapsed = time.monotonic() - start
            self.wall_s += elapsed
            self.cpu_s += time.thread_time() - cpu
            self.max_wall_s = max(self.max_wall_s, elapsed)
        if self.previous is not None:
            self.max_gap_s = max(self.max_gap_s, start - self.previous)
        self.previous = start
        self.samples += 1
        self.with_children += count > 1
        self.max_count = max(self.max_count, count)
        if rss > self.peak:
            self.peak, self.peak_count = rss, count

    def run(self):
        try:
            while not self.stop.wait(self.interval_s):
                self.observe()
        except Exception as error:
            self.error = error
            self.stop.set()

    def __enter__(self):
        self.started = time.monotonic()
        self.observe()
        self.thread = threading.Thread(target=self.run, name="benchmark-rss", daemon=True)
        self.thread.start()
        return self

    def __exit__(self, kind, *unused):
        self.stop.set()
        self.thread.join(timeout=self.reader.limits["sample_wall_s"] + 1)
        if kind is None:
            if self.thread.is_alive():
                raise SamplingError("process-tree RSS observer did not stop")
            if self.error is not None:
                raise SamplingError(f"process-tree RSS observation failed: {self.error}") from self.error
            self.observe()
            self.completed = True
        self.elapsed_s = time.monotonic() - self.started

    def report(self):
        if not self.completed or self.thread.is_alive() or self.error is not None:
            raise SamplingError("process-tree RSS observation is unavailable")
        partial = self.expect_children and not self.with_children
        return {"status": "partial" if partial else "sampled",
                "reason": "no live CGI descendant observed between samples" if partial else None,
                "peak_rss_kib": None if partial else self.peak,
                "observed_peak_rss_kib": self.peak, "processes_at_peak": self.peak_count,
                "max_process_count": self.max_count, "samples": self.samples,
                "samples_with_descendants": self.with_children, "interval_s": self.interval_s,
                "max_gap_s": self.max_gap_s, "elapsed_s": self.elapsed_s,
                "observer_wall_s": self.wall_s, "observer_cpu_s": self.cpu_s,
                "max_sample_wall_s": self.max_wall_s}
