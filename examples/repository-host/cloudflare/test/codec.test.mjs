import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { deflateSync, gzipSync } from "node:zlib";
import { Codec, CodecError, hex, integer } from "../src/codec.mjs";

import { FetchJob } from "../src/fetch.mjs";
import { ObjectSource } from "../src/source.mjs";
import { PAGE_BYTES } from "../src/storage.mjs";

const module = await WebAssembly.compile(await readFile(new URL("../build/git_zero_host_wasm.wasm", import.meta.url)));
const empty = new Uint8Array();
const objectId = (format, kind, body) => createHash(format === 0 ? "sha1" : "sha256").update(`${kind} ${body.length}\0`).update(body).digest();
const packet = (bytes) => { bytes = Buffer.from(bytes); return Buffer.concat([Buffer.from((bytes.length + 4).toString(16).padStart(4, "0")), bytes]); };

test("canonical capture scans whole pages across tiny delta copies", async () => {
  const copies = PAGE_BYTES + 1, body = Buffer.alloc(copies, 122);
  const variable = []; let size = copies;
  do { const byte = size & 127; size = Math.floor(size / 128); variable.push(byte | (size ? 128 : 0)); } while (size);
  const instructions = Buffer.from([1, ...variable]);
  const encoded = Buffer.alloc(instructions.length + copies * 2);
  encoded.set(instructions);
  for (let at = instructions.length; at < encoded.length; at += 2) encoded.set([0x90, 1], at);
  for (const format of [0, 1]) {
    const codec = new Codec(module, format);
    const base = { stage: "capture", node: 12, verified: 1, kind: 3, size: 1, oid: hex(objectId(format, "blob", Buffer.from("z"))) };
    const root = { ...base, node: 13, size: copies, oid: hex(objectId(format, "blob", body)) };
    const source = new ObjectSource({ exec: () => [] }, codec, "capture");
    source.descriptor = (row) => row === base ? { info: { kind: 3, size: 1 } } : { info: { kind: 3, size: copies }, base: { key: source.key(base), info: { kind: 3, size: 1 }, depth: 0 }, instructionsSize: encoded.length };
    source.readRow = async (row, kind, offset, length) => {
      const bytes = row === base ? Buffer.from("z") : encoded;
      assert.equal(kind, row === base ? 0 : 1);
      return bytes.subarray(offset, offset + length);
    };
    let scans = 0, requests = 0;
    const feed = codec.graphFeed.bind(codec), request = codec.rangeRequest.bind(codec);
    codec.graphFeed = (...args) => { scans++; return feed(...args); };
    codec.rangeRequest = () => { requests++; return request(); };
    assert.equal(await source.process(root, false), root.oid);
    assert.deepEqual(source.canonical.get(root.oid), new Uint8Array(body));
    assert.equal(scans, Math.ceil(copies / PAGE_BYTES));
    assert.ok(requests <= copies + 2, "rangeFeed already returns the next typed request");
  }
});

test("duplicate commit haves and shallow markers consult the catalog once", async () => {
  for (const format of [0, 1]) for (const version of [0, 1, 2]) {
    const codec = new Codec(module, format), wanted = Buffer.alloc(codec.idBytes, 1), have = Buffer.alloc(codec.idBytes, 2), shallow = Buffer.alloc(codec.idBytes, 3);
    const sets = new Map(), lookups = [];
    const store = {
      published(id) { lookups.push(id); return { oid: id, kind: 1 }; },
      exec(sql, id) {
        const table = /(?:FROM|INTO) (fetch_\w+)/.exec(sql)?.[1];
        assert.ok(table, sql);
        const values = sets.get(table) ?? new Set(); sets.set(table, values);
        if (sql.startsWith("DELETE")) { values.clear(); return []; }
        if (sql.startsWith("SELECT 1")) return (id === undefined ? values.size > 0 : values.has(id)) ? [{}] : [];
        if (sql.startsWith("INSERT OR IGNORE")) { values.add(id); return []; }
        assert.fail(sql);
      },
    };
    const capability = `object-format=${format ? "sha256" : "sha1"}`;
    const lines = version === 2 ? [packet("command=fetch\n"), packet(capability), Buffer.from("0001")] : [];
    lines.push(packet(`want ${hex(wanted)}${version === 2 ? "" : ` multi_ack_detailed ${capability}`}\n`));
    for (let repeat = 0; repeat < 1025; repeat++) lines.push(packet(`shallow ${hex(shallow)}\n`));
    if (version !== 2) lines.push(Buffer.from("0000"));
    for (let repeat = 0; repeat < 1025; repeat++) lines.push(packet(`have ${hex(have)}\n`));
    lines.push(Buffer.from("0000"));
    await new FetchJob(store, codec, version).decode(new Request("http://fixture/repo.git/git-upload-pack", { method: "POST", body: Buffer.concat(lines) }));
    assert.deepEqual(lookups, [hex(wanted), hex(shallow), hex(have)]);
    assert.deepEqual([...sets.get("fetch_shallow")], [hex(shallow)]);
    assert.deepEqual([...sets.get("fetch_haves")], [hex(have)]);
  }
});

test("paged shallow changes retain frozen controls and exact protocol framing", () => {
  for (const format of [0, 1]) for (const version of [0, 1, 2]) for (const done of [false, true]) {
    const codec = new Codec(module, format);
    const wanted = Buffer.alloc(format ? 32 : 20, 1), previous = Buffer.alloc(wanted.length, 2);
    const capability = `object-format=${format ? "sha256" : "sha1"}`;
    const lines = version === 2 ? [packet("command=fetch\n"), packet(capability), Buffer.from("0001")] : [];
    lines.push(packet(`want ${hex(wanted)}${version === 2 ? "" : ` shallow ${capability}`}\n`), packet(`shallow ${hex(previous)}\n`), packet("deepen 2\n"));
    if (version !== 2) lines.push(Buffer.from("0000"));
    if (done) lines.push(packet("done\n"));
    if (version === 2) lines.push(Buffer.from("0000"));
    const controls = Buffer.concat(lines);
    codec.uploadInit(version);
    for (let at = 0; at < controls.length; at += 7) codec.uploadAppend(controls.subarray(at, at + 7));
    codec.uploadParse(); codec.uploadShallowInit();
    const output = [], enabled = version !== 2 || done;
    if (enabled) output.push(codec.uploadShallowChange(wanted), codec.uploadShallowChange(previous, true));
    output.push(codec.uploadShallowFinish());
    const expected = enabled ? Buffer.concat([...(version === 2 ? [packet("shallow-info\n")] : []), packet(`shallow ${hex(wanted)}\n`), packet(`unshallow ${hex(previous)}\n`), Buffer.from(version === 2 ? "0001" : "0000")]) : Buffer.alloc(0);
    assert.deepEqual(Buffer.concat(output), expected);
    assert.equal(codec.uploadNext().tag, 71); assert.equal(codec.uploadNext().tag, 73); assert.equal(codec.uploadNext().tag, 74);
    assert.throws(() => codec.uploadShallowFinish(), CodecError);
    assert.throws(() => codec.uploadNext(), CodecError);
  }
});

test("external frontier decisions use Rust history and blob-filter rules", () => {
  for (const format of [0, 1]) for (const version of [0, 1, 2]) {
    const codec = new Codec(module, format), wanted = Buffer.alloc(format ? 32 : 20, 1);
    const capability = `object-format=${format ? "sha256" : "sha1"}`;
    const prepare = (relative = false, filtered = true, depth = 2) => {
      const lines = version === 2 ? [packet("command=fetch\n"), packet(capability), Buffer.from("0001")] : [];
      lines.push(packet(`want ${hex(wanted)}${version === 2 ? "" : ` shallow filter${relative ? " deepen-relative" : ""} ${capability}`}\n`));
      if (depth !== null) lines.push(packet(`deepen ${depth}\n`));
      if (relative && version === 2) lines.push(packet("deepen-relative\n"));
      if (filtered) lines.push(packet("filter blob:limit=8\n"));
      if (version !== 2) lines.push(Buffer.from("0000"));
      lines.push(packet("done\n")); if (version === 2) lines.push(Buffer.from("0000"));
      codec.uploadInit(version); codec.uploadAppend(Buffer.concat(lines)); codec.uploadParse();
    };
    const commit = { kind: 1, size: 100 };
    prepare();
    assert.equal(codec.uploadHistory(true).flags, 9);
    assert.equal(codec.uploadSelect(commit, 1).flags, 3);
    assert.equal(codec.uploadSelect(commit, 2).flags, 5);
    assert.equal(codec.uploadSelect(commit, 1, { clientBoundary: true }).flags, 11);
    assert.equal(codec.uploadSelect({ kind: 3, size: 7 }, 1).flags & 1, 1);
    assert.equal(codec.uploadSelect({ kind: 3, size: 8 }, 1).flags & 1, 0);
    assert.equal(codec.uploadSelect({ kind: 3, size: 8 }, 1, { wanted: true }).flags & 1, 1);
    assert.equal(codec.uploadSelect({ kind: 2, size: 100 }, 1).flags & 1, 1);
    prepare(true, false);
    assert.equal(codec.uploadHistory(true).flags, 11);
    const boundary = codec.uploadSelect(commit, 3, { clientBoundary: true, probe: true });
    assert.equal(boundary.flags, 5); assert.equal(boundary.a, 4n);
    codec.uploadHistory(true, 3);
    assert.equal(codec.uploadSelect(commit, 4).flags, 3);
    assert.equal(codec.uploadSelect(commit, 5).flags, 5);
    assert.equal(codec.uploadHistory(false).flags, 8);
    assert.equal(codec.uploadSelect(commit, 99).a, 1n);
    assert.throws(() => codec.uploadSelect(commit, 0), CodecError);
    assert.throws(() => codec.uploadNext(), CodecError);
    prepare(false, false, null);
    assert.equal(codec.uploadHistory(false).flags, 4);
    assert.equal(codec.uploadSelect({ kind: 3, size: 100 }, 1).flags & 1, 1);
  }
});

function fixture(format) {
  const bodies = [Buffer.from("hello"), Buffer.from("hello!"), Buffer.from("hello!?")];
  const ids = bodies.map((body) => objectId(format, "blob", body));
  const plain = Buffer.concat([Buffer.from([0x35]), deflateSync(bodies[0])]);
  assert.ok(plain.length < 128);
  const middle = Buffer.concat([Buffer.from([0x66, plain.length]), deflateSync(Buffer.from([5, 6, 0x90, 5, 1, 33]))]);
  const last = Buffer.concat([Buffer.from([0x76]), ids[1], deflateSync(Buffer.from([6, 7, 0x90, 6, 1, 63]))]);
  const body = Buffer.concat([Buffer.from("5041434b0000000200000003", "hex"), plain, middle, last]);
  const checksum = createHash(format === 0 ? "sha1" : "sha256").update(body).digest();
  const pack = Buffer.concat([body, checksum]);
  const caps = `report-status-v2 delete-refs ofs-delta atomic push-options side-band-64k quiet object-format=${format === 0 ? "sha1" : "sha256"}`;
  const command = Buffer.concat([Buffer.from(`${"0".repeat(ids[2].length * 2)} ${hex(ids[2])} refs/tags/value\0${caps}\n`)]);
  const wire = Buffer.concat([packet(command), Buffer.from("0000"), packet("ci.skip"), packet(""), Buffer.from("0000"), pack]);
  return { bodies, ids, pack, checksum, wire };
}

function receive(codec, fixture, chunk) {
  codec.receiveInit();
  let offset = 0, current;
  const entries = [], commands = [], options = [];
  for (let step = 0; step < fixture.wire.length * 4 + 100; step++) {
    const end = Math.min(offset + chunk, fixture.wire.length);
    const event = codec.receiveStep(fixture.wire.subarray(offset, end), end === fixture.wire.length);
    assert.ok(event.consumed <= end - offset);
    offset += event.consumed;
    if (event.tag === 3) commands.push(event);
    if (event.tag === 5) options.push(Buffer.from(event.data));
    if (event.tag === 8) {
      current = { offset: integer(event.a), size: integer(event.b), compressedOffset: integer(event.c), kind: event.flags, baseOffset: integer(event.d), baseId: event.id, body: [] };
      entries.push(current);
    }
    if ((event.tag === 9 || event.tag === 10) && event.data.length) current.body.push(Buffer.from(event.data));
    if (event.tag === 10) {
      current.end = integer(event.a); current.id = event.flags ? event.id : null;
      assert.equal(!!(event.flags & 2), current.kind === 3, "only authenticated plain blob entries can bypass structural scanning");
    }
    if (event.tag === 11) assert.equal(hex(event.id), hex(fixture.checksum));
    if (event.tag === 12) {
      assert.equal(offset, fixture.wire.length);
      assert.equal(codec.receiveCapabilities(), 254);
      assert.equal(commands.length, 1);
      assert.equal(Buffer.from(commands[0].data).toString(), "refs/tags/value");
      assert.equal(hex(commands[0].other), hex(fixture.ids[2]));
      assert.deepEqual(options.map((value) => value.toString()), ["ci.skip", ""]);
      return entries;
    }
  }
  assert.fail("receive session made no bounded progress");
}

function source(codec, pack, entry, range) {
  codec.sourceInit({ offset: entry.offset, maxBytes: entry.end - entry.offset }, range);
  let offset = entry.offset;
  const parts = [];
  for (let step = 0; step < pack.length * 8 + 100; step++) {
    const end = Math.min(offset + 3, entry.end);
    const event = codec.sourceStep(pack.subarray(offset, end), end === entry.end, 7);
    offset += event.consumed;
    if (event.data.length) parts.push(Buffer.from(event.data));
    if (event.tag === 41) {
      const result = Buffer.concat(parts);
      assert.equal(result.length, Number(range.length));
      return result;
    }
  }
  assert.fail("source session made no bounded progress");
}

function graph(codec, bytes, eof) {
  let offset = 0;
  const events = [];
  for (let step = 0; step < bytes.length + 100; step++) {
    const event = codec.graphFeed(bytes.subarray(offset), eof);
    offset += event.consumed;
    if (event.tag === 31 || event.tag === 32) events.push(event);
    if (event.tag === 33) { assert.equal(offset, bytes.length); return { events, id: event.id }; }
    if (event.tag === 30 && offset === bytes.length && !eof) return { events };
  }
  assert.fail("graph session made no bounded progress");
}

test("hosting Wasm has fixed memory, no imports and no allocator", () => {
  assert.deepEqual(WebAssembly.Module.imports(module), []);
  assert.ok(!WebAssembly.Module.exports(module).some(({ name }) => /alloc|free/.test(name)));
  const codec = new Codec(module);
  assert.ok(codec.memory.byteLength <= 96 * 65536, "hosting state plus 4 MiB frozen controls exceeds its 6 MiB linear-memory budget");
});

test("fixed ABI frames reject wrong lengths even when valid trailing bytes remain in input", () => {
  function upload(codec) {
    codec.uploadInit(0);
    codec.uploadAppend(Buffer.concat([
      packet(`want ${"01".repeat(codec.idBytes)} object-format=${codec.format ? "sha256" : "sha1"}\n`),
      Buffer.from("0000"), packet("done\n"),
    ]));
    codec.uploadParse();
  }
  const range = (codec) => codec.rangeInit({ staged: 12 });
  const compose = (codec) => codec.packStart(1, 0);
  const history = (codec) => { upload(codec); codec.uploadHistory(false); };
  const cases = [
    ["host_receive_init", 72, (codec) => codec.receiveInit()],
    ["host_range_init", 104, range],
    ["host_source_init", 64, (codec) => codec.sourceInit({ offset: 12, maxBytes: 1024 }, { offset: 0, length: 0 })],
    ["host_graph_init", 40, (codec) => codec.graphInit({ kind: 3, size: 0 })],
    ["host_range_describe", 96, (codec) => { range(codec); codec.rangeDescribe({ info: { kind: 3, size: 0 } }); }, range],
    ["host_pack_entry", 88, (codec) => { compose(codec); codec.packEntry({ kind: 3, id: Buffer.alloc(codec.idBytes, 1), size: 0, compressedSize: 11 }); }, compose],
    ["host_upload_history_init", 8, history, upload],
    ["host_upload_select", 20, (codec) => { history(codec); codec.uploadSelect({ kind: 3, size: 0 }, 1); }, history, [0]],
  ];
  for (const format of [0, 1]) for (const [operation, size, prepare, reset = () => {}, args = []] of cases) {
    const codec = new Codec(module, format);
    prepare(codec); // Prove that the saved input describes a valid operation.
    const frame = new Uint8Array(codec.memory, codec.exports.host_input_ptr(), size).slice();
    for (const length of [...Array(size).keys(), size + 1, 65537]) {
      reset(codec); codec.stage(frame);
      assert.equal(codec.exports[operation](length, ...args), length > 65536 ? -1 : -3, `${operation} format=${format} length=${length}`);
    }
    prepare(codec); // A correct frame must still reinitialize after rejection.
  }
});

test("streamed receive validates compressed plain/OFS/REF intake in both formats", () => {
  for (const format of [0, 1]) {
    const input = fixture(format);
    for (const chunk of [1, 7, 65536]) {
      const codec = new Codec(module, format, { capabilities: 255 });
      const memory = codec.memory;
      const entries = receive(codec, input, chunk);
      assert.deepEqual(entries.map((entry) => entry.kind), [3, 6, 7]);
      assert.equal(hex(entries[0].id), hex(input.ids[0]));
      assert.equal(entries[1].baseOffset, entries[0].offset);
      assert.equal(hex(entries[2].baseId), hex(input.ids[1]));
      assert.deepEqual(Buffer.concat(entries[0].body), input.bodies[0]);
      assert.equal(codec.exports.memory.buffer, memory);
      assert.equal(codec.receiveStep(empty, true).tag, 12);
    }
  }
});

test("storage effects reconstruct and hash canonical objects through separate packed-range sessions", () => {
  for (const format of [0, 1]) {
    const input = fixture(format), codec = new Codec(module, format, { capabilities: 255 });
    const entries = receive(codec, input, 5);
    const records = entries.map((entry, index) => ({ entry, info: { kind: 3, size: input.bodies[index].length } }));
    for (let index = 1; index < records.length; index++) {
      const base = records[index - 1];
      const prefix = source(codec, input.pack, entries[index], { offset: 0, length: Math.min(32, entries[index].size) });
      const header = codec.deltaHeader(prefix, { ...base.info, depth: index - 1 });
      assert.equal(header.tag, 50);
      assert.equal(header.b, BigInt(records[index].info.size));
      records[index].base = { key: { staged: base.entry.offset }, info: base.info, depth: index - 1 };
      records[index].instructionsSize = entries[index].size;
    }
    const root = records[2];
    codec.rangeInit({ staged: root.entry.offset });
    codec.graphInit(root.info);
    const parts = [];
    let digest;
    for (let steps = 0; steps < 10000; steps++) {
      const request = codec.rangeRequest();
      if (request.tag === 20) {
        codec.rangeDescribe(records.find((record) => BigInt(record.entry.offset) === request.key.staged));
      } else if (request.tag === 21) {
        const record = records.find((record) => BigInt(record.entry.offset) === request.key.staged);
        const data = source(codec, input.pack, record.entry, { offset: request.a, length: request.b });
        const event = codec.rangeFeed(data, 3);
        parts.push(Buffer.from(event.data));
        const scanned = graph(codec, event.data, event.tag === 22);
        digest ??= scanned.id;
      } else if (request.tag === 22) {
        digest ??= graph(codec, empty, true).id;
        break;
      } else assert.fail(`unexpected range event ${request.tag}`);
    }
    assert.deepEqual(Buffer.concat(parts), input.bodies[2]);
    assert.equal(hex(digest), hex(input.ids[2]));
  }
});

test("graph events retain tree names, historical modes and typed commit parents", () => {
  for (const format of [0, 1]) {
    const codec = new Codec(module, format);
    const target = objectId(format, "blob", Buffer.from("body"));
    const body = Buffer.concat([Buffer.from("100664 "), Buffer.from([0xff]), Buffer.from([0]), target]);
    codec.graphInit({ kind: 2, size: body.length });
    const result = graph(codec, body, true);
    assert.equal(result.events.length, 1);
    assert.equal(result.events[0].tag, 32);
    assert.equal(result.events[0].flags, 3);
    assert.equal(result.events[0].a, BigInt(0o100664));
    assert.deepEqual(Buffer.from(result.events[0].data), Buffer.from([0xff]));
    assert.equal(hex(result.id), hex(objectId(format, "tree", body)));
    const commit = Buffer.from(`tree ${hex(result.id)}\nparent ${hex(target)}\nauthor A\ncommitter A\n\nmessage`);
    codec.graphInit({ kind: 1, size: commit.length });
    const links = graph(codec, commit, true);
    assert.deepEqual(links.events.map((event) => [event.tag, event.flags, event.a]), [[31, 2, 0n], [31, 1, 1n]]);
    assert.equal(hex(links.id), hex(objectId(format, "commit", commit)));
  }
});

test("strict receive header policy is opt-in and survives fragmented identities", () => {
  for (const format of [0, 1]) {
    const codec = new Codec(module, format), target = "1".repeat(codec.idBytes * 2);
    for (const [identity, accepted] of [["A <a@b> 0 +0000", true], [" <a@b> 1 -9999", true], ["A <a@b> \t1 +9999", true], ["x", false], ["<a@b> 1 +0000", false], ["A <a@b> 01 +0000", false], ["A <a@b> 0\t+0000", false], ["A <a@b> 9223372036854775808 +0000", false]]) {
      const body = Buffer.from(`tree ${target}\nauthor ${identity}\ncommitter A <a@b> 0 +0000\n\nmessage`);
      for (const chunk of [1, 7, body.length]) {
        codec.graphInit({ kind: 1, size: body.length }, { strict: true });
        const scan = () => { for (let at = 0; at < body.length; at += chunk) graph(codec, body.subarray(at, at + chunk), at + chunk >= body.length); };
        if (accepted) scan();
        else assert.throws(scan, (error) => error instanceof CodecError && error.code === -7);
      }
      codec.graphInit({ kind: 1, size: body.length });
      assert.equal(hex(graph(codec, body, true).id), hex(objectId(format, "commit", body)));
    }
  }
});

test("malformed intake is terminal and a fresh session clears old scratch", () => {
  const codec = new Codec(module, 0, { capabilities: 255 });
  const input = fixture(0);
  const corrupt = { ...input, wire: Buffer.from(input.wire) };
  corrupt.wire[corrupt.wire.length - 1] ^= 1;
  assert.throws(() => receive(codec, corrupt, 17), (error) => error instanceof CodecError && error.code === -4);
  assert.throws(() => codec.receiveStep(empty, true), (error) => error.code === -4);
  assert.equal(receive(codec, input, 17).length, 3);
  codec.receiveCancel();
  assert.throws(() => codec.receiveStep(empty, true), (error) => error.code === -3);
  codec.stage(new Uint8Array(1));
  assert.equal(codec.exports.host_range_init(1), -3);
  assert.throws(() => codec.rangeRequest(), (error) => error.code === -3);
});


test("compressed fetch controls enforce transport and decoded byte budgets", async () => {
  let parsed = false;
  const codec = { limits: { maxControlBytes: 64 }, uploadInit() {}, uploadAppend() {}, uploadParse() { parsed = true; return { flags: 0 }; } };
  const store = { namespace: "gzip-budget", exec() { return []; } };
  const request = (body) => new Request("http://example.test/repo.git/git-upload-pack", { method: "POST", headers: { "content-encoding": "gzip" }, body });
  const emptyBlocks = Buffer.concat([Buffer.from("1f8b0800000000000003", "hex"), ...Array.from({ length: 20 }, () => Buffer.from([0, 0, 0, 255, 255])), Buffer.from([1, 0, 0, 255, 255]), Buffer.alloc(8)]);
  await assert.rejects(new FetchJob(store, codec, 0).decode(request(emptyBlocks)), /compressed fetch controls exceed byte limit/);
  assert.equal(parsed, false, "oversized compressed input must fail before parsing");
  await new FetchJob(store, codec, 0).decode(request(gzipSync(Buffer.from("0000"))));
  assert.equal(parsed, true);
  const real = new Codec(module, 0, { maxControlBytes: 64 });
  await assert.rejects(new FetchJob(store, real, 0).decode(request(gzipSync(Buffer.alloc(65, 97)))), CodecError);
});


test("offset delta permission survives the bounded upload ABI", () => {
  for (const format of [0, 1]) for (const version of [0, 1, 2]) for (const ofs of [false, true]) {
    const codec = new Codec(module, format), id = Buffer.alloc(format ? 32 : 20, 1).toString("hex");
    const capability = `object-format=${format ? "sha256" : "sha1"}`;
    const lines = version === 2 ? [packet("command=fetch\n"), packet(capability), Buffer.from("0001")] : [];
    lines.push(packet(`want ${id}${version === 2 ? "" : ` ${capability}${ofs ? " ofs-delta" : ""}`}\n`));
    if (version === 2 && ofs) lines.push(packet("ofs-delta\n"));
    if (version !== 2) lines.push(Buffer.from("0000"));
    lines.push(packet("done\n")); if (version === 2) lines.push(Buffer.from("0000"));
    codec.uploadInit(version); codec.uploadAppend(Buffer.concat(lines));
    assert.equal(!!(codec.uploadParse().flags & (1 << 21)), ofs);
    codec.uploadCancel();
  }
});
