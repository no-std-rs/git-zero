#!/usr/bin/env python3
"""Private stdio launcher for the existing independent Git HTTP reference."""
import argparse
import json
from pathlib import Path
import sys

from interop import environment, reference_server


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    parser.add_argument("--git", default="git")
    parser.add_argument("--trace2", type=Path)
    args = parser.parse_args()
    env = environment(args.git)
    if args.trace2 is not None:
        env["GIT_TRACE2_EVENT"] = str(args.trace2.resolve())
    with reference_server(args.root, env, True) as origin:
        print(json.dumps({"origin": origin}), flush=True)
        sys.stdin.read()


if __name__ == "__main__":
    main()
