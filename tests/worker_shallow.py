"""Run the existing independent shallow oracle against a bounded Worker host."""
import argparse
from itertools import product
from pathlib import Path

from interop import reference_server
from server_revisions import revision_environment, compare as compare_revisions
from server_shallow import compare_clone


def compare(root, url):
    env = revision_environment(root)
    bare = root / "repo.git"
    with reference_server(root, env, True) as reference:
        urls = (("reference", reference), ("zero", url))
        for version, branch, depth in product((0, 1, 2), ("main", "merge"), (1, 2, 8)):
            # Includes repeat fetch, relative/absolute deepening and unshallow,
            # strict fsck, exact shallow files, bodies and checkout comparisons.
            compare_clone(root, bare, urls, env, version, branch, depth)
        for version, filter_spec in product((0, 1, 2), ("blob:none", "blob:limit=5", "blob:limit=1k")):
            compare_clone(root, bare, urls, env, version, "main", None, filter_spec)
            compare_clone(root, bare, urls, env, version, "merge", 1, filter_spec)
    compare_revisions(root, url)
    print("Worker absolute/relative/filter/time/exclusion oracle passed", flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--zero-url", required=True)
    args = parser.parse_args()
    compare(args.root, args.zero_url)
