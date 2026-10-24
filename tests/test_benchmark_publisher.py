"""Trust-boundary tests for the default-branch Runnerless publisher."""
import importlib.util
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import zipfile

spec = importlib.util.spec_from_file_location("benchmark_publisher", Path(__file__).parents[1] / "scripts/publish-benchmarks.py")
publisher = importlib.util.module_from_spec(spec)
spec.loader.exec_module(publisher)


def archive(name="benchmark.json", body=b'{"source_revision":"a"}'):
    output = io.BytesIO()
    with zipfile.ZipFile(output, "w") as bundle:
        bundle.writestr(name, body)
    return output.getvalue()


class ArtifactTests(unittest.TestCase):
    def test_retains_original_observations(self):
        raw = {"results": [{"samples": {"git": [{"wall_s": 1, "extra": [2, 3]}]}}]}
        self.assertEqual(publisher.read_artifact(archive(body=json.dumps(raw).encode())), raw)

    def test_no_extraction_paths_duplicates_or_nonfinite_json(self):
        for name in ("../benchmark.json", "/benchmark.json", "script.py", "nested/benchmark.json"):
            with self.subTest(name=name), self.assertRaises(ValueError):
                publisher.read_artifact(archive(name=name))
        for raw in (b'{"x":1,"x":2}', b'{"x":NaN}', b'{"x":Infinity}'):
            with self.subTest(raw=raw), self.assertRaises(ValueError):
                publisher.read_artifact(archive(body=raw))
        with self.assertRaises(ValueError):
            publisher.read_artifact(archive(body=b" " * (publisher.MAX_SUBMISSION + 1)))


class CommentTests(unittest.TestCase):
    def test_stale_head_does_not_replace_current_report(self):
        with patch.object(publisher, "github", return_value={"state": "open", "head": {"sha": "b"}}) as github:
            publisher.publish_comment(1, publisher.MARKER + "\nreport", "a", 100, 1)
            self.assertEqual(github.call_count, 1)

    def test_only_own_comment_for_the_exact_attempt_is_updated(self):
        own = {"id": 7, "user": {"login": "github-actions[bot]"},
               "body": "<!-- runnerless:git-zero-benchmarks:200-2 -->\nreport"}
        forged = {"id": 6, "user": {"login": "contributor"}, "body": own["body"]}
        with patch.object(publisher, "github", side_effect=[{"state": "open", "head": {"sha": "a"}}, [forged, own], {}]) as github:
            publisher.publish_comment(1, publisher.MARKER + "\nreport", "a", 200, 2)
            self.assertEqual(github.call_args.args, ("issues/comments/7",))
            self.assertEqual(github.call_args.kwargs["method"], "PATCH")
            self.assertIn("<!-- runnerless:git-zero-benchmarks:200-2 -->", github.call_args.kwargs["data"]["body"])
        with patch.object(publisher, "github", side_effect=[{"state": "open", "head": {"sha": "a"}}, [own], {}]) as github:
            publisher.publish_comment(1, publisher.MARKER + "\nnew run", "a", 201, 1)
            self.assertEqual(github.call_args.args, ("issues/1/comments",))


class PublicationTests(unittest.TestCase):
    def test_fork_and_superseded_head_runs_keep_their_pr_identity(self):
        run = {"pull_requests": [], "head_sha": "a" * 40, "head_branch": "feature",
               "head_repository": {"full_name": "contributor/git-zero"}}
        pull = {"number": 7, "head": {"sha": "b" * 40, "ref": "feature", "repo": {"full_name": "contributor/git-zero"}},
                "base": {"repo": {"full_name": "no-std-rs/git-zero"}}}
        with patch.object(publisher, "github", return_value=[pull]):
            self.assertEqual(publisher.resolve_pull(run, "no-std-rs/git-zero"), 7)
        with patch.object(publisher, "github", return_value=[pull, pull]), self.assertRaises(ValueError):
            publisher.resolve_pull(run, "no-std-rs/git-zero")

    def test_manual_main_attempt_is_submitted_with_all_measurements(self):
        run = {"id": 100, "run_attempt": 2, "head_sha": "a" * 40, "head_branch": "main",
               "repository": {"full_name": "no-std-rs/git-zero"}, "head_repository": {"full_name": "no-std-rs/git-zero"},
               "path": ".github/workflows/benchmarks.yml", "status": "completed", "conclusion": "success",
               "event": "workflow_dispatch", "pull_requests": []}
        report = {"source_revision": run["head_sha"], "results": [{"all_raw_values": [1, 2, 3]}]}
        with tempfile.TemporaryDirectory() as directory:
            event_path = Path(directory) / "event.json"
            event_path.write_text(json.dumps({"workflow_run": run}))
            env = {"GITHUB_EVENT_NAME": "workflow_run", "GITHUB_EVENT_PATH": str(event_path),
                   "GITHUB_REPOSITORY": "no-std-rs/git-zero", "GITHUB_STEP_SUMMARY": str(Path(directory) / "summary")}
            with patch.dict(os.environ, env), patch.object(publisher, "github", side_effect=[run, {"artifacts": [{
                    "id": 1, "name": "runnerless-benchmarks-100-2", "expired": False, "size_in_bytes": 200}]}]) as github, \
                    patch.object(publisher, "download", return_value=report), \
                    patch.object(publisher, "submit", return_value={"comment": "Runnerless report"}) as submit:
                publisher.main()
                self.assertEqual(github.call_args_list[0].args, ("actions/runs/100/attempts/2",))
                self.assertEqual(submit.call_args.args[1]["report"], report)
                self.assertEqual(submit.call_args.args[1]["source"]["run_attempt"], 2)
                self.assertIsNone(submit.call_args.args[1]["source"]["pull_number"])


if __name__ == "__main__":
    unittest.main()
