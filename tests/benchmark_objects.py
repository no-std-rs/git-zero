"""Paired local object reads from identical compressed loose/pack inputs."""
import hashlib

from interop import run
from upstream_http import git


WORKLOADS = ["client-object-read-loose", "client-object-read-pack"]


def prepare_pack(fixture, env, config):
    """Prepare separate owned packed storage; HTTP keeps its loose fixture."""
    repository = fixture / "repo-packed.git"
    run("git", "clone", "-q", "--bare", "--no-hardlinks", str(fixture / "repo.git"),
        str(repository), env=env)
    for key, value in config.overrides().items():
        git(repository, "config", key, str(value), env=env)
    git(repository, "repack", "-a", "-d", "-f", env=env)
    git(repository, "prune-packed", env=env)
    git(repository, "fsck", "--strict", env=env)
    packs = sorted((repository / "objects/pack").glob("*.pack"))
    if len(packs) != 1:
        raise ValueError("object-read fixture requires one complete pack")
    pack = packs[0]
    index = pack.with_suffix(".idx")
    git(repository, "index-pack", "--strict", str(pack), env=env)
    rows = [line.split() for line in git(repository, "verify-pack", "-v", str(index), env=env).splitlines()]
    deltas = [row for row in rows if len(row) == 7]
    return {"repository": str(repository), "path": str(pack), "bytes": pack.stat().st_size,
            "sha256": hashlib.sha256(pack.read_bytes()).hexdigest(),
            "index_sha256": hashlib.sha256(index.read_bytes()).hexdigest(),
            "inflated_entry_bytes": sum(int(row[2]) for row in rows if len(row) in (5, 7)),
            "delta_objects": len(deltas), "max_delta_depth": max((int(row[5]) for row in deltas), default=0)}


def benchmark(name, decoder, fixture, expected, metadata, env, samples, warmup, time_binary,
              measure, summarize):
    packed = name.endswith("-pack")
    repository = metadata["decoder_pack"]["repository"] if packed else str(fixture / "repo.git")
    source = metadata["decoder_pack"]["path"] if packed else repository
    mode = "pack" if packed else "loose"
    ids = sorted(expected)
    request = b"\n".join(ids) + b"\n"
    expected_output = b"".join(oid + b" " + expected[oid][0] + b" " + str(len(expected[oid][1])).encode()
                               + b"\n" + expected[oid][1] + b"\n" for oid in ids)
    arguments = {"git": ["git", "-C", repository, "cat-file", "--batch"],
                 "zero": [decoder, mode, source, str(fixture / "objects.tsv")]}
    results = {implementation: [] for implementation in arguments}
    # Rotate paired executions: Git/zero, then zero/Git, preserving warmup pairs.
    order = ("git", "zero", "zero", "git")
    for iteration in range(2 * (warmup + samples)):
        implementation = order[iteration % 4]
        output, measurements = measure(arguments[implementation], env, time_binary,
                                       request if implementation == "git" else b"")
        assert output == expected_output, f"{implementation} object bodies/IDs differ"
        measurements.update({"decoded_objects": len(ids), "decoded_body_bytes": metadata["raw_object_bytes"],
                             "output_bytes": len(output)})
        if iteration >= 2 * warmup:
            results[implementation].append(measurements)
    summaries = {implementation: summarize(values) for implementation, values in results.items()}
    return {"workload": name, "execution": "local-object-read", "samples": results, "summary": summaries,
            "equivalent_inputs": True, "compression_and_deltas": "Identical encoded files for both readers",
            "measurement_scope": "Complete fresh process, file I/O, decoding, materialization and batch output; "
                                 "warm OS caches. Rust also verifies checksums/canonical IDs and allocates host base slots. "
                                 "This is not an isolated inflater CPU measurement or a complete client fetch.",
            "compressed_input_bytes": metadata["decoder_pack"]["bytes"] if packed else metadata["git_loose_object_bytes"],
            "zero_over_git_wall_ratio": summaries["zero"]["median_wall_s"] / summaries["git"]["median_wall_s"]}
