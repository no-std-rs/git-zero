import { MAX_SNAPSHOT_BYTES, SnapshotError } from "./snapshots.mjs";
import { JsonProjection } from "./metadata.mjs";

const encoder = new TextEncoder();
const decoder = new TextDecoder("utf-8", { fatal: true });
const groups = ["cf", "headers", "vars", "runtime"];
const MAX_FILES = 56;

function concatenate(values) {
  const result = new Uint8Array(values.reduce((sum, value) => sum + value.length, 0));
  let offset = 0;
  for (const value of values) { result.set(value, offset); offset += value.length; }
  return result;
}

function filename(name) {
  const bytes = encoder.encode(name);
  if (decoder.decode(bytes) !== name) throw new SnapshotError("Metadata names must be valid Unicode", 400);
  let encoded = [...bytes].map((byte) =>
    byte >= 97 && byte <= 122 || byte >= 48 && byte <= 57 || byte === 45 || byte === 95
      ? String.fromCharCode(byte) : `%${byte.toString(16).toUpperCase().padStart(2, "0")}`
  ).join("");
  if (/^(con|prn|aux|nul|com[1-9]|lpt[1-9])$/.test(encoded)) {
    encoded = `%${bytes[0].toString(16).toUpperCase()}${encoded.slice(1)}`;
  }
  encoded += ".json";
  if (encoded.length > 255) throw new SnapshotError("Tree metadata filename exceeds 255 bytes", 413);
  return encoded;
}

class Objects {
  records = new Map();
  bytes = 0;

  async add(kind, body) {
    const header = encoder.encode(`${kind} ${body.length}\0`);
    const digest = new Uint8Array(await crypto.subtle.digest("SHA-1", concatenate([header, body])));
    const id = [...digest].map((byte) => byte.toString(16).padStart(2, "0")).join("");
    if (!this.records.has(id)) {
      this.bytes += body.length;
      if (this.records.size === 64 || this.bytes > 131072) throw new SnapshotError("Tree metadata exceeds repository capacity", 413);
      this.records.set(id, { id, kind, body });
    }
    return { id, digest };
  }

  async tree(entries) {
    entries.sort((left, right) => left.name < right.name ? -1 : left.name > right.name ? 1 : 0);
    return this.add("tree", concatenate(entries.flatMap((entry) => [
      encoder.encode(`${entry.mode} ${entry.name}\0`), entry.digest,
    ])));
  }
}

// Explicit tree snapshots project top-level keys; nested values remain JSON.
// Empty groups are omitted, avoiding a referenced empty tree in loose HTTP.
export async function treeRepository(parts) {
  if (!Array.isArray(parts) || parts.length !== 4 || parts.some((part) => typeof part !== "string")) {
    throw new SnapshotError("Invalid tree metadata", 400);
  }
  if (parts.reduce((sum, part) => sum + part.length, 0) > MAX_SNAPSHOT_BYTES || encoder.encode(JSON.stringify(parts)).length > MAX_SNAPSHOT_BYTES) {
    throw new SnapshotError("Metadata snapshot exceeds 8 KiB", 413);
  }
  const projection = new JsonProjection();
  let count = 0;
  const categories = parts.map((part) => {
    let value;
    try { value = JSON.parse(part); }
    catch { throw new SnapshotError("Invalid tree metadata JSON", 400); }
    if (!value || Array.isArray(value) || typeof value !== "object") throw new SnapshotError("Tree metadata categories must be objects", 400);
    const entries = Object.entries(value);
    count += entries.length;
    if (count > MAX_FILES) throw new SnapshotError("Tree metadata exceeds 56 files", 413);
    return entries.map(([key, value]) => [key, projection.project(value, 400)]);
  });
  const objects = new Objects();
  const root = [], files = [];
  for (const [index, category] of categories.entries()) {
    const entries = [];
    for (const [key, value] of category) {
      const name = filename(key);
      const blob = await objects.add("blob", encoder.encode(JSON.stringify(value, null, 2) + "\n"));
      entries.push({ name, mode: "100644", digest: blob.digest });
      files.push(`${groups[index]}/${name}`);
    }
    if (entries.length) {
      const tree = await objects.tree(entries);
      root.push({ name: groups[index], mode: "40000", digest: tree.digest });
    }
  }
  if (!root.length) throw new SnapshotError("Tree metadata requires a nonempty category", 400);
  const tree = await objects.tree(root);
  const identity = "git-zero <git-zero@example.invalid> 0 +0000";
  const commit = await objects.add("commit", encoder.encode(`tree ${tree.id}\nauthor ${identity}\ncommitter ${identity}\n\nTree metadata snapshot\n`));
  return {
    repository: { format: "sha1", objects: [...objects.records.values()],
      refs: [{ name: "refs/heads/main", id: commit.id }],
      head: { type: "symbolic", target: "refs/heads/main", id: commit.id } },
    files: files.sort(),
  };
}
