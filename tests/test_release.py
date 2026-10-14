"""Exercise release version staging and immutable asset retry behavior."""

import hashlib
import importlib.util
import json
from pathlib import Path
import struct
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


class CargoUploads(unittest.TestCase):
    def upload(self, name="git-zero-client", version="0.1.0"):
        metadata = json.dumps({
            "name": name, "vers": version,
            "features": {"client": ["dep:transport", "transport?/tls"]},
            "deps": [{"name": "git-zero-core", "version_req": "^0.1.0", "features": ["read"],
                      "optional": False, "default_features": False, "target": None,
                      "kind": "normal", "registry": None}],
        }).encode()
        archive = b"bounded archive fixture"
        return struct.pack("<I", len(metadata)) + metadata + struct.pack("<I", len(archive)) + archive

    def test_preserves_dependencies_and_namespaced_features_in_sparse_index(self):
        record = json.loads(capture.parse_upload(self.upload(), "git-zero-client", "0.1.0"))
        self.assertEqual(record["v"], 2)
        self.assertEqual(record["features2"]["client"], ["dep:transport", "transport?/tls"])
        self.assertEqual(record["deps"][0]["features"], ["read"])
        self.assertFalse(record["deps"][0]["default_features"])
        self.assertEqual(record["cksum"], hashlib.sha256(b"bounded archive fixture").hexdigest())

    def test_rejects_bad_lengths_wrong_identity_and_trailing_data(self):
        for body in (b"", self.upload()[:-1], self.upload() + b"extra",
                     self.upload(name="wrong"), self.upload(version="0.2.0"), struct.pack("<I", 0xffffffff) + b"data"):
            with self.assertRaises(ValueError):
                capture.parse_upload(body, "git-zero-client", "0.1.0")


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


if __name__ == "__main__":
    unittest.main()
