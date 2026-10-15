import copy
import json
from pathlib import Path
import tempfile
import unittest

from benchmark_limits import evaluate, load


WORKLOAD = "server-clone-smart"


def policy(metric="server.frontend_peak_rss_kib", maximum=64):
    return {"schema_version": 1, "limits": [{"workload": WORKLOAD, "implementation": "zero",
                                           "metric": metric, "maximum": maximum}]}


def results():
    return [{"workload": WORKLOAD, "samples": {
        "git": [{"max_rss_kib": 1024, "server": {"frontend_peak_rss_kib": 1024}}],
        "zero": [
            {"wall_s": 0.2, "cpu_s": 0.1, "max_rss_kib": 32,
             "server": {"wall_s": 0.1, "cpu_s": 0.05, "frontend_peak_rss_kib": 32,
                        "backend_child_peak_rss_kib": 0}},
            {"wall_s": 0.4, "cpu_s": 0.2, "max_rss_kib": 64,
             "server": {"wall_s": 0.2, "cpu_s": 0.1, "frontend_peak_rss_kib": 64,
                        "backend_child_peak_rss_kib": 0}},
        ]}, "server_startup": {"zero": {"startup_wall_s": 1, "startup_cpu_s": 0.5,
                                         "frontend_peak_rss_kib": 128}}}]


class BenchmarkLimitsTests(unittest.TestCase):
    def load_policy(self, value, workloads=(WORKLOAD,)):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "limits.json"
            path.write_text(json.dumps(value))
            return load(path, workloads)

    def test_boundary_is_inclusive_and_each_sample_counts(self):
        for maximum, status in [(64, "passed"), (63, "exceeded"), (65, "passed")]:
            with self.subTest(maximum=maximum):
                entry, = evaluate(self.load_policy(policy(maximum=maximum)), results())
                self.assertEqual(entry["status"], status)
                self.assertEqual(entry["observed_maximum"], 64)
                self.assertEqual(entry["observations"], 2)
        samples = results()
        samples[0]["samples"]["zero"].extend([samples[0]["samples"]["zero"][0]] * 10)
        self.assertEqual(evaluate(policy(maximum=63), samples)[0]["status"], "exceeded")

    def test_scopes_keep_client_frontend_child_and_startup_separate(self):
        maxima = {"client.wall_s": 0.4, "client.cpu_s": 0.2, "client.max_rss_kib": 64,
                  "server.wall_s": 0.2, "server.cpu_s": 0.1, "server.frontend_peak_rss_kib": 64,
                  "server.backend_child_peak_rss_kib": 0, "startup.wall_s": 1,
                  "startup.cpu_s": 0.5, "startup.frontend_peak_rss_kib": 128}
        for metric, maximum in maxima.items():
            with self.subTest(metric=metric):
                entry, = evaluate(self.load_policy(policy(metric, maximum)), results())
                self.assertEqual(entry["status"], "passed")
                self.assertEqual(entry["observed_maximum"], maximum)
                self.assertEqual(entry["observations"], 1 if metric.startswith("startup.") else 2)
        git_policy = policy()
        git_policy["limits"][0]["implementation"] = "git"
        self.assertEqual(evaluate(git_policy, results())[0]["observed_maximum"], 1024)

    def test_failed_workload_and_missing_observations_do_not_pass(self):
        empty_samples = results()
        empty_samples[0]["samples"]["zero"] = []
        missing_metric = results()
        del missing_metric[0]["samples"]["zero"][1]["server"]["frontend_peak_rss_kib"]
        missing_implementation = results()
        del missing_implementation[0]["samples"]["zero"]
        for measured in ([], empty_samples, missing_metric, missing_implementation):
            with self.subTest(measured=measured):
                entry, = evaluate(policy(), measured)
                self.assertEqual(entry["status"], "missing_or_invalid")
                self.assertIsNone(entry["observed_maximum"])
        entry, = evaluate(policy("startup.cpu_s", 1), [{"workload": WORKLOAD}])
        self.assertEqual(entry["status"], "missing_or_invalid")

    def test_sampled_tree_limit_rejects_partial_and_unavailable_observations(self):
        measured = results()
        for sample in measured[0]["samples"]["zero"]:
            sample["server"]["process_tree_rss"] = {"status": "sampled", "peak_rss_kib": 96}
        limit = self.load_policy(policy("server.process_tree_peak_rss_kib", 96))
        self.assertEqual(evaluate(limit, measured)[0]["status"], "passed")
        measured[0]["samples"]["zero"][0]["server"]["process_tree_rss"] = {
            "status": "partial", "peak_rss_kib": None, "observed_peak_rss_kib": 32}
        entry, = evaluate(limit, measured)
        self.assertEqual(entry["status"], "missing_or_invalid")
        self.assertIsNone(entry["observed_maximum"])
        measured[0]["samples"]["zero"][0]["server"]["process_tree_rss"]["peak_rss_kib"] = 32
        self.assertEqual(evaluate(limit, measured)[0]["status"], "missing_or_invalid")
        del measured[0]["samples"]["zero"][0]["server"]["process_tree_rss"]
        self.assertEqual(evaluate(limit, measured)[0]["status"], "missing_or_invalid")

    def test_invalid_observation_cannot_be_hidden_by_valid_samples(self):
        for value in (None, True, -1, "64", float("nan"), float("inf")):
            with self.subTest(value=value):
                measured = results()
                measured[0]["samples"]["zero"][0]["server"]["frontend_peak_rss_kib"] = value
                entry, = evaluate(policy(), measured)
                self.assertEqual(entry["status"], "missing_or_invalid")
                self.assertIsNone(entry["observed_maximum"])

    def test_policy_rejects_typos_unselected_cases_and_duplicate_limits(self):
        invalid = [None, [], {}, {"schema_version": True, "limits": policy()["limits"]},
                   {"schema_version": 2, "limits": policy()["limits"]},
                   {"schema_version": 1, "limits": []},
                   {"schema_version": 1, "limits": {}},
                   {**policy(), "limit": 1},
                   {"schema_version": 1, "limits": [None]},
                   {"schema_version": 1, "limits": [{**policy()["limits"][0], "max": 64}]}]
        for key, value in [("workload", "typo"), ("workload", []), ("implementation", "both"),
                           ("metric", "server.total_rss_kib"), ("maximum", True), ("maximum", -1),
                           ("maximum", None), ("maximum", float("nan")), ("maximum", float("inf"))]:
            entry = policy()
            entry["limits"][0][key] = value
            invalid.append(entry)
        duplicated = policy()
        duplicated["limits"].append(copy.deepcopy(duplicated["limits"][0]))
        invalid.append(duplicated)
        for value in invalid:
            with self.subTest(value=value), self.assertRaises(ValueError):
                self.load_policy(value)
        with self.assertRaisesRegex(ValueError, "unselected"):
            self.load_policy(policy(), workloads=("server-discovery-dumb",))

    def test_no_limits_preserves_opt_in_behavior_and_inputs_are_unchanged(self):
        measured, limits = results(), policy()
        original = copy.deepcopy((measured, limits))
        evaluate(limits, measured)
        self.assertEqual((measured, limits), original)
        self.assertEqual(evaluate({"schema_version": 1, "limits": []}, measured), [])
        self.assertEqual(self.load_policy(policy(maximum=10 ** 400))["limits"][0]["maximum"], 10 ** 400)

    def test_checked_in_ci_limits_are_valid(self):
        limits = load(Path(__file__).with_name("benchmark-limits.json"), (WORKLOAD,))
        self.assertEqual(len(evaluate(limits, results())), 3)
        tree = load(Path(__file__).with_name("benchmark-tree-limits.json"), (WORKLOAD,))
        self.assertEqual(evaluate(tree, results())[0]["status"], "missing_or_invalid")


if __name__ == "__main__":
    unittest.main()
