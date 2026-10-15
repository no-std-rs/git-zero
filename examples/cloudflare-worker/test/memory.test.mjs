import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { inflateSync } from "node:zlib";

const module = await WebAssembly.compile(await readFile(new URL("../build/git_zero_worker_wasm.wasm", import.meta.url)));
const INPUT_BYTES = 64 * 1024;
const OUTPUT_BYTES = 128 * 1024;
const MEMORY_BUDGET = 24 * 64 * 1024;

function instance() {
  const { exports } = new WebAssembly.Instance(module, {});
  assert.equal(exports.input_capacity(), INPUT_BYTES);
  assert.equal(exports.output_capacity(), OUTPUT_BYTES);
  assert.ok(exports.memory.buffer.byteLength <= MEMORY_BUDGET, "initial linear memory exceeds 24 Wasm pages");
  return exports;
}

function frame(parts, target, body = Buffer.alloc(0)) {
  const files = parts.map((part) => Buffer.from(part));
  const path = Buffer.from(target);
  const lengths = Buffer.alloc(16);
  files.forEach((file, index) => lengths.writeUInt32LE(file.length, index * 4));
  const targetLength = Buffer.alloc(4);
  targetLength.writeUInt32LE(path.length);
  return Buffer.concat([lengths, ...files, targetLength, path, body]);
}

function call(exports, input, method = 0, format = null, http = false) {
  assert.ok(input.length <= INPUT_BYTES);
  const memory = exports.memory.buffer;
  new Uint8Array(memory, exports.input_ptr(), input.length).set(input);
  const length = http ? exports.serve_http(input.length, method, format) : format === null ? exports.serve(input.length, method) : exports.serve_with_format(input.length, method, format);
  assert.equal(exports.memory.buffer, memory, "serve grew or replaced linear memory");
  if (length < 0) return length;
  assert.ok(length >= 3 && length <= OUTPUT_BYTES, "response exceeds the output budget");
  const output = Buffer.from(new Uint8Array(memory, exports.output_ptr(), length));
  return { status: output.readUInt16LE(0), type: output[2], body: output.subarray(3) };
}

function httpFrame(target, body, protocol, media = "application/x-git-upload-pack-request") {
  const headers = [Buffer.from(protocol), Buffer.from(media)];
  const lengths = Buffer.alloc(8);
  headers.forEach((value, index) => lengths.writeUInt32LE(value.length, index * 4));
  return Buffer.concat([frame(parts, target), lengths, ...headers, body]);
}

function command(format, name, args) {
  return Buffer.concat([
    packet(`command=${name}\n`), packet(`object-format=${format === 0 ? "sha1" : "sha256"}\n`), Buffer.from("0001"),
    ...args.map((arg) => packet(`${arg}\n`)), Buffer.from("0000"),
  ]);
}

function checkV2Round(exports, format, head) {
  for (const version of [1, 2]) {
    const discovery = call(exports, httpFrame("info/refs?service=git-upload-pack", Buffer.alloc(0), `version=${version}`), 0, format, true);
    assert.ok(discovery.body.includes(packet(`version ${version}\n`)));
  }
  const refs = command(format, "ls-refs", ["symrefs", "peel", "ref-prefix HEAD"]);
  const result = call(exports, httpFrame("git-upload-pack", refs, "version=2"), 1, format, true);
  assert.deepEqual(result.body, Buffer.concat([packet(`${head} HEAD symref-target:refs/heads/main\n`), Buffer.from("0000")]));
  const fetch = command(format, "fetch", [`want ${head}`, "done"]);
  const response = call(exports, httpFrame("git-upload-pack", fetch, "version=2"), 1, format, true);
  const { lines, pack } = decodeNegotiation(response.body, "side-band-64k");
  assert.deepEqual(lines, ["packfile\n"]);
  const width = format === 0 ? 20 : 32;
  assert.deepEqual(createHash(format === 0 ? "sha1" : "sha256").update(pack.subarray(0, -width)).digest(), pack.subarray(-width));
  const invalid = call(exports, httpFrame("git-upload-pack", fetch, "version=2", "text/plain"), 1, format, true);
  assert.equal(invalid.status, 415);
  assert.equal(invalid.body.length, 0);
  assert.equal(call(exports, httpFrame("git-upload-pack", fetch.subarray(0, -1), "version=2"), 1, format, true), -2);
}

test("v1/v2 discovery, ls-refs and fetch preserve fixed Wasm memory in both formats", () => {
  const exports = instance();
  for (const format of [0, 1]) {
    const headV0 = call(exports, frame(parts, "info/refs?service=git-upload-pack"), 0, format).body.toString();
    const head = new RegExp(`([0-9a-f]{${format === 0 ? 40 : 64}}) HEAD\\0`).exec(headV0)[1];
    for (let round = 0; round < 32; round++) checkV2Round(exports, format, head);
  }
});

function packet(line) {
  return Buffer.from(`${(Buffer.byteLength(line) + 4).toString(16).padStart(4, "0")}${line}`);
}

const parts = ["{\"colo\":\"CDG\"}\n", "{}\n", "{\"public\":true}\n", "{\"format\":1}\n"];

function fetchBody(head, format, version, args = [], capabilities = "") {
  const options = args.map((arg) => packet(`${arg}\n`));
  if (version === 2) return command(format, "fetch", [`want ${head}`, ...args, "done"]);
  return Buffer.concat([packet(`want ${head}${capabilities}\n`), ...options, Buffer.from("0000"), packet("done\n")]);
}

function assertFilteredPack(response, head, format, version) {
  const prefix = Buffer.concat([
    ...(version === 2 ? [packet("shallow-info\n")] : []), packet(`shallow ${head}\n`),
    Buffer.from(version === 2 ? "0001" : "0000"),
  ]);
  assert.deepEqual(response.body.subarray(0, prefix.length), prefix);
  const { pack } = decodeNegotiation(response.body.subarray(prefix.length), version === 2 ? "side-band-64k" : "");
  assert.equal(pack.readUInt32BE(8), 2, "only commit and tree are sent");
  const width = head.length / 2;
  assert.deepEqual(createHash(format === 0 ? "sha1" : "sha256").update(pack.subarray(0, -width)).digest(), pack.subarray(-width));
}

function checkShallowFiltered(first, second, format, version) {
  const discovery = call(first, frame(parts, "info/refs?service=git-upload-pack"), 0, format);
  const head = new RegExp(`([0-9a-f]{${format === 0 ? 40 : 64}}) HEAD\\0`).exec(discovery.body.toString())[1];
  const body = fetchBody(head, format, version, ["deepen 1", "filter blob:none"], ` filter object-format=${format === 0 ? "sha1" : "sha256"}`);
  const input = httpFrame("git-upload-pack", body, `version=${version}`);
  const expected = call(first, input, 1, format, true);
  assertFilteredPack(expected, head, format, version);
  for (let round = 0; round < 16; round++) {
    checkStreamingTarget(first, second, input, 1, format, version === 2, expected);
  }
  const relative = fetchBody(head, format, version,
    [`shallow ${head}`, "deepen 1", ...(version === 2 ? ["deepen-relative"] : [])],
    ` deepen-relative object-format=${format === 0 ? "sha1" : "sha256"}`);
  const relativeInput = httpFrame("git-upload-pack", relative, `version=${version}`);
  const deepened = call(first, relativeInput, 1, format, true);
  assert.equal(deepened.status, 200);
  assert.ok(deepened.body.includes(packet(`unshallow ${head}\n`)));
  assert.ok(!deepened.body.includes(packet(`shallow ${head}\n`)));
  for (let round = 0; round < 16; round++) {
    checkStreamingTarget(first, second, relativeInput, 1, format, version === 2, deepened);
  }
  const full = version === 2
    ? command(format, "fetch", [`want ${head}`, `have ${head}`, "deepen 1", "deepen-relative", "done"])
    : Buffer.concat([packet(`want ${head} deepen-relative object-format=${format === 0 ? "sha1" : "sha256"}\n`),
      packet("deepen 1\n"), Buffer.from("0000"), packet(`have ${head}\n`), packet("done\n")]);
  const fullInput = httpFrame("git-upload-pack", full, `version=${version}`);
  const unchanged = call(first, fullInput, 1, format, true);
  const { lines, pack } = decodeNegotiation(version === 2 ? unchanged.body : unchanged.body.subarray(4), version === 2 ? "side-band-64k" : null);
  assert.deepEqual(lines, version === 2 ? ["packfile\n"] : [`ACK ${head}\n`]);
  assert.equal(pack.readUInt32BE(8), 0, "a full client's common history must not be resent");
  for (let round = 0; round < 16; round++) {
    checkStreamingTarget(first, second, fullInput, 1, format, version === 2, unchanged);
  }
}

test("shallow and filtered pulls retain fixed memory, format widths and exact packs", () => {
  const first = instance();
  const second = instance();
  for (const format of [0, 1]) {
    for (const version of [0, 1, 2]) checkShallowFiltered(first, second, format, version);
  }
});

function decodeNegotiation(wire, band) {
  const lines = [];
  const packed = [];
  while (wire.length) {
    if (!band && wire.subarray(0, 4).toString() === "PACK") {
      packed.push(wire);
      break;
    }
    const length = Number.parseInt(wire.subarray(0, 4).toString(), 16);
    if (length === 0) { assert.equal(wire.length, 4); break; }
    assert.ok(length <= (band === "side-band" ? 1000 : 65520));
    const line = wire.subarray(4, length);
    wire = wire.subarray(length);
    if (line[0] === 1) packed.push(line.subarray(1));
    else lines.push(line.toString());
  }
  return { lines, pack: Buffer.concat(packed) };
}

function assertNegotiatedResponse(response, head, mode, band, hash) {
  assert.equal(response.status, 200);
  const { lines, pack } = decodeNegotiation(response.body, band);
  assert.deepEqual(lines, [`ACK ${head} ${mode === "multi_ack" ? "continue" : "common"}\n`, `ACK ${head}\n`]);
  const checksumBytes = head.length / 2;
  assert.equal(pack.subarray(0, 4).toString(), "PACK");
  assert.deepEqual(createHash(hash).update(pack.subarray(0, -checksumBytes)).digest(), pack.subarray(-checksumBytes));
}

function checkRepeatedNegotiation(exports, format, head, mode, band) {
  const memory = exports.memory.buffer;
  const want = packet(`want ${head} ${mode} ${band} no-progress\n`);
  const upload = Buffer.concat([want, Buffer.from("0000"), packet(`have ${head}\n`), packet("done\n")]);
  const input = frame(parts, "git-upload-pack", upload);
  const hash = format === 0 ? "sha1" : "sha256";
  const expected = call(exports, input, 1, format);
  assertNegotiatedResponse(expected, head, mode, band, hash);
  for (let round = 1; round < 32; round++) {
    const response = call(exports, input, 1, format);
    assertNegotiatedResponse(response, head, mode, band, hash);
    assert.deepEqual(response, expected);
  }
  assert.equal(exports.memory.buffer, memory);
}

test("negotiated ACK and sideband responses keep fixed Wasm memory in both formats", () => {
  const exports = instance();
  const memory = exports.memory.buffer;
  for (const format of [0, 1]) {
    const width = format === 0 ? 40 : 64;
    const discovery = call(exports, frame(parts, "info/refs?service=git-upload-pack"), 0, format);
    const head = new RegExp(`([0-9a-f]{${width}}) HEAD\\0`).exec(discovery.body.toString())[1];
    for (const mode of ["multi_ack", "multi_ack_detailed"]) {
      for (const band of ["side-band", "side-band-64k"]) {
        checkRepeatedNegotiation(exports, format, head, mode, band);
        assert.equal(exports.memory.buffer, memory);
      }
    }
  }
});

test("Wasm keeps fixed memory across repeated discovery, pack and loose-object requests", () => {
  const exports = instance();
  const memory = exports.memory.buffer;
  const discovery = call(exports, frame(parts, "info/refs?service=git-upload-pack"));
  assert.equal(discovery.status, 200);
  assert.equal(discovery.type, 1);
  const head = /([0-9a-f]{40}) HEAD\0/.exec(discovery.body.toString())[1];
  const upload = Buffer.concat([packet(`want ${head}\n`), Buffer.from("0000"), packet("done\n")]);
  const target = `objects/${head.slice(0, 2)}/${head.slice(2)}`;
  const expectedPack = call(exports, frame(parts, "git-upload-pack", upload), 1);
  const expectedLoose = call(exports, frame(parts, target));
  for (let round = 0; round < 128; round++) {
    assert.deepEqual(call(exports, frame(parts, "info/refs?service=git-upload-pack")), discovery);
    const pack = call(exports, frame(parts, "git-upload-pack", upload), 1);
    assert.equal(pack.status, 200);
    assert.equal(pack.type, 2);
    assert.equal(pack.body.subarray(0, 12).toString(), "0008NAK\nPACK");
    const bytes = pack.body.subarray(8);
    assert.deepEqual(createHash("sha1").update(bytes.subarray(0, -20)).digest(), bytes.subarray(-20));
    const loose = call(exports, frame(parts, target));
    assert.equal(loose.status, 200);
    assert.equal(loose.type, 3);
    assert.equal(createHash("sha1").update(inflateSync(loose.body)).digest("hex"), head);
    assert.deepEqual(pack, expectedPack);
    assert.deepEqual(loose, expectedLoose);
  }
  assert.equal(exports.memory.buffer, memory);
});

function checkSha256Objects(exports, head, commit, discovery, upload) {
  const target = `objects/${head.slice(0, 2)}/${head.slice(2)}`;
const pack = call(exports, frame(parts, "git-upload-pack", upload), 1, 1);
assert.equal(pack.status, 200);
const bytes = pack.body.subarray(8);
assert.deepEqual(createHash("sha256").update(bytes.subarray(0, -32)).digest(), bytes.subarray(-32));
const loose = call(exports, frame(parts, target), 0, 1);
assert.deepEqual(inflateSync(loose.body), Buffer.concat([Buffer.from(`commit ${commit.length}\0`), commit]));
assert.deepEqual(call(exports, frame(parts, "info/refs?service=git-upload-pack"), 0, 1), discovery);
}

test("SHA-256 Wasm snapshots match independent IDs and retain fixed memory", () => {
  const exports = instance();
  const oid = (kind, data) => createHash("sha256").update(Buffer.from(`${kind} ${data.length}\0`)).update(data).digest();
  const files = ["cf.json", "headers.json", "vars.json", "runtime.json"].map((name, index) => [name, Buffer.from(parts[index])]);
  files.sort(([left], [right]) => left < right ? -1 : 1);
  const tree = Buffer.concat(files.map(([name, data]) => Buffer.concat([Buffer.from(`100644 ${name}\0`), oid("blob", data)])));
  const commit = Buffer.from(`tree ${oid("tree", tree).toString("hex")}\nauthor git-zero <git-zero@example.invalid> 0 +0000\ncommitter git-zero <git-zero@example.invalid> 0 +0000\n\nIn-memory snapshot\n`);
  const expected = oid("commit", commit).toString("hex");
  const discovery = call(exports, frame(parts, "info/refs?service=git-upload-pack"), 0, 1);
  const head = /([0-9a-f]{64}) HEAD\0/.exec(discovery.body.toString())[1];
  assert.equal(head, expected);
  assert.ok(discovery.body.includes(Buffer.from("object-format=sha256")));
  for (const capability of ["", " object-format=sha256"]) {
    const upload = Buffer.concat([packet(`want ${head}${capability}\n`), Buffer.from("0000"), packet("done\n")]);
    for (let round = 0; round < 32; round++) {
      checkSha256Objects(exports, head, commit, discovery, upload);
    }
  }
  const mismatch = Buffer.concat([packet(`want ${head} object-format=sha1\n`), Buffer.from("0000"), packet("done\n")]);
  assert.equal(call(exports, frame(parts, "git-upload-pack", mismatch), 1, 1), -2);
  assert.equal(call(exports, frame(parts, "HEAD"), 0, 2), -2);
  assert.deepEqual(call(exports, frame(parts, "HEAD")), call(exports, frame(parts, "HEAD"), 0, 0));
});

test("Wasm serves a full input frame within its memory and response budgets", () => {
  const exports = instance();
  // Exercise the ABI's full 64 KiB capacity, independently of the JS snapshot limit.
  const target = "git-upload-pack";
  const headResponse = call(exports, frame(parts, "HEAD"));
  assert.equal(headResponse.status, 200);
  const discovery = call(exports, frame(parts, "info/refs?service=git-upload-pack"));
  const head = /([0-9a-f]{40}) HEAD\0/.exec(discovery.body.toString())[1];
  const upload = Buffer.concat([packet(`want ${head}\n`), Buffer.from("0000"), packet("done\n")]);
  // The ABI accepts a full frame on GET as well as on upload-pack.
  const padded = frame(parts, "HEAD", Buffer.alloc(INPUT_BYTES - frame(parts, "HEAD").length));
  assert.equal(padded.length, INPUT_BYTES);
  assert.deepEqual(call(exports, padded), headResponse);

  const largeParts = ["a".repeat(INPUT_BYTES - 20 - Buffer.byteLength(target) - upload.length - 3), "b", "c", "d"];
  const largeDiscovery = call(exports, frame(largeParts, "info/refs"));
  assert.equal(largeDiscovery.status, 200);
  const largeHead = largeDiscovery.body.toString().split("\t")[0];
  const largeUpload = Buffer.concat([packet(`want ${largeHead}\n`), Buffer.from("0000"), packet("done\n")]);
  const input = frame(largeParts, target, largeUpload);
  assert.equal(input.length, INPUT_BYTES);
  const response = call(exports, input, 1);
  assert.equal(response.status, 200);
  assert.equal(response.type, 2);
  assert.ok(response.body.length > 64 * 1024, "large pack path was not exercised");
  const bytes = response.body.subarray(8);
  assert.deepEqual(createHash("sha1").update(bytes.subarray(0, -20)).digest(), bytes.subarray(-20));
});

test("malformed ABI frames fail without growth and leave the instance usable", () => {
  const exports = instance();
  const memory = exports.memory.buffer;
  const valid = frame(parts, "HEAD");
  const expected = call(exports, valid);
  const invalid = [];
  for (let length = 0; length < valid.length; length++) invalid.push(valid.subarray(0, length));
  for (const offset of [0, 4, 8, 12, valid.length - 8]) {
    const input = Buffer.from(valid);
    input.writeUInt32LE(0xffffffff, offset);
    invalid.push(input);
  }
  invalid.push(frame(parts, Buffer.from([0xff])));
  for (const input of invalid) {
    assert.equal(call(exports, input), -2);
    assert.deepEqual(call(exports, valid), expected);
  }
  assert.equal(call(exports, valid, 2), -2);
  assert.equal(exports.serve(INPUT_BYTES + 1, 0), -1);
  assert.equal(exports.serve(0xffffffff, 0), -1);
  assert.equal(exports.memory.buffer, memory);
  assert.deepEqual(call(exports, valid), expected);
});

test("independent Wasm instances preserve their own buffers", () => {
  const first = instance();
  const second = instance();
  assert.notEqual(first.memory.buffer, second.memory.buffer);
  const original = call(first, frame(parts, "info/refs"));
  const firstOutput = Buffer.from(new Uint8Array(first.memory.buffer, first.output_ptr(), original.body.length + 3));
  const changed = call(second, frame(["changed", ...parts.slice(1)], "info/refs"));
  assert.notDeepEqual(changed.body, original.body);
  assert.deepEqual(Buffer.from(new Uint8Array(first.memory.buffer, first.output_ptr(), firstOutput.length)), firstOutput);
  assert.deepEqual(call(first, frame(parts, "info/refs")), original);
});


function streamingCall(exports, input, method, format, capacity) {
  const memory = exports.memory.buffer;
  new Uint8Array(memory, exports.input_ptr(), input.length).set(input);
  assert.equal(exports.stream_start(input.length, method, format), 3, `method=${method} format=${format} frame=${input.toString()}`);
  const metadata = Buffer.from(new Uint8Array(memory, exports.output_ptr(), 3));
  const chunks = [];
  assert.equal(exports.stream_read(0), -1);
  assert.equal(exports.stream_read(OUTPUT_BYTES + 1), -1);
  while (true) {
    const count = exports.stream_read(capacity);
    assert.ok(count >= 0 && count <= capacity);
    if (count === 0) break;
    chunks.push(Buffer.from(new Uint8Array(memory, exports.output_ptr(), count)));
  }
  assert.equal(exports.stream_read(capacity), 0);
  assert.equal(exports.memory.buffer, memory);
  return { status: metadata.readUInt16LE(0), type: metadata[2], body: Buffer.concat(chunks) };
}

function assertStreamMatches(actual, expected, sideband) {
  assert.equal(actual.status, expected.status);
  assert.equal(actual.type, expected.type);
  if (!sideband) {
    assert.deepEqual(actual, expected);
    return;
  }
  const marker = packet("packfile\n");
  const start = Math.max(0, expected.body.indexOf(marker));
  assert.deepEqual(actual.body.subarray(0, start), expected.body.subarray(0, start));
  assert.deepEqual(decodeNegotiation(actual.body.subarray(start), "side-band-64k"), decodeNegotiation(expected.body.subarray(start), "side-band-64k"));
}

function checkStreamingTarget(first, second, input, method, format, sideband, expected = call(first, input, method, format, true)) {
  for (const capacity of [1, 7, 1024, OUTPUT_BYTES]) {
    assertStreamMatches(streamingCall(first, input, method, format, capacity), expected, sideband);
  }
  new Uint8Array(first.memory.buffer, first.input_ptr(), input.length).set(input);
  assert.equal(first.stream_start(input.length, method, format), 3);
  assert.ok(first.stream_read(1) > 0);
  // Another instance can finish while the first is paused.
  streamingCall(second, input, method, format, 13);
  first.stream_cancel();
  assert.equal(first.stream_read(1), -2);
  assert.deepEqual(call(first, input, method, format, true), expected);
}

function checkStreamingVersion(first, second, format, version, head) {
  const body = fetchBody(head, format, version);
  const protocol = version === 0 ? "" : `version=${version}`;
  const cases = [
    ["info/refs?service=git-upload-pack", Buffer.alloc(0), 0],
    ["git-upload-pack", body, 1],
    [`objects/${head.slice(0,2)}/${head.slice(2)}`, Buffer.alloc(0), 0],
  ];
  for (const [target, requestBody, method] of cases) {
    checkStreamingTarget(first, second, httpFrame(target, requestBody, protocol), method, format, version === 2 && method === 1);
  }
}

test("pull ABI preserves responses, cancellation and independent instances without memory growth", () => {
  const first = instance();
  const second = instance();
  for (const format of [0, 1]) {
    const head = new RegExp(`([0-9a-f]{${format === 0 ? 40 : 64}}) HEAD\\0`).exec(call(first, frame(parts, "info/refs?service=git-upload-pack"), 0, format).body.toString())[1];
    for (const version of [0, 1, 2]) checkStreamingVersion(first, second, format, version, head);
  }
  assert.equal(first.stream_start(INPUT_BYTES + 1, 0, 0), -1);
  assert.equal(first.stream_read(1), -2);
  assert.equal(first.stream_start(0, 0, 0), -2);
  assert.equal(first.stream_read(1), -2);
});
