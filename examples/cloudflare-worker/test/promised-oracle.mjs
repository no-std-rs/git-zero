// Independent Git oracle shared by the native and Worker policy hosts.
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { lstat, mkdir, readFile, readlink, readdir } from "node:fs/promises";
import { join, basename, dirname } from "node:path";
import { withServer } from "./http.mjs";
import { gzipSync } from "node:zlib";

export const binary = process.env.GIT_ZERO_TEST_GIT ?? "git";
export function environment(format) {
  return { ...Object.fromEntries(Object.entries(process.env).filter(([name]) => !name.startsWith("GIT_"))),
    GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_TERMINAL_PROMPT: "0", GIT_DEFAULT_HASH: format };
}
export function command(program, args, data = null, env = process.env) {
  return new Promise((resolve, reject) => {
    const child = execFile(program, args, { env, encoding: "buffer", timeout: 60000, maxBuffer: 8 * 1024 * 1024 },
      (error, stdout, stderr) => error ? reject(Object.assign(error, { stdout, stderr })) : resolve({ stdout, stderr }));
    child.stdin.on("error", () => {}); child.stdin.end(data);
  });
}
export const git = (format, ...args) => command(binary, args, null, environment(format));
export function packet(text) { return Buffer.from(`${(Buffer.byteLength(text) + 4).toString(16).padStart(4, "0")}${text}`); }
export function fetchBody(format, version, ids, filter = null) {
  if (version === 2) return Buffer.concat([packet("command=fetch\n"), packet(`object-format=${format}\n`), Buffer.from("0001"),
    ...ids.map(id => packet(`want ${id}\n`)), ...(filter ? [packet(`filter ${filter}\n`)] : []), packet("done\n"), Buffer.from("0000")]);
  return Buffer.concat([...ids.map((id, index) => packet(`want ${id}${index === 0 ? ` object-format=${format}${filter ? " filter" : ""}` : ""}\n`)),
    ...(filter ? [packet(`filter ${filter}\n`)] : []), Buffer.from("0000"), packet("done\n")]);
}
export async function withGitServer(repository, callback) {
  await git(repository.format, "-C", repository.source, "config", "uploadpack.allowFilter", "true");
  await git(repository.format, "-C", repository.source, "config", "uploadpack.allowReachableSHA1InWant", "true");
  await withServer(async request => {
    const url = new URL(request.url);
    const result = await command(binary, ["http-backend"], Buffer.from(await request.arrayBuffer()), {
      ...environment(repository.format), GIT_PROJECT_ROOT: dirname(repository.source), GIT_HTTP_EXPORT_ALL: "1",
      PATH_INFO: `/${basename(repository.source)}${url.pathname.replace(/^\/repo.git/, "")}`,
      REQUEST_METHOD: request.method, QUERY_STRING: url.search.slice(1),
      CONTENT_TYPE: request.headers.get("content-type") ?? "", CONTENT_LENGTH: request.headers.get("content-length") ?? "",
      HTTP_GIT_PROTOCOL: request.headers.get("git-protocol") ?? "",
    });
    const split = result.stdout.indexOf("\r\n\r\n"); assert.ok(split >= 0);
    const headers = new Headers(); let status = 200;
    for (const line of result.stdout.subarray(0, split).toString().split("\r\n")) {
      const colon = line.indexOf(":"); const name = line.slice(0, colon); const value = line.slice(colon + 1).trim();
      if (name.toLowerCase() === "status") status = Number(value.split(" ")[0]); else headers.append(name, value);
    }
    return new Response(result.stdout.subarray(split + 4), { status, headers });
  }, callback);
}
async function physical(repository, directory) {
  return (await git(repository.format, "-C", directory, "cat-file", "--batch-all-objects", "--batch-check=%(objectname)")).stdout.toString().trim().split("\n").filter(Boolean).sort();
}
async function checkBodies(repository, directory) {
  const ids = await physical(repository, directory); const expected = new Map(repository.objects.map(value => [value.id, value]));
  for (const id of ids) {
    const value = expected.get(id); assert.ok(value, `unexpected physical object ${id}`);
    assert.deepEqual((await git(repository.format, "-C", directory, "cat-file", value.kind, id)).stdout, value.body);
  }
  return ids;
}
async function checkCheckout(repository, directory, revision) {
  const expected = (await git(repository.format, "-C", repository.source, "ls-tree", "-rz", revision)).stdout;
  assert.deepEqual((await git(repository.format, "-C", directory, "ls-tree", "-rz", "HEAD")).stdout, expected);
  for (const record of expected.toString().split("\0").filter(Boolean)) {
    const [metadata, name] = record.split("\t"); const [mode, kind, id] = metadata.split(" "); assert.equal(kind, "blob");
    const value = repository.objects.find(value => value.id === id); const path = join(directory, name);
    if (mode === "120000") assert.equal(await readlink(path), value.body.toString());
    else { assert.deepEqual(await readFile(path), value.body); assert.equal(Boolean((await lstat(path)).mode & 0o111), mode === "100755"); }
  }
  await git(repository.format, "-C", directory, "fsck", "--strict"); return checkBodies(repository, directory);
}
function decodePack(wire, version) {
  const parts = [];
  while (wire.length) {
    if (wire.subarray(0, 4).toString() === "PACK") { parts.push(wire); break; }
    const size = Number.parseInt(wire.subarray(0, 4).toString(), 16); assert.ok(Number.isInteger(size) && size >= 0 && size <= 65520);
    if (size < 4) { wire = wire.subarray(4); continue; }
    const data = wire.subarray(4, size); wire = wire.subarray(size); if (version === 2 && data[0] === 1) parts.push(data.subarray(1));
  }
  return Buffer.concat(parts);
}
async function selected(repository, url, root, version, id, index, checkGzip) {
  const kind = repository.objects.find(value => value.id === id).kind;
  const response = await fetch(`${url}/repo.git/git-upload-pack`, { method: "POST", headers: {
    "content-type": "application/x-git-upload-pack-request", "git-protocol": `version=${version}`,
  }, body: fetchBody(repository.format, version, [id], kind === "blob" ? "blob:none" : null) });
  assert.equal(response.status, 200); const wire = Buffer.from(await response.arrayBuffer());
  if (checkGzip && kind === "blob") {
    const headers = { "content-type": "application/x-git-upload-pack-request", "git-protocol": `version=${version}`, "content-encoding": "gzip" };
    const compressed = await fetch(`${url}/repo.git/git-upload-pack`, { method: "POST", headers,
      body: gzipSync(fetchBody(repository.format, version, [id], "blob:none")) });
    assert.equal(compressed.status, 200); assert.deepEqual(Buffer.from(await compressed.arrayBuffer()), wire, "gzip must yield the same independently verified pack");
    for (const ids of [[id, "7".repeat(id.length)], ["7".repeat(id.length), id]]) {
      const denied = await fetch(`${url}/repo.git/git-upload-pack`, { method: "POST", headers, body: gzipSync(fetchBody(repository.format, version, ids)) });
      assert.equal(denied.status, 400); assert.ok(!(await denied.text()).includes("PACK"));
    }
  }
  const pack = decodePack(wire, version);
  assert.equal(pack.subarray(0, 4).toString(), "PACK"); const directory = join(root, `selected-${version}-${index}`);
  await git(repository.format, "init", "-q", "--bare", `--object-format=${repository.format}`, directory);
  await command(binary, ["-C", directory, "index-pack", "--stdin", "--strict"], pack, environment(repository.format));
  const ids = await checkBodies(repository, directory);
  const closure = (await git(repository.format, "-C", repository.source, "rev-list", "--objects", "--no-object-names", id)).stdout.toString().trim().split("\n");
  assert.deepEqual(ids, closure.sort(), `exact selected ${kind} payload`);
  return { kind, objects: ids.length };
}
export async function exercisePromised(repository, url, root, checkGzip = false) {
  await mkdir(root, { recursive: true }); const rows = [];
  const hiddenTag = repository.objects.find(value => value.kind === "tag" && !repository.refs.some(ref => ref.id === value.id)); assert.ok(hiddenTag);
  const old = (await git(repository.format, "-C", repository.source, "rev-parse", `${repository.mainHistory}~1`)).stdout.toString().trim();
  const oldTree = (await git(repository.format, "-C", repository.source, "rev-parse", `${old}^{tree}`)).stdout.toString().trim();
  const binaryBlob = repository.objects.find(value => value.kind === "blob" && value.body.length === 98304);
  for (const version of [0, 1, 2]) {
    const directory = join(root, `clone-${version}`); const trace = join(root, `trace-${version}`);
    const client = (...args) => command(binary, ["-c", `protocol.version=${version}`, ...args], null, { ...environment(repository.format), GIT_TRACE_PACKET: trace });
    await client("clone", "-q", "--filter=blob:none", "--no-checkout", `${url}/repo.git`, directory);
    const initial = await physical(repository, directory);
    assert.deepEqual(initial, repository.objects.filter(value => value.kind !== "blob").map(value => value.id).sort(), "initial filter must omit exactly all blobs");
    assert.equal((await client("-C", directory, "config", "remote.origin.promisor")).stdout.toString().trim(), "true");
    const absent = await command(binary, ["-C", directory, "cat-file", "--batch-check"], Buffer.from(`${binaryBlob.id}\n`), { ...environment(repository.format), GIT_NO_LAZY_FETCH: "1" });
    assert.equal(absent.stdout.toString(), `${binaryBlob.id} missing\n`);
    assert.ok((await readdir(join(directory, ".git/objects/pack"))).some(name => name.endsWith(".promisor")));
    await client("-C", directory, "checkout", "-q", "--force", "HEAD"); const afterCheckout = await checkCheckout(repository, directory, repository.head.id);
    const firstTrace = await readFile(trace, "utf8"); assert.ok(firstTrace.includes(`want ${binaryBlob.id}`), "ordinary checkout must issue the omitted blob want");
    const wants = [...firstTrace.matchAll(/> want ([0-9a-f]+)/g)].map(match => match[1]);
    assert.ok(wants.filter(id => repository.objects.find(value => value.id === id)?.kind === "blob").length >= 2, "checkout must retrieve multiple missing blobs");
    await client("-C", directory, "checkout", "-q", "--force", `${repository.mainHistory}~7`); await checkCheckout(repository, directory, `${repository.mainHistory}~7`);
    await client("-C", directory, "checkout", "-q", "--force", "origin/side"); await checkCheckout(repository, directory, "refs/heads/side");
    for (const limit of [98304, 98305]) {
      const filtered = join(root, `limit-${version}-${limit}`); await client("clone", "-q", `--filter=blob:limit=${limit}`, "--no-checkout", `${url}/repo.git`, filtered);
      assert.deepEqual(await physical(repository, filtered), repository.objects.filter(value => value.kind !== "blob" || value.body.length < limit).map(value => value.id).sort());
    }
    const selectedKinds = [];
    for (const [index, id] of [old, oldTree, binaryBlob.id, hiddenTag.id].entries()) selectedKinds.push(await selected(repository, url, root, version, id, index, checkGzip));
    rows.push({ format: repository.format, version, initialObjects: initial.length, checkoutObjects: afterCheckout.length, omittedBinary: binaryBlob.id, selectedKinds });
  }
  return rows;
}
