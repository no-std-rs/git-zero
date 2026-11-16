// Build-source provenance, independent of PR commit history and squash strategy.
import { createHash } from "node:crypto";
import { readFile, readdir } from "node:fs/promises";
import { join } from "node:path";
export async function codecSourceDigest(root, inputs) {
  const paths = [];
  async function visit(path) {
    if (path.endsWith("/")) {
      for (const entry of await readdir(join(root, path), { withFileTypes: true })) {
        if (entry.isDirectory()) await visit(path + entry.name + "/");
        else if (entry.name.endsWith(".rs")) paths.push(path + entry.name);
      }
    } else paths.push(path);
  }
  for (const input of inputs) await visit(input);
  const hash = createHash("sha256");
  for (const path of paths.sort()) { hash.update(path); hash.update("\0"); hash.update(await readFile(join(root, path))); hash.update("\0"); }
  return hash.digest("hex");
}
