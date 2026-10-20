import { createHash } from "node:crypto";
import { deflateSync } from "node:zlib";

export const digest = (format, bytes) => createHash(format === 0 ? "sha1" : "sha256").update(bytes).digest();
export const object = (format, kind, body) => ({ kind, body: Buffer.from(body), id: digest(format, Buffer.concat([Buffer.from(`${["", "commit", "tree", "blob", "tag"][kind]} ${Buffer.byteLength(body)}\0`), Buffer.from(body)])) });
export function packet(bytes) { bytes = Buffer.from(bytes); return Buffer.concat([Buffer.from((bytes.length + 4).toString(16).padStart(4, "0")), bytes]); }

export function entry(kind, body, base = Buffer.alloc(0)) {
  let size = BigInt(body.length);
  let byte = (kind << 4) | Number(size & 15n);
  size >>= 4n;
  const header = [];
  for (;;) {
    header.push(byte | (size > 0n ? 128 : 0));
    if (size === 0n) break;
    byte = Number(size & 127n); size >>= 7n;
  }
  return Buffer.concat([Buffer.from(header), base, deflateSync(body)]);
}

export function pack(format, entries) {
  const header = Buffer.from("5041434b0000000200000000", "hex"); header.writeUInt32BE(entries.length, 8);
  const body = Buffer.concat([header, ...entries]); return Buffer.concat([body, digest(format, body)]);
}

export function wire(format, packed, newId, name = "refs/tags/value") {
  const caps = `report-status-v2 delete-refs ofs-delta atomic push-options side-band-64k quiet object-format=${format === 0 ? "sha1" : "sha256"}`;
  const command = Buffer.concat([Buffer.from(`${"0".repeat(newId.length * 2)} ${newId.toString("hex")} `), Buffer.from(name), Buffer.from(`\0${caps}\n`)]);
  return Buffer.concat([packet(command), Buffer.from("0000"), packet("ci.skip"), packet(""), Buffer.from("0000"), packed]);
}

export function deltas(format, forward = false) {
  const objects = ["hello", "hello!", "hello!?"].map((body) => object(format, 3, body));
  const plain = entry(3, objects[0].body);
  const middle = entry(forward ? 7 : 6, Buffer.from([5, 6, 0x90, 5, 1, 33]), forward ? objects[0].id : Buffer.from([plain.length]));
  const last = entry(7, Buffer.from([6, 7, 0x90, 6, 1, 63]), objects[1].id);
  const packed = pack(format, forward ? [last, middle, plain] : [plain, middle, last]);
  return { objects, pack: packed, wire: wire(format, packed, objects[2].id), root: objects[2] };
}

export function project(format, invalid = false, aliases = false) {
  const blob = object(format, 3, "file contents\n");
  const emptyTree = object(format, 2, "");
  const names = aliases
    ? Buffer.concat([Buffer.from("100644 foo\0"), blob.id, Buffer.from("100644 foo.bar\0"), blob.id, Buffer.from("40000 foo\0"), emptyTree.id])
    : Buffer.concat([Buffer.from("100644 file\0"), blob.id]);
  const tree = object(format, 2, names);
  const commit = object(format, 1, `tree ${invalid ? "0".repeat(tree.id.length * 2) : tree.id.toString("hex")}\nauthor A <a@b> 0 +0000\ncommitter A <a@b> 0 +0000\n\ninitial\n`);
  const objects = aliases ? [blob, emptyTree, tree, commit] : [blob, tree, commit];
  const packed = pack(format, objects.map((value) => entry(value.kind, value.body)));
  return { objects, pack: packed, wire: wire(format, packed, commit.id, "refs/heads/main"), root: commit };
}
