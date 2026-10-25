"""Trust-boundary tests for the default-branch Runnerless publisher."""
import importlib.util
from contextlib import contextmanager
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


def producer_run(event="pull_request"):
    return {"id": 100, "run_attempt": 2, "head_sha": "a" * 40,
            "head_branch": "feature" if event == "pull_request" else "main",
            "repository": {"full_name": "no-std-rs/git-zero"},
            "head_repository": {"full_name": "no-std-rs/git-zero"},
            "path": ".github/workflows/benchmarks.yml", "status": "completed", "conclusion": "success",
            "event": event, "pull_requests": [{"number": 7}] if event == "pull_request" else []}


@contextmanager
def publisher_execution(run, responses):
    with tempfile.TemporaryDirectory() as directory:
        event_path = Path(directory) / "event.json"
        event_path.write_text(json.dumps({"workflow_run": run}))
        env = {"GITHUB_EVENT_NAME": "workflow_run", "GITHUB_EVENT_PATH": str(event_path),
               "GITHUB_REPOSITORY": "no-std-rs/git-zero", "GITHUB_STEP_SUMMARY": str(Path(directory) / "summary")}
        with patch.dict(os.environ, env), patch.object(publisher, "github", side_effect=[run, *responses]) as github, \
                patch.object(publisher, "submit", return_value={}) as submit:
            yield github, submit


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

    def test_recovers_an_exact_attempt_artifact_beyond_the_first_page(self):
        first = [{"id": number, "name": f"other-{number}", "expired": False} for number in range(100)]
        expected = {"id": 101, "name": "runnerless-benchmarks-42-1", "expired": False, "size_in_bytes": 200}
        with patch.object(publisher, "github", side_effect=[
                {"total_count": 101, "artifacts": first},
                {"total_count": 101, "artifacts": [expected]}]) as github:
            self.assertEqual(publisher.find_benchmark_artifacts(42, 1), [expected])
            self.assertEqual([call.args[0] for call in github.call_args_list], [
                "actions/runs/42/artifacts?per_page=100&page=1",
                "actions/runs/42/artifacts?per_page=100&page=2"])

    def test_artifact_history_preserves_expiry_and_distinct_duplicate_checks(self):
        expected = {"id": 101, "name": "runnerless-benchmarks-42-1", "expired": False}
        duplicate = {**expected, "id": 102}
        expired = {**expected, "id": 103, "expired": True}
        first = [expected, expired] + [
            {"id": number, "name": "unrelated", "expired": False} for number in range(98)]
        with patch.object(publisher, "github", side_effect=[
                {"total_count": 102, "artifacts": first},
                {"total_count": 102, "artifacts": [expected, duplicate]}]):
            self.assertEqual(publisher.find_benchmark_artifacts(42, 1), [expected, duplicate])

    def test_artifact_history_fails_explicitly_if_pagination_exhausts_its_budget(self):
        entries = [{"id": number, "name": "unrelated", "expired": False} for number in range(100)]
        with patch.object(publisher, "MAX_ARTIFACT_PAGES", 2), \
                patch.object(publisher, "github", return_value={"total_count": 300, "artifacts": entries}) as github, \
                self.assertRaisesRegex(ValueError, "artifact history exceeds publisher budget"):
            publisher.find_benchmark_artifacts(42, 1)
        self.assertEqual(github.call_count, 2)


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
    def test_lifecycle_run_syncs_retention_even_if_the_pr_has_reopened(self):
        jobs = [{"name": "lifecycle", "conclusion": "success"}, {"name": "benchmark", "conclusion": "skipped"}]
        for state in ("open", "closed"):
            with self.subTest(state=state):
                pull = {"state": state, "updated_at": "2026-10-25T00:00:01Z",
                        "closed_at": "2026-10-25T00:00:00Z" if state == "closed" else None}
                with publisher_execution(producer_run(), [pull, {"artifacts": []}, {"total_count": 2, "jobs": jobs}]) as (github, submit):
                    publisher.main()
                    self.assertEqual(github.call_args.args, ("actions/runs/100/attempts/2/jobs?per_page=100",))
                    self.assertEqual(submit.call_count, 1)
                    self.assertEqual(submit.call_args.args[0], "pull")
                    self.assertEqual(submit.call_args.args[1]["state"], state)

    def test_successful_benchmark_missing_its_report_fails_even_after_pr_close(self):
        jobs = [{"name": "lifecycle", "conclusion": "skipped"}, {"name": "benchmark", "conclusion": "success"}]
        for state in ("open", "closed"):
            with self.subTest(state=state):
                pull = {"state": state, "updated_at": "2026-10-25T00:00:01Z",
                        "closed_at": "2026-10-25T00:00:00Z" if state == "closed" else None}
                with publisher_execution(producer_run(), [pull, {"artifacts": []}, {"total_count": 2, "jobs": jobs}]) as (_, submit), \
                        self.assertRaisesRegex(ValueError, "expected one bounded benchmark artifact"):
                    publisher.main()
                self.assertEqual(submit.call_count, 1, "only retention may be synchronized without measurements")

    def test_successful_main_benchmark_missing_its_report_fails(self):
        with publisher_execution(producer_run("workflow_dispatch"), [{"artifacts": []}]) as (_, submit), \
                self.assertRaisesRegex(ValueError, "expected one bounded benchmark artifact"):
            publisher.main()
        submit.assert_not_called()

    def test_lifecycle_detection_requires_the_complete_expected_job_inventory(self):
        jobs = [{"name": "lifecycle", "conclusion": "success"}, {"name": "benchmark", "conclusion": "skipped"}]
        with patch.object(publisher, "github", return_value={"total_count": 3, "jobs": jobs}):
            self.assertFalse(publisher.lifecycle_only(100, 2))
        with patch.object(publisher, "github", return_value={"total_count": 3, "jobs": jobs + [{"name": "other", "conclusion": "success"}]}):
            self.assertFalse(publisher.lifecycle_only(100, 2))

    def test_fork_and_superseded_head_runs_keep_their_pr_identity(self):
        run = {"pull_requests": [], "head_sha": "a" * 40, "head_branch": "feature",
               "head_repository": {"full_name": "contributor/git-zero"}}
        pull = {"number": 7, "head": {"sha": "b" * 40, "ref": "feature", "repo": {"full_name": "contributor/git-zero"}},
                "base": {"repo": {"full_name": "no-std-rs/git-zero"}}}
        with patch.object(publisher, "github", return_value=[pull]):
            self.assertEqual(publisher.resolve_pull(run, "no-std-rs/git-zero"), 7)
        with patch.object(publisher, "github", return_value=[pull, pull]), self.assertRaises(ValueError):
            publisher.resolve_pull(run, "no-std-rs/git-zero")

    def test_closed_merged_pr_can_use_the_base_branch_merge_commit(self):
        run = {"pull_requests": [], "head_sha": "c" * 40, "head_branch": "main",
               "head_repository": {"full_name": "no-std-rs/git-zero"}}
        pull = {"number": 7, "merge_commit_sha": "c" * 40,
                "head": {"sha": "b" * 40, "ref": "feature", "repo": {"full_name": "contributor/git-zero"}},
                "base": {"repo": {"full_name": "no-std-rs/git-zero"}}}
        with patch.object(publisher, "github", return_value=[pull]):
            self.assertEqual(publisher.resolve_pull(run, "no-std-rs/git-zero"), 7)

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
