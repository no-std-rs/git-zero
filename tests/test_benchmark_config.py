import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import zlib

from benchmark_config import REFERENCE_GIT_VERSION, configuration, version_pin
from benchmark_fixture import create
from benchmark_objects import prepare_pack
from interop import environment
from upstream_http import git


class BenchmarkConfigTests(unittest.TestCase):
    def test_pin_requires_exact_version_and_records_explicit_opt_out(self):
        expected = f"git version {REFERENCE_GIT_VERSION}"
        self.assertTrue(version_pin(expected)["matches"])
        for actual in ("git version 0.0.0", expected + ".custom", "unexpected output"):
            with self.subTest(actual=actual), self.assertRaisesRegex(ValueError, "requires"):
                version_pin(actual)
            pin = version_pin(actual, allow_unpinned=True)
            self.assertFalse(pin["matches"])
            self.assertFalse(pin["required"])

    def test_profile_conflicts_and_invalid_tuning_fail_before_preparation(self):
        for values in ({"compression": 6}, {"delta_window": 10}, {"delta_depth": 50}):
            with self.subTest(values=values), self.assertRaisesRegex(ValueError, "cannot be combined"):
                configuration("upstream-defaults", **values)
        for values in ({"compression": -1}, {"compression": 10}, {"delta_window": -1},
                       {"delta_depth": -1}, {"delta_depth": 4096}):
            with self.subTest(values=values), self.assertRaises(ValueError):
                configuration(**values)
        runner = Path(__file__).with_name("benchmark.py")
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "invalid.json"
            result = subprocess.run([sys.executable, str(runner), "--git-profile", "upstream-defaults",
                                     "--git-compression", "0", "--output", str(output)],
                                    capture_output=True, timeout=30)
            self.assertEqual(result.returncode, 2)
            self.assertIn(b"cannot be combined", result.stderr)
            self.assertFalse(output.exists())

    def test_real_encoded_inputs_follow_matched_defaults_and_custom_delta_policies(self):
        env = environment()
        profiles = [configuration(), configuration("upstream-defaults"),
                    configuration(compression=6), configuration(delta_window=10, delta_depth=2)]
        expected = None
        for config in profiles:
            with self.subTest(config=config), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                bare, objects, metadata = create(root, env, 4, 32768, 8, config)
                if expected is None:
                    expected = objects
                self.assertEqual(objects, expected)
                self.assertEqual(metadata["algorithms"], config.algorithms())
                self.assertEqual(json.loads((root / "fixture.json").read_text()), metadata)
                self.assert_git_inputs(bare, config, objects, env)
                pack = prepare_pack(root, env, config)
                if config.delta_window == 0 or config.delta_depth == 0:
                    self.assertEqual(pack["delta_objects"], 0)
                else:
                    self.assertGreater(pack["delta_objects"], 0)
                    self.assertLessEqual(pack["max_delta_depth"], config.delta_depth or 50)
                self.assertEqual(config.algorithms()["equivalent_server_features"], config.profile == "matched")

    def test_sampling_configuration_and_tree_limits_fail_before_starting_servers(self):
        runner = Path(__file__).with_name("benchmark.py")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            policy = root / "limits.json"
            policy.write_text(json.dumps({"schema_version": 1, "limits": [{
                "workload": "server-clone-smart", "implementation": "zero",
                "metric": "server.process_tree_peak_rss_kib", "maximum": 65536}]}))
            cases = [(["--rss-sample-ms", str(value)], b"rss-sample-ms") for value in (-1, 1, 9, 1001)]
            cases.append((["--limits", str(policy)], b"require --rss-sample-ms"))
            for arguments, message in cases:
                with self.subTest(arguments=arguments):
                    output = root / "invalid.json"
                    result = subprocess.run([sys.executable, str(runner), "--output", str(output), *arguments],
                                            capture_output=True, timeout=30)
                    self.assertEqual(result.returncode, 2)
                    self.assertIn(message, result.stderr)
                    self.assertFalse(output.exists())

    def assert_git_inputs(self, bare, config, objects, env):
        """Independently check the loose bytes and the selected Git configuration."""
        actual = git(bare, "config", "--local", "--list", env=env).decode()
        if config.profile == "upstream-defaults":
            for name in ("core.compression", "core.loosecompression", "pack.compression", "pack.window", "pack.depth"):
                self.assertNotIn(name + "=", actual)
        for oid, (kind, body) in objects.items():
            encoded = (bare / "objects" / oid[:2].decode() / oid[2:].decode()).read_bytes()
            self.assertEqual(zlib.decompress(encoded), kind + b" " + str(len(body)).encode() + b"\0" + body)


if __name__ == "__main__":
    unittest.main()
