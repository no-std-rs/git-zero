#!/usr/bin/env python3
"""Opt-in paired Git/git-zero HTTP benchmarks; Linux with GNU time."""
import argparse
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import platform
import signal
import statistics
import subprocess
import tempfile
import threading
import time

from interop import environment, reference_server, run, zero_server
from upstream_http import compare_repository, refs, snapshot_fixture, verify_checkout


WORKLOADS = [f"{operation}-{protocol}" for operation in
             ("client-discovery", "server-discovery", "server-clone", "server-fetch")
             for protocol in ("smart", "dumb")]


def measure(arguments, env, time_binary):
    """wait4 attributes CPU/RSS to this command, even with concurrent workers.

    Output files avoid pipe deadlocks and host-side output parsing during timing.
    CPU includes the small GNU time observer and waited client descendants;
    RSS measures the command tree after exec, excluding the Python launcher.
    Both exclude HTTP server processes.
    """
    if arguments[0] == "git":
        arguments = [env["GIT_ZERO_TEST_GIT"], *arguments[1:]]
    with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr, tempfile.NamedTemporaryFile() as rss:
        observed = [time_binary, "-f", "%M", "-o", rss.name, "--", *arguments]
        start = time.perf_counter()
        process = subprocess.Popen(observed, env=env, stdout=stdout, stderr=stderr, start_new_session=True)

        def timeout():
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass

        watchdog = threading.Timer(30, timeout)
        watchdog.start()
        try:
            _, status, usage = os.wait4(process.pid, 0)
            elapsed = time.perf_counter() - start
            process.returncode = os.waitstatus_to_exitcode(status)
        finally:
            watchdog.cancel()
            watchdog.join()
        stdout.seek(0)
        stderr.seek(0)
        output = stdout.read()
        assert process.returncode == 0, f"{arguments!r}: {stderr.read().decode(errors='replace')}"
        rss_kib = int(Path(rss.name).read_text().strip())
    return output, {"wall_s": elapsed, "cpu_s": usage.ru_utime + usage.ru_stime,
                    "max_rss_kib": rss_kib}


def summary(samples):
    wall = sorted(sample["wall_s"] for sample in samples)
    # Nearest-rank p95 preserves an observed sample even for small runs.
    return {"median_wall_s": statistics.median(wall),
            "p95_wall_s": wall[(95 * len(wall) + 99) // 100 - 1],
            "median_cpu_s": statistics.median(sample["cpu_s"] for sample in samples),
            "median_max_rss_kib": statistics.median(sample["max_rss_kib"] for sample in samples)}


def benchmark(name, binary, git, samples, warmup, time_binary):
    operation, protocol = name.rsplit("-", 1)
    smart = protocol == "smart"
    with tempfile.TemporaryDirectory(prefix=f"git-zero-benchmark-{name}-") as directory:
        root = Path(directory)
        env = environment(git)
        if not smart:
            env["GIT_SMART_HTTP"] = "0"
        bare, head = snapshot_fixture(root, env)
        expected_refs = {b"HEAD": head.encode(), b"refs/heads/main": head.encode()}
        results = {implementation: [] for implementation in ("git", "zero")}
        with reference_server(root, env, smart) as reference, zero_server(binary, env) as zero:
            urls = {"git": reference, "zero": zero}
            # Client comparison holds the Git reference server fixed. Server
            # comparison holds the real Git client and snapshot contents fixed.
            fetch_targets = {}
            if operation == "server-fetch":
                for implementation, url in urls.items():
                    target = root / f"fetch-{implementation}"
                    run("git", "clone", "-q", url, str(target), env=env)
                    compare_repository(bare, target, "--all", env)
                    fetch_targets[implementation] = target
            for index in range(warmup + samples):
                # Reverse paired order each round to distribute host/cache drift.
                order = ("git", "zero") if index % 2 == 0 else ("zero", "git")
                for implementation in order:
                    url = urls[implementation]
                    if operation == "client-discovery":
                        arguments = (["git", "-c", "protocol.version=0", "ls-remote", reference]
                                     if implementation == "git" else [binary, "ls-remote", reference])
                    elif operation == "server-discovery":
                        arguments = ["git", "-c", "protocol.version=0", "ls-remote", url]
                    elif operation == "server-clone":
                        target = root / f"clone-{implementation}-{index}"
                        arguments = ["git", "-c", "protocol.version=0", "clone", "-q", url, str(target)]
                    else:
                        target = fetch_targets[implementation]
                        arguments = ["git", "-C", str(target), "-c", "protocol.version=0", "fetch", "origin"]
                    output, measurements = measure(arguments, env, time_binary)
                    if operation.endswith("discovery"):
                        assert refs(output) == expected_refs
                    else:
                        compare_repository(bare, target, "--all", env)
                        verify_checkout(bare, target, head, env)
                    if index >= warmup:
                        results[implementation].append(measurements)
        summaries = {implementation: summary(values) for implementation, values in results.items()}
        return {"workload": name, "samples": results, "summary": summaries,
                "zero_over_git_wall_ratio": summaries["zero"]["median_wall_s"] / summaries["git"]["median_wall_s"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path(__file__).resolve().parents[1] / "target/release/git-zero")
    parser.add_argument("--git", default=os.environ.get("GIT_ZERO_TEST_GIT", "git"))
    parser.add_argument("--time", default="/usr/bin/time", help="GNU time executable for command RSS")
    parser.add_argument("--samples", type=int, default=25)
    parser.add_argument("--warmup", type=int, default=3)
    parser.add_argument("--jobs", type=int, default=1, help="Independent workload workers; 1 avoids benchmark contention")
    parser.add_argument("--case", action="append", choices=WORKLOADS)
    parser.add_argument("--output", type=Path, required=True, help="JSON report including every raw measurement")
    args = parser.parse_args()
    if args.samples < 1 or args.warmup < 0 or args.jobs < 1:
        parser.error("samples/jobs must be positive and warmup nonnegative")
    if platform.system() != "Linux" or not hasattr(os, "wait4"):
        parser.error("CPU/RSS measurement currently requires Linux")
    binary = str(args.binary.resolve())
    repository_root = Path(__file__).resolve().parents[1]
    git = str(Path(args.git).resolve()) if os.sep in args.git else args.git
    env = environment(git)
    if b"GNU Time" not in run(args.time, "--version", env=env):
        parser.error("--time must select GNU time")
    cases = list(dict.fromkeys(args.case or WORKLOADS))
    report = {
        "schema_version": 1, "git_version": run("git", "--version", env=env).decode().strip(),
        "git_zero_binary_sha256": hashlib.sha256(Path(binary).read_bytes()).hexdigest(),
        "source_revision": run("git", "rev-parse", "HEAD", cwd=repository_root, env=env).decode().strip(),
        "source_dirty": bool(run("git", "status", "--porcelain", cwd=repository_root, env=env)),
        "platform": platform.platform(), "cpu_count": os.cpu_count(),
        "rss_observer": run(args.time, "--version", env=env).decode().splitlines()[0],
        "samples_per_implementation": args.samples, "warmup_rounds": args.warmup, "jobs": args.jobs,
        "fixture": {"files": 1, "objects": 3, "file_bytes": 56, "history_commits": 1},
        "measurement_scope": "Fresh client command tree; CPU/RSS exclude server processes. "
                             "CPU includes the small GNU time observer; RSS excludes Python launcher memory. "
                             "Persistent warm servers and OS caches; no cold-cache claim. "
                             "Git server uses Python HTTP/CGI shim; zero uses native rustix. "
                             "Results describe these loopback deployments, not encoder throughput.",
        "results": [],
    }
    print(f"{report['git_version']}; {args.samples} paired samples, {args.jobs} workload workers", flush=True)
    print("workload | Git median ms | zero median ms | zero/Git | Git/zero CPU ms | Git/zero RSS KiB", flush=True)
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as executor:
        futures = {executor.submit(benchmark, name, binary, git, args.samples, args.warmup, args.time): name for name in cases}
        for future in concurrent.futures.as_completed(futures):
            result = future.result()
            report["results"].append(result)
            original, zero = result["summary"]["git"], result["summary"]["zero"]
            print(f"{result['workload']} | {original['median_wall_s'] * 1000:.3f} | "
                  f"{zero['median_wall_s'] * 1000:.3f} | {result['zero_over_git_wall_ratio']:.3f} | "
                  f"{original['median_cpu_s'] * 1000:.3f}/{zero['median_cpu_s'] * 1000:.3f} | "
                  f"{original['median_max_rss_kib']:.0f}/{zero['median_max_rss_kib']:.0f}", flush=True)
    report["results"].sort(key=lambda result: result["workload"])
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(f"Raw measurements: {args.output}", flush=True)


if __name__ == "__main__":
    main()
