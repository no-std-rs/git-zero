import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { codecSourceDigest } from "../scripts/codec-source.mjs";
test("documented codec pin matches the public build source rather than a PR-only commit", async () => {
  const root = fileURLToPath(new URL("../../../../", import.meta.url));
  const pin = JSON.parse(await readFile(new URL("../codec-source-pin.json", import.meta.url), "utf8"));
  assert.equal(await codecSourceDigest(root, pin.inputs), pin.sourceSha256, "update the immutable source pin when the codec build inputs change");
  const contract = await readFile(new URL("../../../../docs/repository-host-v1.md", import.meta.url), "utf8");
  assert.ok(contract.includes(pin.commit)); assert.ok(contract.includes("codec-source-pin.json"));
});
