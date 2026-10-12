#!/usr/bin/env python3
"""Paired Git/git-zero benchmarks through one Rust HTTP host implementation."""
import argparse
import concurrent.futures
import contextlib
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

from interop import environment, run
from upstream_http import refs
from benchmark_fixture import create, verify
from benchmark_harness import Server
from benchmark_limits import evaluate, load
from benchmark_source import create as create_source, read as read_source


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
        host_load = os.getloadavg()
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
                    "max_rss_kib": rss_kib, "host_load_1_5_15": host_load}


def summary(samples):
    wall = sorted(sample["wall_s"] for sample in samples)
    # Nearest-rank p95 preserves an observed sample even for small runs.
    result = {"median_wall_s": statistics.median(wall),
            "p95_wall_s": wall[(95 * len(wall) + 99) // 100 - 1],
            "median_cpu_s": statistics.median(sample["cpu_s"] for sample in samples),
            "median_max_rss_kib": statistics.median(sample["max_rss_kib"] for sample in samples)}
    for key in ("cpu_s", "frontend_cpu_s", "backend_children_cpu_s", "response_bytes", "request_count",
                "frontend_rss_kib", "sendfile_bytes", "buffered_file_bytes"):
        result["median_server_" + key] = statistics.median(sample["server"][key] for sample in samples)
    # RSS high-water marks are lifetime observations, never counter deltas.
    for key in ("frontend_peak_rss_kib", "backend_child_peak_rss_kib"):
        result["server_" + key] = max(sample["server"][key] for sample in samples)
    return result


def benchmark(name, binary, harness, git, backend, fixture, expected, metadata, samples, warmup,
              time_binary, response_limit, file_transfer, zero_loose_cache):
    operation, protocol = name.rsplit("-", 1)
    smart = protocol == "smart"
    with tempfile.TemporaryDirectory(prefix=f"git-zero-benchmark-{name}-") as directory:
        root = Path(directory)
        env = environment(git)
        if not smart:
            env["GIT_SMART_HTTP"] = "0"
        bare, head = fixture / "repo.git", metadata["head"]
        expected_refs = {b"HEAD": head.encode(), b"refs/heads/main": head.encode()}
        results = {implementation: [] for implementation in ("git", "zero")}
        with contextlib.ExitStack() as stack:
            original = stack.enter_context(Server(harness, fixture, backend, "git", protocol, env, root,
                                                 response_limit, file_transfer, zero_loose_cache))
            zero = (original if operation == "client-discovery" else
                    stack.enter_context(Server(harness, fixture, backend, "zero", protocol, env, root,
                                               response_limit, file_transfer, zero_loose_cache)))
            servers = {"git": original, "zero": zero}
            urls = {implementation: server.url for implementation, server in servers.items()}
            fetch_targets = {}
            if operation == "server-fetch":
                for implementation, url in urls.items():
                    target = root / f"fetch-{implementation}"
                    run("git", "clone", "-q", url, str(target), env=env)
                    verify(bare, target, expected, env)
                    fetch_targets[implementation] = target
            for index in range(warmup + samples):
                # Reverse paired order each round to distribute host/cache drift.
                order = ("git", "zero") if index % 2 == 0 else ("zero", "git")
                for implementation in order:
                    url = urls[implementation]
                    if operation == "client-discovery":
                        arguments = (["git", "-c", "protocol.version=0", "ls-remote", original.url]
                                     if implementation == "git" else [binary, "ls-remote", original.url])
                    elif operation == "server-discovery":
                        arguments = ["git", "-c", "protocol.version=0", "ls-remote", url]
                    elif operation == "server-clone":
                        target = root / f"clone-{implementation}-{index}"
                        arguments = ["git", "-c", "protocol.version=0", "clone", "-q", url, str(target)]
                    else:
                        target = fetch_targets[implementation]
                        arguments = ["git", "-C", str(target), "-c", "protocol.version=0", "fetch", "origin"]
                    server = servers[implementation]
                    mark = len(server.records())
                    output, measurements = measure(arguments, env, time_binary)
                    measurements["server"] = server.measurements(mark)
                    if operation.endswith("discovery"):
                        assert refs(output) == expected_refs
                    else:
                        verify(bare, target, expected, env)
                    if index >= warmup:
                        results[implementation].append(measurements)
        summaries = {implementation: summary(values) for implementation, values in results.items()}
        return {"workload": name, "samples": results, "summary": summaries,
                "server_startup": {implementation: server.startup for implementation, server in servers.items()},
                "zero_over_git_wall_ratio": summaries["zero"]["median_wall_s"] / summaries["git"]["median_wall_s"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path(__file__).resolve().parents[1] / "target/release/git-zero")
    parser.add_argument("--harness", type=Path, default=Path(__file__).resolve().parents[1] / "target/release/examples/benchmark-server")
    parser.add_argument("--git", default=os.environ.get("GIT_ZERO_TEST_GIT", "git"))
    parser.add_argument("--time", default="/usr/bin/time", help="GNU time executable for command RSS")
    parser.add_argument("--samples", type=int, default=25)
    parser.add_argument("--warmup", type=int, default=3)
    parser.add_argument("--jobs", type=int, default=1, help="Independent workload workers; 1 avoids benchmark contention")
    parser.add_argument("--case", action="append", choices=WORKLOADS)
    parser.add_argument("--output", type=Path, required=True, help="JSON report including every raw measurement")
    parser.add_argument("--limits", type=Path, help="JSON upper bounds for selected workload/implementation metrics")
    parser.add_argument("--fixture-dir", type=Path, help="Retain the prepared repository/export in a new directory")
    parser.add_argument("--source-repo", type=Path, help="Read a caller repository instead of generating a synthetic fixture")
    parser.add_argument("--revision", help="Full lowercase SHA-1 commit ID; required with --source-repo")
    parser.add_argument("--max-source-objects", type=int, default=8192, help="Object-count ceiling before loading source bodies")
    parser.add_argument("--max-source-bytes", type=int, default=64 * 1024 * 1024, help="Raw-body byte ceiling before loading source bodies")
    parser.add_argument("--files", type=int, help="Synthetic file count (default: 64)")
    parser.add_argument("--blob-bytes", type=int, help="Synthetic bytes per blob (default: 16384)")
    parser.add_argument("--commits", type=int, help="Synthetic history length (default: 16)")
    parser.add_argument("--git-compression", type=int, choices=range(10), default=0, help="0 matches stored-DEFLATE; 6 shows compression tradeoffs")
    parser.add_argument("--response-limit", type=int, default=32 * 1024 * 1024, help="Per-response body budget in the Rust host")
    parser.add_argument("--file-transfer", choices=("auto", "buffered"), default="auto",
                        help="auto tries sendfile for encoded files, with bounded pread/send fallback")
    parser.add_argument("--zero-loose-cache", action="store_true",
                        help="Preencode Rust loose objects at server startup to compare file serving on both dumb backends")
    args = parser.parse_args()
    if bool(args.source_repo) != bool(args.revision):
        parser.error("--source-repo and --revision must be supplied together")
    dimensions = (args.files, args.blob_bytes, args.commits)
    if args.source_repo and any(value is not None for value in dimensions):
        parser.error("synthetic dimensions cannot be combined with --source-repo")
    args.files, args.blob_bytes, args.commits = (
        default if value is None else value for value, default in zip(dimensions, (64, 16384, 16)))
    if args.samples < 1 or args.warmup < 0 or args.jobs < 1:
        parser.error("samples/jobs must be positive and warmup nonnegative")
    if min(args.max_source_objects, args.max_source_bytes) < 1:
        parser.error("source object and byte limits must be positive")
    if min(args.files, args.commits, args.response_limit) < 1 or args.blob_bytes < 0:
        parser.error("files/commits/response-limit must be positive and blob-bytes nonnegative")
    cases = list(dict.fromkeys(args.case or WORKLOADS))
    try:
        limits = load(args.limits, cases) if args.limits else {"schema_version": 1, "limits": []}
    except (OSError, ValueError) as error:
        parser.error(str(error))
    if platform.system() != "Linux" or not hasattr(os, "wait4"):
        parser.error("CPU/RSS measurement currently requires Linux")
    binary = str(args.binary.resolve())
    harness = str(args.harness.resolve())
    if not Path(binary).is_file() or not Path(harness).is_file():
        parser.error("build the native git-zero binary and benchmark-server example first")
    repository_root = Path(__file__).resolve().parents[1]
    git = str(Path(args.git).resolve()) if os.sep in args.git else args.git
    env = environment(git)
    source = None
    if args.source_repo:
        try:
            source = read_source(args.source_repo, args.revision, env,
                                 args.max_source_objects, args.max_source_bytes,
                                 (args.output, args.fixture_dir))
        except (OSError, ValueError, AssertionError, subprocess.TimeoutExpired) as error:
            parser.error(str(error))
    backend = str(Path(run("git", "--exec-path", env=env).decode().strip()) / "git-http-backend")
    if b"GNU Time" not in run(args.time, "--version", env=env):
        parser.error("--time must select GNU time")
    report = {
        "schema_version": 3, "git_version": run("git", "--version", env=env).decode().strip(),
        "git_zero_binary_sha256": hashlib.sha256(Path(binary).read_bytes()).hexdigest(),
        "harness_sha256": hashlib.sha256(Path(harness).read_bytes()).hexdigest(),
        "git_http_backend_sha256": hashlib.sha256(Path(backend).read_bytes()).hexdigest(),
        "source_revision": run("git", "rev-parse", "HEAD", cwd=repository_root, env=env).decode().strip(),
        "source_dirty": bool(run("git", "status", "--porcelain", cwd=repository_root, env=env)),
        "platform": platform.platform(), "cpu_count": os.cpu_count(),
        "cpu_affinity": sorted(os.sched_getaffinity(0)), "host_load_start_1_5_15": os.getloadavg(),
        "rss_observer": run(args.time, "--version", env=env).decode().splitlines()[0],
        "samples_per_implementation": args.samples, "warmup_rounds": args.warmup, "jobs": args.jobs,
        "file_transfer": args.file_transfer, "zero_loose_cache": args.zero_loose_cache,
        "budgets": {"request_bytes": 65536, "object_read_bytes": 65536, "response_bytes": args.response_limit},
        "resource_limits": limits,
        "measurement_scope": "Fresh client command tree; CPU/RSS exclude server processes. "
                             "CPU includes the small GNU time observer; RSS excludes Python launcher memory. "
                             "Persistent warm servers and OS caches; no cold-cache claim. "
                             "Both backends use the same Rust HTTP parser/response writer and file-backed object graph. "
                             "Git smart HTTP uses CGI/upload-pack; Git dumb HTTP serves loose files directly through Rust. "
                             "Encoded files use the selected common transfer path; Rust loose caching is explicit and charged to startup. "
                             "Server request CPU includes frontend and reaped Git children via getrusage deltas. "
                             "Server RSS separates frontend /proc VmRSS/VmHWM and largest-child lifetime ru_maxrss; "
                             "these peaks are not simultaneous aggregate memory and must not be added. "
                             "Server initialization, control barriers and metric reporting are outside request intervals. "
                             "Rust generates stored-DEFLATE responses; Git compression is configurable, deltas disabled.",
        "expected_workloads": cases, "results": [], "errors": [],
    }
    print(f"{report['git_version']}; {args.samples} paired samples, {args.jobs} workload workers", flush=True)
    print("workload | Git/zero wall ms | ratio | server CPU ms | frontend peak KiB | child peak KiB", flush=True)
    fixture_context = (contextlib.nullcontext(args.fixture_dir.resolve()) if args.fixture_dir else
                       tempfile.TemporaryDirectory(prefix="git-zero-benchmark-fixture-"))
    with fixture_context as directory:
        fixture = Path(directory)
        if args.fixture_dir:
            fixture.mkdir(parents=True, exist_ok=False)
        if source is None:
            bare, expected, metadata = create(fixture, env, args.files, args.blob_bytes, args.commits, args.git_compression)
        else:
            expected, description = source
            bare, expected, metadata = create_source(fixture, env, args.revision, expected, description,
                                                     args.git_compression)
        report["fixture"] = metadata
        report["fixture_directory"] = str(fixture) if args.fixture_dir else None
        print(f"Fixture: {metadata['objects']} objects, {metadata['raw_object_bytes']} raw bytes, HEAD {metadata['head']}", flush=True)
        with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as executor:
            futures = {executor.submit(benchmark, name, binary, harness, git, backend, fixture, expected, metadata,
                                       args.samples, args.warmup, args.time, args.response_limit,
                                       args.file_transfer, args.zero_loose_cache): name for name in cases}
            for future in concurrent.futures.as_completed(futures):
                try:
                    result = future.result()
                except Exception as error:
                    failure = {"workload": futures[future], "type": type(error).__name__, "message": str(error)}
                    report["errors"].append(failure)
                    print(f"{failure['workload']} FAILED: {failure['type']}: {failure['message']}", flush=True)
                    continue
                report["results"].append(result)
                original, zero = result["summary"]["git"], result["summary"]["zero"]
                print(f"{result['workload']} | {original['median_wall_s'] * 1000:.3f}/{zero['median_wall_s'] * 1000:.3f} | "
                      f"{result['zero_over_git_wall_ratio']:.3f} | "
                      f"{original['median_server_cpu_s'] * 1000:.3f}/{zero['median_server_cpu_s'] * 1000:.3f} | "
                      f"{original['server_frontend_peak_rss_kib']}/{zero['server_frontend_peak_rss_kib']} | "
                      f"{original['server_backend_child_peak_rss_kib']}/{zero['server_backend_child_peak_rss_kib']}", flush=True)
    report["results"].sort(key=lambda result: result["workload"])
    report["budget_evaluations"] = evaluate(limits, report["results"])
    violations = [entry for entry in report["budget_evaluations"] if entry["status"] != "passed"]
    for entry in violations:
        print(f"BUDGET FAILED: {entry['workload']}/{entry['implementation']} {entry['metric']}: "
              f"{entry['observed_maximum']} <= {entry['maximum']} ({entry['status']})", flush=True)
    report["status"] = "failed" if report["errors"] or violations else "complete"
    report["host_load_end_1_5_15"] = os.getloadavg()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(f"Raw measurements: {args.output}", flush=True)
    if report["status"] == "failed":
        raise SystemExit("Benchmark failed; measurements, errors and budget evaluations are retained in the JSON report")


if __name__ == "__main__":
    main()
