import { testRuntimes } from "./runtimes.mjs";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { inflateSync } from "node:zlib";
import { digest, entry, object, pack, packet, wire } from "./fixture.mjs";
import { headers } from "./wrangler.mjs";

// Parse the wire independently of the server's pack codec.
function kinds(packed, format) {
  assert.equal(packed.subarray(0, 4).toString(), "PACK");
  const idBytes = format ? 32 : 20, result = [];
  let offset = 12;
  for (let index = 0; index < packed.readUInt32BE(8); index++) {
    let byte = packed[offset++]; const kind = (byte >> 4) & 7;
    while (byte & 128) byte = packed[offset++];
    if (kind === 6) { do { byte = packed[offset++]; } while (byte & 128); }
    if (kind === 7) offset += idBytes;
    const inflated = inflateSync(packed.subarray(offset), { info: true });
    assert.ok(inflated.engine.bytesWritten > 0);
    offset += inflated.engine.bytesWritten; result.push(kind);
  }
  assert.equal(offset + idBytes, packed.length);
  assert.deepEqual(digest(format, packed.subarray(0, offset)), packed.subarray(offset));
  return result;
}
function responsePack(bytes, version) {
  const chunks = [];
  for (let at = 0; at < bytes.length;) {
    if (version !== 2 && bytes.subarray(at, at + 4).toString() === "PACK") return bytes.subarray(at);
    const length = Number.parseInt(bytes.subarray(at, at + 4).toString(), 16);
    assert.ok(Number.isInteger(length));
    if (length <= 2) { at += 4; continue; }
    assert.ok(length >= 4 && at + length <= bytes.length);
    if (bytes[at + 4] === 1) chunks.push(bytes.subarray(at + 5, at + length));
    at += length;
  }
  return Buffer.concat(chunks);
}
function git(directory, ...args) {
  const result = spawnSync("git", args, { cwd: directory, encoding: "utf8", timeout: 10000, env: { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null" } });
  assert.equal(result.status, 0, `${args.join(" ")}\n${result.stderr}`); return result.stdout.trim();
}
testRuntimes("{runtime} negotiates offset delta reuse", { timeout: 60000 }, async (serve) => {
  const directory = await mkdtemp(join(tmpdir(), "git-zero-ofs-")); let server;
  try {
    server = await serve(join(directory, "state"));
    for (const format of [0, 1]) {
      const base = object(format, 3, "hello"), blob = object(format, 3, "hello!");
      const plain = entry(3, base.body);
      const delta = entry(6, Buffer.from([5, 6, 0x90, 5, 1, 33]), Buffer.from([plain.length]));
      const tree = object(format, 2, Buffer.concat([Buffer.from("100644 base\0"), base.id, Buffer.from("100644 file\0"), blob.id]));
      const commit = object(format, 1, `tree ${tree.id.toString("hex")}\nauthor A <a@b> 0 +0000\ncommitter A <a@b> 0 +0000\n\ninitial\n`);
      const packed = pack(format, [plain, delta, entry(2, tree.body), entry(1, commit.body)]);
      const remote = `${server.origin}/ofs-${format}-${randomUUID()}.git`;
      const namespace = remote.split("/").at(-1).slice(0, -4);
      const initialized = await fetch(`${server.origin}/_test/${namespace}/initialize`, { method: "POST", headers: headers(format) });
      assert.equal(initialized.status, 200); await initialized.arrayBuffer();
      const push = await fetch(`${remote}/git-receive-pack`, { method: "POST", headers: headers(format), body: wire(format, packed, commit.id, "refs/heads/main") });
      assert.equal(push.status, 200); assert.match(await push.text(), /unpack/);
      const references = await fetch(`${server.origin}/_test/${namespace}/refs`, { headers: headers() });
      assert.equal(references.status, 200);
      assert.deepEqual((await references.json()).refs.map(ref => ({ name: Buffer.from(ref.name).toString(), oid: ref.oid })), [{ name: "refs/heads/main", oid: commit.id.toString("hex") }]);
      for (const version of [0, 1, 2]) {
        const http = { ...headers(format), "content-type": "application/x-git-upload-pack-request", "git-protocol": `version=${version}` };
        const advertised = await fetch(`${remote}/info/refs?service=git-upload-pack`, { headers: http });
        assert.equal(advertised.status, 200);
        const advertisement = await advertised.text();
        if (version !== 2) assert.match(advertisement, / ofs-delta(?: |\n)/);
        for (const ofs of [false, true]) {
          const capability = `object-format=${format ? "sha256" : "sha1"}`;
          const lines = version === 2 ? [packet("command=fetch\n"), packet(capability), Buffer.from("0001")] : [];
          lines.push(packet(`want ${commit.id.toString("hex")}${version === 2 ? "" : ` ${capability}${ofs ? " ofs-delta" : ""}`}\n`));
          if (version === 2 && ofs) lines.push(packet("ofs-delta\n"));
          if (version !== 2) lines.push(Buffer.from("0000"));
          lines.push(packet("done\n")); if (version === 2) lines.push(Buffer.from("0000"));
          const response = await fetch(`${remote}/git-upload-pack`, { method: "POST", headers: http, body: Buffer.concat(lines) });
          assert.equal(response.status, 200);
          const received = responsePack(Buffer.from(await response.arrayBuffer()), version);
          assert.deepEqual(kinds(received, format), [3, ofs ? 6 : 7, 2, 1]);
          const repository = join(directory, `check-${format}-${version}-${ofs}`);
          git(directory, "init", "--bare", `--object-format=${format ? "sha256" : "sha1"}`, repository);
          const indexed = spawnSync("git", ["index-pack", "--stdin", "--strict"], { cwd: repository, input: received, encoding: "utf8", timeout: 10000 });
          assert.equal(indexed.status, 0, indexed.stderr);
          git(repository, "update-ref", "refs/heads/main", commit.id.toString("hex"));
          git(repository, "fsck", "--full", "--strict");
          assert.equal(git(repository, "cat-file", "-p", blob.id.toString("hex")), "hello!");
          if (ofs) assert.deepEqual(received, packed, "negotiated covering pack keeps its original offset deltas");
        }
      }
    }
  } finally { await server?.stop(); await rm(directory, { recursive: true, force: true }); }
}, { frozen: true });
