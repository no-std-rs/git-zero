import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("llm_cc_ratio", Path(__file__).resolve().parents[1] / "scripts/llm-cc-ratio.py")
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)


class RatioHandoffTests(unittest.TestCase):
    def artifacts(self, directory, base=(10, 100), head=(10, 100), status="complete", comment=b"Full comparison\n"):
        identity = {"repository": "no-std-rs/git-zero", "head_sha": "a" * 40,
                    "target_sha": "b" * 40, "base_sha": "b" * 40, "pr_number": 4,
                    "target_branch": "release@`v1", "pipeline_id": "example", "started_at": "example"}
        report = {"schema_version": 2, "identity": identity, "fingerprint": "c" * 64,
                  "status": status, "errors": [], "sides": {
                      "base": {"totals": {"llm_cc": base[0], "token_count": base[1]}},
                      "head": {"totals": {"llm_cc": head[0], "token_count": head[1]}}}}
        publication = {"schema_version": 2, "identity": identity, "fingerprint": report["fingerprint"],
                       "status": status, "comment": {"path": "comment.md", "bytes": len(comment),
                       "sha256": hashlib.sha256(comment).hexdigest()}}
        for name, value in [("report.json", report), ("publication.json", publication)]:
            (directory / name).write_text(json.dumps(value))
        (directory / "comment.md").write_bytes(comment)

    def payload(self, directory):
        comment = (directory / "comment.md").read_bytes()
        envelope = json.loads((directory / "publication.json").read_text())
        self.assertEqual(envelope["comment"]["bytes"], len(comment))
        self.assertEqual(envelope["comment"]["sha256"], hashlib.sha256(comment).hexdigest())
        self.assertNotIn("@", comment.decode())
        return json.loads(comment.decode().splitlines()[0][len(gate.PREFIX):-1])

    def test_exact_ratios_preserve_equal_improved_and_regressed_measurements(self):
        for head in [(10, 100), (5, 100), (10.0000000001, 100)]:
            with self.subTest(head=head), tempfile.TemporaryDirectory() as name:
                directory = Path(name)
                self.artifacts(directory, head=head)
                gate.annotate(directory)
                payload = self.payload(directory)
                self.assertEqual(payload["base_ratio"], 0.1)
                self.assertEqual(payload["head_ratio"], head[0] / head[1])
                self.assertEqual(payload["identity"]["target_branch"], "release@`v1")
                self.assertTrue((directory / "comment.md").read_bytes().endswith(b"Full comparison\n"))
                before = (directory / "comment.md").read_bytes()
                gate.annotate(directory)
                self.assertEqual((directory / "comment.md").read_bytes(), before)

    def test_missing_zero_and_incomplete_measurements_stay_unavailable(self):
        for complexity, tokens, status in [(None, None, "complete"), (0, 0, "complete"), (10, 100, "incomplete")]:
            with self.subTest(status=status, tokens=tokens), tempfile.TemporaryDirectory() as name:
                directory = Path(name)
                self.artifacts(directory, head=(complexity, tokens), status=status)
                gate.annotate(directory)
                self.assertIsNone(self.payload(directory)["head_ratio"])

    def test_invalid_metrics_and_mismatched_envelopes_fail(self):
        for complexity, tokens in [(float("nan"), 100), (-1, 100), (1, -1), (1, True)]:
            with self.subTest(complexity=complexity, tokens=tokens), tempfile.TemporaryDirectory() as name:
                directory = Path(name)
                self.artifacts(directory, head=(complexity, tokens))
                with self.assertRaises(ValueError):
                    gate.annotate(directory)
        with tempfile.TemporaryDirectory() as name:
            directory = Path(name)
            self.artifacts(directory)
            (directory / "comment.md").write_bytes(b"tampered")
            with self.assertRaisesRegex(ValueError, "envelope"):
                gate.annotate(directory)

    def test_maximum_comment_stays_bounded_and_skipped_runs_need_no_report(self):
        with tempfile.TemporaryDirectory() as name:
            directory = Path(name)
            self.artifacts(directory, comment=b"x" * gate.MAX_COMMENT_BYTES)
            gate.annotate(directory)
            self.payload(directory)
            self.assertLessEqual((directory / "comment.md").stat().st_size, gate.MAX_COMMENT_BYTES)
        with tempfile.TemporaryDirectory() as name:
            directory = Path(name)
            (directory / "skipped.json").write_text("{}")
            gate.annotate(directory)

    def test_isolated_producer_ignores_pr_imports(self):
        with tempfile.TemporaryDirectory() as name:
            directory = Path(name)
            self.artifacts(directory)
            # A proposed checkout must not inject code through standard imports.
            producer = directory / "producer.py"
            producer.write_bytes(Path(spec.origin).read_bytes())
            (directory / "json.py").write_text("raise RuntimeError('untrusted import')\n")
            unsafe = subprocess.run([sys.executable, str(producer), str(directory)],
                                    cwd=directory, capture_output=True, text=True)
            self.assertNotEqual(unsafe.returncode, 0)
            self.assertIn("untrusted import", unsafe.stderr)
            result = subprocess.run([sys.executable, "-I", str(producer), str(directory)],
                                    cwd=directory, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(self.payload(directory)["head_ratio"], 0.1)


if __name__ == "__main__":
    unittest.main()
