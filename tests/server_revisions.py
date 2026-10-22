"""Independent Git time/exclusion-deepening fixture and HTTP client oracle."""
import argparse
import datetime
import json
from pathlib import Path

from interop import command, reference_server, run
from server_fixture import export_fixture
from server_references import make_commit, make_tree, test_environment
from server_shallow import compare_delivered_bodies, delivered, shallow_ids
from upstream_http import git, object_manifest, verify_checkout


def revision_environment(root):
    env = test_environment(root)
    binary = Path(env["GIT_ZERO_TEST_GIT"])
    # Support an explicitly selected official in-tree Git build, whose compiled
    # install prefix is absent. Use only the helper beside that chosen binary.
    if binary.is_absolute() and (binary.parent / "git-http-backend").is_file():
        env["GIT_EXEC_PATH"] = str(binary.parent)
    return env


def prepare(root, object_format):
    env = revision_environment(root)
    bare = root / "repo.git"
    run("git", "init", "-q", "--bare", "--object-format=" + object_format,
        "-b", "main", str(bare), env=env)
    blob = git(bare, "hash-object", "-w", "--stdin", env=env,
               data=b"shared binary\0" + bytes(range(256)) * 32).strip().decode()
    tree = make_tree(bare, (("shared", blob),), env)
    parents = {"A": (), "B": ("A",), "C": ("B",), "D": ("C",), "E": ("B",), "M": ("D", "E")}
    dates = {"A": 100, "B": 300, "C": 200, "D": 400, "E": 350, "M": 500}
    ids = {"blob": blob, "tree": tree}
    for label, date in dates.items():
        dated = dict(env, GIT_COMMITTER_DATE=f"@{1700000000 + date} +0000")
        ids[label] = make_commit(bare, tree, [ids[parent] for parent in parents[label]], label, dated)
    for name, label in (("main", "D"), ("merge", "M"), ("cutoff", "B"), ("ambiguous", "B")):
        git(bare, "update-ref", "refs/heads/" + name, ids[label], env=env)
    git(bare, "symbolic-ref", "refs/heads/symcut", "refs/heads/cutoff", env=env)
    git(bare, "tag", "-a", "-m", "inner annotation", "temporary-inner", ids["C"], env=env)
    inner = git(bare, "rev-parse", "temporary-inner", env=env).strip().decode()
    git(bare, "tag", "-a", "-m", "outer annotation", "cut-tag", inner, env=env)
    git(bare, "tag", "-d", "temporary-inner", env=env)
    git(bare, "update-ref", "refs/tags/blob", blob, env=env)
    git(bare, "update-ref", "refs/tags/ambiguous", ids["C"], env=env)
    git(bare, "config", "uploadpack.allowFilter", "true", env=env)
    git(bare, "fsck", "--strict", env=env)
    manifest = export_fixture(root, bare, env)
    assert len(manifest) <= 64
    assert sum(len(body) for _, body in manifest.values()) <= 131072
    assert len((root / "refs.txt").read_text().splitlines()) == 8
    (root / "revision-ids.json").write_text(json.dumps(ids, indent=2) + "\n")


def since(offset):
    date = datetime.datetime.fromtimestamp(1700000000 + offset, datetime.timezone.utc)
    return "--shallow-since=" + date.isoformat()


def clone(root, env, name, url, version, branch, options):
    path = root / name
    result = command("git", "-c", f"protocol.version={version}", "clone", "-q",
                     "--single-branch", "--no-tags", "--branch", branch, *options,
                     url, str(path), env=env)
    return path, result


def compare_clients(bare, clients, env):
    left, right = clients
    assert shallow_ids(left) == shallow_ids(right), (shallow_ids(left), shallow_ids(right))
    assert delivered(left, env) == delivered(right, env)
    assert object_manifest(left, "HEAD", env) == object_manifest(right, "HEAD", env)
    compare_delivered_bodies(left, right, delivered(left, env), env)
    for path in clients:
        git(path, "fsck", "--strict", env=env)
        head = git(path, "rev-parse", "HEAD", env=env).strip().decode()
        verify_checkout(bare, path, head, env)


def compare(root, zero_url):
    env = revision_environment(root)
    bare = root / "repo.git"
    ids = json.loads((root / "revision-ids.json").read_text())
    cases = [
        ("equal-root", "main", [since(100)]), ("equal-C", "main", [since(200)]),
        ("skew-cut", "main", [since(250)]), ("merge-skew", "merge", [since(250)]),
        ("merge-parent-cut", "merge", [since(400)]),
        ("explicit-older-tips", "main", [since(400), "--no-single-branch"]),
        ("exclude-branch", "main", ["--shallow-exclude=cutoff"]),
        ("exclude-symbolic", "main", ["--shallow-exclude=symcut"]),
        ("exclude-nested-tag", "main", ["--shallow-exclude=cut-tag"]),
        ("exclude-shared-blob", "main", ["--shallow-exclude=blob"]),
        ("multiple", "main", ["--shallow-exclude=cutoff", "--shallow-exclude=cut-tag"]),
        ("combined", "main", [since(250), "--shallow-exclude=cutoff"]),
    ]
    failures = [("all-cut", [since(401)]), ("ambiguous", ["--shallow-exclude=ambiguous"]),
                ("raw-id", ["--shallow-exclude=" + ids["B"]])]
    with reference_server(root, env, True) as reference:
        urls = (("reference", reference), ("zero", zero_url))
        for version in (0, 1, 2):
            for case, branch, options in cases:
                clients = []
                for name, url in urls:
                    path, result = clone(root, env, f"{name}-v{version}-{case}", url, version, branch, options)
                    assert result.returncode == 0, (name, case, result.stderr)
                    clients.append(path)
                compare_clients(bare, clients, env)
            for case, options in failures:
                for name, url in urls:
                    _, result = clone(root, env, f"{name}-v{version}-{case}", url, version, "main", options)
                    assert result.returncode != 0, (name, case)
            for mode in ("since", "exclude"):
                clients = []
                for name, url in urls:
                    path, result = clone(root, env, f"{name}-v{version}-fetch-{mode}", url, version, "main", ["--depth=1"])
                    assert result.returncode == 0, result.stderr
                    clients.append(path)
                options = [since(200)] if mode == "since" else ["--shallow-exclude=cutoff"]
                for arguments in (options, options, [since(100)], [since(350)], ["--unshallow"]):
                    for path in clients:
                        git(path, "-c", f"protocol.version={version}", "fetch", "--no-tags", *arguments, "origin", env=env)
                    compare_clients(bare, clients, env)
            clients = []
            for name, url in urls:
                path, result = clone(root, env, f"{name}-v{version}-unrelated", url, version,
                                     "main", ["--depth=1", "--no-single-branch"])
                assert result.returncode == 0, result.stderr
                clients.append(path)
            for arguments, branch in (([since(200)], "main"), ([since(100)], "merge")):
                for path in clients:
                    git(path, "-c", f"protocol.version={version}", "fetch", "--no-tags", *arguments,
                        "origin", branch, env=env)
                compare_clients(bare, clients, env)
            print(f"v{version} revision clone/fetch/boundary/checkout oracle passed", flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("phase", choices=("prepare", "compare"))
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--object-format", choices=("sha1", "sha256"), default="sha1")
    parser.add_argument("--zero-url")
    args = parser.parse_args()
    if args.phase == "prepare":
        prepare(args.root, args.object_format)
    else:
        compare(args.root, args.zero_url)
