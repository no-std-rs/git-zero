#!/usr/bin/env python3
"""Include exact report ratios in Runnerless's verified comparison comment."""

import hashlib
import json
import math
from pathlib import Path
import sys

PREFIX = "LM-CC/token data: `"
MAX_COMMENT_BYTES = 24576


def ratio(totals):
    tokens, complexity = totals.get("token_count"), totals.get("llm_cc")
    if tokens is None or complexity is None:
        return None
    if type(tokens) is not int or tokens < 0:
        raise ValueError("Invalid token count")
    if type(complexity) not in (int, float) or not math.isfinite(complexity) or complexity < 0:
        raise ValueError("Invalid LM-CC measurement")
    if tokens == 0:
        return None
    result = complexity / tokens
    if not math.isfinite(result):
        raise ValueError("Invalid LM-CC/token ratio")
    return result


def annotate(directory):
    directory = Path(directory)
    if not (directory / "report.json").exists() and (directory / "skipped.json").exists():
        return
    report = json.loads((directory / "report.json").read_text())
    publication = json.loads((directory / "publication.json").read_text())
    comment = (directory / "comment.md").read_bytes()
    if report.get("schema_version") != 2 or publication.get("schema_version") != 2:
        raise ValueError("Expected llm-cc comparison schema 2")
    if report.get("identity") != publication.get("identity") or report.get("fingerprint") != publication.get("fingerprint"):
        raise ValueError("Report and publication identities differ")
    if publication.get("status") != report.get("status"):
        raise ValueError("Report and publication statuses differ")
    expected = publication["comment"]
    if expected["path"] != "comment.md" or expected["bytes"] != len(comment) or expected["sha256"] != hashlib.sha256(comment).hexdigest():
        raise ValueError("Comment does not match its publication envelope")
    data = {"schema_version": 1, "identity": report["identity"], "status": report["status"],
            "base_ratio": None, "head_ratio": None}
    if report["status"] == "complete" and not report.get("errors"):
        for side in ("base", "head"):
            data[side + "_ratio"] = ratio(report["sides"][side]["totals"])
    encoded = json.dumps(data, sort_keys=True, separators=(",", ":"), allow_nan=False)
    # The publication validator forbids mentions and Markdown link delimiters.
    # Unicode escapes retain JSON meaning for branch names containing these bytes.
    for char in "@[]<>`":
        encoded = encoded.replace(char, "\\u%04x" % ord(char))
    prefix = (PREFIX + encoded + "`\n\n").encode()
    if comment.startswith(PREFIX.encode()):
        comment = comment.split(b"\n\n", 1)[1]
    body = prefix + comment
    if len(body) > MAX_COMMENT_BYTES:
        body = prefix + b"Full comparison details are available in the linked report files.\n"
    if len(body) > MAX_COMMENT_BYTES:
        raise ValueError("Ratio identity exceeds the comment limit")
    publication["comment"] = {"path": "comment.md", "bytes": len(body),
                              "sha256": hashlib.sha256(body).hexdigest()}
    (directory / "comment.md").write_bytes(body)
    (directory / "publication.json").write_text(json.dumps(publication, sort_keys=True, separators=(",", ":")) + "\n")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("Usage: llm-cc-ratio.py COMPARISON_ARTIFACT_DIRECTORY")
    annotate(sys.argv[1])
