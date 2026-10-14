"""Exercise release version staging and immutable asset retry behavior."""

from copy import deepcopy
from contextlib import ExitStack, closing
import hashlib
import http.client
import importlib.util
import json
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import tomllib
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
from release_packages import PACKAGES


def load(name):
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), ROOT / "scripts" / (name + ".py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


prepare = load("prepare-release")
capture = load("package-registry-uploads")
attach = load("attach-release-assets")
public = load("check-public-packages")


class PrepareRelease(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        for relative in [*PACKAGES.values(), "Cargo.lock"]:
            destination = self.root / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes((ROOT / relative).read_bytes())
        (self.root / "version.txt").write_text("1.2.3\n")
        locked = tomllib.loads((self.root / "Cargo.lock").read_text())
        self.worker_version = next(item["version"] for item in locked["package"] if item["name"] == "git-zero-worker-wasm")
        self.root_patch = patch.object(prepare, "ROOT", self.root)
        self.root_patch.start()
        self.addCleanup(self.root_patch.stop)

    def files(self):
        return {str(path.relative_to(self.root)): path.read_bytes()
                for path in self.root.rglob("*") if path.is_file()}

    def test_updates_packages_dependencies_and_lock_without_touching_external_dependencies(self):
        prepare.prepare("v1.2.3")
        for relative in PACKAGES.values():
            manifest = tomllib.loads((self.root / relative).read_text())
            self.assertEqual(manifest["package"]["version"], "1.2.3")
            for name, dependency in manifest.get("dependencies", {}).items():
                if name in PACKAGES:
                    self.assertEqual(dependency["version"], "1.2.3")
        core = tomllib.loads((self.root / PACKAGES["git-zero-core"]).read_text())
        self.assertEqual(core["dependencies"]["sha1"]["version"], "0.10.7")
        lock = tomllib.loads((self.root / "Cargo.lock").read_text())
        self.assertTrue(all(item["version"] == "1.2.3" for item in lock["package"] if item["name"] in PACKAGES))
        self.assertEqual(next(item for item in lock["package"] if item["name"] == "git-zero-worker-wasm")["version"], self.worker_version)
        once = self.files()
        prepare.prepare("v1.2.3")
        self.assertEqual(self.files(), once)

    def test_bad_tag_or_version_mismatch_leaves_all_files_untouched(self):
        before = self.files()
        for tag in ("v1.2.3-rc.1", "v01.2.3", "1.2.3", "v1.2.4"):
            with self.assertRaises(ValueError):
                prepare.prepare(tag)
            self.assertEqual(self.files(), before)

    def test_missing_lock_entry_leaves_manifests_untouched(self):
        lock = self.root / "Cargo.lock"
        lock.write_text(lock.read_text().replace('name = "git-zero-client"', 'name = "missing-client"'))
        before = self.files()
        with self.assertRaises(ValueError):
            prepare.prepare("v1.2.3")
        self.assertEqual(self.files(), before)


def cargo_upload(name="git-zero-client", version="0.1.0"):
    metadata = json.dumps({
        "name": name, "vers": version,
        "features": {"client": ["dep:transport", "transport?/tls"]},
        "deps": [{"name": "git-zero-core", "version_req": "^0.1.0", "features": ["read"],
                  "optional": False, "default_features": False, "target": None,
                  "kind": "normal", "registry": None}],
    }).encode()
    archive = b"bounded archive fixture"
    return struct.pack("<I", len(metadata)) + metadata + struct.pack("<I", len(archive)) + archive


class CargoUploads(unittest.TestCase):

    def test_preserves_dependencies_and_namespaced_features_in_sparse_index(self):
        record = json.loads(capture.parse_upload(cargo_upload(), "git-zero-client", "0.1.0"))
        self.assertEqual(record["v"], 2)
        self.assertEqual(record["features2"]["client"], ["dep:transport", "transport?/tls"])
        self.assertEqual(record["deps"][0]["features"], ["read"])
        self.assertFalse(record["deps"][0]["default_features"])
        self.assertEqual(record["cksum"], hashlib.sha256(b"bounded archive fixture").hexdigest())

    def test_rejects_bad_lengths_wrong_identity_and_trailing_data(self):
        for body in (b"", cargo_upload()[:-1], cargo_upload() + b"extra",
                     cargo_upload(name="wrong"), cargo_upload(version="0.2.0"), struct.pack("<I", 0xffffffff) + b"data"):
            with self.assertRaises(ValueError):
                capture.parse_upload(body, "git-zero-client", "0.1.0")


class RegistryHTTP(unittest.TestCase):
    def setUp(self):
        self.stack = ExitStack()
        self.addCleanup(self.stack.close)
        self.root = Path(self.stack.enter_context(tempfile.TemporaryDirectory()))
        self.server = self.stack.enter_context(capture.registry(self.root, "0.1.0"))
        self.server.expected = "git-zero-client"
        self.headers = {"Authorization": self.server.token}

    def request(self, method, path, body=None, headers=None):
        with closing(http.client.HTTPConnection("127.0.0.1", self.server.server_port, timeout=3)) as connection:
            connection.request(method, path, body, self.headers if headers is None else headers)
            response = connection.getresponse()
            return response.status, response.read()

    def test_publish_serves_exact_sibling_archive_and_refuses_overwrite(self):
        body = cargo_upload()
        self.assertEqual(self.request("PUT", "/api/v1/crates/new", body)[0], 200)
        status, index = self.request("GET", "/index/gi/t-/git-zero-client")
        self.assertEqual(status, 200)
        record = json.loads(index)
        self.assertEqual(record["features2"]["client"], ["dep:transport", "transport?/tls"])
        self.assertEqual(record["deps"][0]["name"], "git-zero-core")
        self.assertEqual(self.request("GET", "/crates/git-zero-client/0.1.0/download"),
                         (200, b"bounded archive fixture"))
        self.assertEqual(self.request("PUT", "/api/v1/crates/new", body)[0], 422)
        self.assertEqual((self.root / "git-zero-client-0.1.0.cargo-upload").read_bytes(), body)

    def test_missing_or_wrong_authorization_cannot_capture(self):
        for headers in ({}, {"Authorization": "incorrect-local-fixture-token"}):
            self.assertEqual(self.request("PUT", "/api/v1/crates/new", cargo_upload(), headers)[0], 403)
        self.assertEqual(list(self.root.iterdir()), [])
        self.assertEqual(self.server.index, {})
        self.assertIsNone(self.server.error)

    def test_invalid_lengths_and_wrong_identity_leave_no_capture(self):
        for length in ("0", "-1", "invalid", str(capture.MAX_UPLOAD + 1)):
            self.assertEqual(self.request("PUT", "/api/v1/crates/new", b"",
                                         {**self.headers, "Content-Length": length})[0], 422)
        self.assertEqual(self.request("PUT", "/api/v1/crates/new", cargo_upload(name="wrong"))[0], 422)
        self.assertEqual(list(self.root.iterdir()), [])
        self.assertEqual(self.server.index, {})

    def test_configuration_and_public_dependency_routes_use_correct_origins(self):
        status, data = self.request("GET", "/index/config.json")
        self.assertEqual(status, 200)
        origin = f"http://127.0.0.1:{self.server.server_port}"
        self.assertEqual(json.loads(data), {"dl": origin + "/crates", "api": origin})
        with patch.object(capture, "fetch", return_value=b"public index") as fetch:
            self.assertEqual(self.request("GET", "/index/sh/a1/sha1"), (200, b"public index"))
            fetch.assert_called_once_with("https://index.crates.io/sh/a1/sha1", capture.MAX_INDEX)
        with patch.object(capture, "fetch", return_value=b"public archive") as fetch:
            self.assertEqual(self.request("GET", "/crates/sha1/0.10.7/download"), (200, b"public archive"))
            fetch.assert_called_once_with("https://static.crates.io/crates/sha1/sha1-0.10.7.crate", capture.MAX_UPLOAD, 503)

    def test_upstream_errors_and_invalid_routes_are_http_failures(self):
        with patch.object(capture, "fetch", side_effect=capture.RegistryError(503)):
            self.assertEqual(self.request("GET", "/index/sh/a1/sha1")[0], 503)
        with patch.object(capture, "fetch") as fetch:
            self.assertEqual(self.request("GET", "/unknown")[0], 404)
            self.assertEqual(self.request("GET", "/crates/sha1/0.10.7/invalid")[0], 404)
            self.assertEqual(self.request("PUT", "/unknown", cargo_upload())[0], 404)
            fetch.assert_not_called()
        self.assertIsNone(self.server.error)


class ReleaseAssets(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.files = [f"{name}-0.1.0.cargo-upload" for name in PACKAGES] + ["SHA256SUMS"]
        for name in self.files:
            (self.root / name).write_bytes(name.encode())
        self.release = {"tag_name": "v0.1.0", "draft": False, "prerelease": False, "assets": []}
        self.calls = []

    def gh(self, arguments):
        self.calls.append(arguments)
        if arguments[0] == "api":
            return json.dumps(self.release).encode()
        return b""

    def asset(self, name):
        body = (self.root / name).read_bytes()
        return {"id": 1, "name": name, "size": len(body), "digest": "sha256:" + hashlib.sha256(body).hexdigest()}

    def test_retry_attaches_only_missing_files(self):
        self.release["assets"] = [self.asset(self.files[0])]
        with patch.object(attach, "gh", self.gh):
            attach.attach("no-std-rs/git-zero", "v0.1.0", self.root)
        upload = self.calls[-1]
        self.assertEqual(upload[:3], ["release", "upload", "v0.1.0"])
        self.assertNotIn(str(self.root / self.files[0]), upload)
        self.assertTrue(all(str(self.root / name) in upload for name in self.files[1:]))

    def test_mismatched_existing_asset_prevents_all_uploads(self):
        asset = self.asset(self.files[-1])
        asset["digest"] = "sha256:" + "0" * 64
        self.release["assets"] = [asset]
        with patch.object(attach, "gh", self.gh), self.assertRaises(ValueError):
            attach.attach("no-std-rs/git-zero", "v0.1.0", self.root)
        self.assertEqual(len(self.calls), 1)

    def test_complete_retry_is_read_only(self):
        self.release["assets"] = [self.asset(name) for name in self.files]
        with patch.object(attach, "gh", self.gh):
            attach.attach("no-std-rs/git-zero", "v0.1.0", self.root)
        self.assertEqual(len(self.calls), 1)


class PublicPackages(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.metadata = json.loads(subprocess.check_output(
            ["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"], cwd=ROOT
        ))
        cls.packages = {item["name"]: item for item in cls.metadata["packages"]}
        cls.core = cls.packages["git-zero-core"]

    def test_real_core_metadata_and_external_dependency_are_valid(self):
        public.check_metadata(self.core, PACKAGES["git-zero-core"])
        public.check_dependencies(self.core, set(), self.core["version"])

    def test_missing_release_metadata_is_rejected(self):
        for field in ("description", "license", "repository", "readme"):
            with self.subTest(field=field):
                package = deepcopy(self.core)
                package[field] = None
                with self.assertRaisesRegex(ValueError, field):
                    public.check_metadata(package, PACKAGES["git-zero-core"])

    def test_dependency_order_and_version_mismatches_are_rejected(self):
        client = self.packages["git-zero-client"]
        with self.assertRaisesRegex(ValueError, "uploaded first"):
            public.check_dependencies(client, set(), client["version"])
        with self.assertRaisesRegex(ValueError, "version differs"):
            public.check_dependencies(client, {"git-zero-core"}, "9.8.7")

    def test_unpublished_local_dependency_is_rejected(self):
        package = deepcopy(self.core)
        package["dependencies"].append({"name": "internal", "path": str(ROOT / "internal")})
        with self.assertRaisesRegex(ValueError, "unpublished local"):
            public.check_dependencies(package, set(), package["version"])

    def test_example_accidentally_marked_public_is_rejected(self):
        metadata = deepcopy(self.metadata)
        next(item for item in metadata["packages"] if item["name"] == "git-zero-worker-wasm")["publish"] = None
        with patch.object(public.subprocess, "check_output", return_value=json.dumps(metadata).encode()):
            with self.assertRaisesRegex(ValueError, "release package list"):
                public.check()

    def test_mixed_public_versions_are_rejected(self):
        metadata = deepcopy(self.metadata)
        next(item for item in metadata["packages"] if item["name"] == "git-zero-client")["version"] = "9.8.7"
        with patch.object(public.subprocess, "check_output", return_value=json.dumps(metadata).encode()):
            with self.assertRaisesRegex(ValueError, "one version"):
                public.check()


if __name__ == "__main__":
    unittest.main()
