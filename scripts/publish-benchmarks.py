#!/usr/bin/env python3
"""Trusted workflow_run publisher. Artifact contents are data, never commands."""
import datetime
import io
import json
import os
from pathlib import Path
import re
import time
import urllib.error
import urllib.parse
import urllib.request
import zipfile

ORIGIN = "https://app.runnerlesshq.com"
MARKER = "<!-- runnerless:git-zero-benchmarks -->"
MAX_SUBMISSION = 1024 * 1024


def request(url, token, *, data=None, method=None, maximum=2 * MAX_SUBMISSION):
    headers = {"Authorization": f"Bearer {token}", "User-Agent": "git-zero-benchmarks",
               "Accept": "application/vnd.github+json"}
    if data is not None:
        headers["Content-Type"] = "application/json"
        data = json.dumps(data, separators=(",", ":"), allow_nan=False).encode()
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    # urllib strips Authorization on GitHub's signed artifact redirect only via
    # the dedicated download helper below; ordinary API calls cannot redirect.
    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, *args, **kwargs):
            return None
    with urllib.request.build_opener(NoRedirect).open(req, timeout=45) as response:
        body = response.read(maximum + 1)
        if len(body) > maximum:
            raise ValueError("response exceeds the publication budget")
        return json.loads(body)


def github(path, **kwargs):
    return request(f"https://api.github.com/repos/{os.environ['GITHUB_REPOSITORY']}/{path}",
                   os.environ["GH_TOKEN"], **kwargs)


def oidc():
    url = os.environ["ACTIONS_ID_TOKEN_REQUEST_URL"] + "&" + urllib.parse.urlencode({"audience": ORIGIN})
    return request(url, os.environ["ACTIONS_ID_TOKEN_REQUEST_TOKEN"])["value"]


def submit(path, data):
    # Short-lived credentials stay in memory. Retrying the exact run is idempotent;
    # a changed report under an existing identity is rejected by Runnerless.
    for attempt in range(4):
        try:
            return request(f"{ORIGIN}/api/benchmark-ci/github/{path}", oidc(), data=data)
        except urllib.error.HTTPError as error:
            if error.code not in (409, 429, 500, 502, 503, 504) or attempt == 3:
                raise RuntimeError(f"Runnerless benchmark submission failed (HTTP {error.code})") from None
        except (urllib.error.URLError, TimeoutError):
            if attempt == 3:
                raise RuntimeError("Runnerless benchmark submission could not complete") from None
        time.sleep(2 ** attempt)


def milliseconds(value):
    return int(datetime.datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp() * 1000)


def sync_pull(number):
    # Read live state, rather than a possibly delayed close/reopen payload.
    pull = github(f"pulls/{number}")
    submit("pull", {"number": number, "state": pull["state"], "updated_at": milliseconds(pull["updated_at"]),
                    "closed_at": milliseconds(pull["closed_at"]) if pull["closed_at"] else None})
    return pull


def read_artifact(archive):
    if len(archive) > 2 * MAX_SUBMISSION:
        raise ValueError("benchmark archive exceeds the publication budget")
    with zipfile.ZipFile(io.BytesIO(archive)) as bundle:
        entries = bundle.infolist()
        if len(entries) != 1 or entries[0].filename != "benchmark.json" or entries[0].file_size > MAX_SUBMISSION:
            raise ValueError("expected one bounded benchmark.json; archives are never extracted")
        raw = bundle.read(entries[0])
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError("duplicate benchmark JSON field")
            result[key] = value
        return result
    return json.loads(raw, object_pairs_hook=unique,
                      parse_constant=lambda value: (_ for _ in ()).throw(ValueError("non-finite benchmark JSON")))


def download(artifact_id):
    url = f"https://api.github.com/repos/{os.environ['GITHUB_REPOSITORY']}/actions/artifacts/{artifact_id}/zip"
    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, *args, **kwargs):
            return None
    req = urllib.request.Request(url, headers={"Authorization": f"Bearer {os.environ['GH_TOKEN']}",
                                              "User-Agent": "git-zero-benchmarks"})
    try:
        response = urllib.request.build_opener(NoRedirect).open(req, timeout=45)
    except urllib.error.HTTPError as redirect:
        if redirect.code != 302:
            raise RuntimeError(f"GitHub artifact download failed (HTTP {redirect.code})") from None
        location = redirect.headers["Location"]
        parsed = urllib.parse.urlsplit(location)
        if parsed.scheme != "https" or parsed.username or parsed.password:
            raise ValueError("invalid artifact download location")
        # The signed download receives no GitHub or OIDC credential.
        response = urllib.request.urlopen(location, timeout=45)
    with response:
        raw = response.read(2 * MAX_SUBMISSION + 1)
    return read_artifact(raw)


def publish_comment(number, body, sha, run_id, run_attempt):
    pull = github(f"pulls/{number}")
    if pull["state"] != "open" or pull["head"]["sha"] != sha:
        return  # Retain old-head measurements without replacing the current report.
    # An installation's own comments alone can be updated. A user cannot forge
    # the marker and cause the publisher to overwrite their comment.
    # A distinct comment per immutable producer attempt avoids cross-run races.
    # Publisher jobs deliberately have no concurrency group: GitHub replaces
    # pending group members even with cancel-in-progress: false, losing main data.
    marker = f"<!-- runnerless:git-zero-benchmarks:{run_id}-{run_attempt} -->"
    body = marker + "\n" + body.removeprefix(MARKER).lstrip()
    for page in range(1, 501):
        comments = github(f"issues/{number}/comments?per_page=20&page={page}")
        for comment in comments:
            if comment["user"]["login"] == "github-actions[bot]" and comment["body"].startswith(marker + "\n"):
                github(f"issues/comments/{comment['id']}", data={"body": body}, method="PATCH")
                return
        if len(comments) < 20:
            break
    else:
        raise ValueError("PR comment history exceeds publisher budget")
    github(f"issues/{number}/comments", data={"body": body})


def resolve_pull(run, repository):
    pulls = run["pull_requests"]
    if not pulls:
        # The API's commit association identifies the PR even after its head
        # advances. Requiring head.sha to remain current would lose old runs.
        pulls = [pull for pull in github(f"commits/{run['head_sha']}/pulls?per_page=100")
                 if pull["base"]["repo"]["full_name"] == repository
                 and (pull["head"]["ref"] == run["head_branch"]
                      or pull.get("merge_commit_sha") == run["head_sha"])
                 and (pull["head"].get("repo") is None
                      or pull["head"]["repo"]["full_name"] == run["head_repository"]["full_name"])]
    if len(pulls) != 1:
        raise ValueError("cannot identify exactly one benchmark PR")
    return pulls[0]["number"]


def main():
    event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
    source = event["workflow_run"]
    run = github(f"actions/runs/{source['id']}/attempts/{source['run_attempt']}")
    repository = os.environ["GITHUB_REPOSITORY"]
    if (run["repository"]["full_name"] != repository or run["path"] != ".github/workflows/benchmarks.yml"
            or run["status"] != "completed" or run["event"] not in ("push", "pull_request", "workflow_dispatch")):
        raise ValueError("unrecognized benchmark producer")
    if not re.fullmatch(r"[0-9a-f]{40}", run["head_sha"]):
        raise ValueError("invalid benchmark source commit")
    if run["event"] == "pull_request":
        number = resolve_pull(run, repository)
        pull = sync_pull(number)
    else:
        if run["head_branch"] != "main" or run["head_repository"]["full_name"] != repository:
            raise ValueError("only main can publish default-branch measurements")
        number = None
    name = f"runnerless-benchmarks-{run['id']}-{run['run_attempt']}"
    artifacts = github(f"actions/runs/{run['id']}/artifacts?per_page=100")
    matches = [entry for entry in artifacts["artifacts"] if entry["name"] == name and not entry["expired"]]
    if not matches and (run["conclusion"] != "success" or number is not None and pull["state"] == "closed"):
        # A build failure has no measurements; the closed-event lifecycle job
        # intentionally produces none. Other missing artifacts are errors.
        return
    if len(matches) != 1 or matches[0]["size_in_bytes"] > 2 * MAX_SUBMISSION:
        raise ValueError("expected one bounded benchmark artifact for this attempt")
    report = download(matches[0]["id"])
    if report.get("source_revision") != run["head_sha"]:
        raise ValueError("benchmark report does not match the producing commit")
    payload = {"schema": "runnerless.benchmark.ci/v1", "source": {
        "run_id": run["id"], "run_attempt": run["run_attempt"], "sha": run["head_sha"],
        "branch": run["head_branch"], "event": run["event"], "pull_number": number}, "report": report}
    if len(json.dumps(payload, separators=(",", ":"), allow_nan=False).encode()) > MAX_SUBMISSION:
        raise ValueError("benchmark submission exceeds the Runnerless budget")
    receipt = submit("submit", payload)
    if number is not None:
        publish_comment(number, receipt["comment"], run["head_sha"], run["id"], run["run_attempt"])
    with Path(os.environ["GITHUB_STEP_SUMMARY"]).open("a") as summary:
        summary.write(receipt["comment"] + "\n")


if __name__ == "__main__":
    main()
