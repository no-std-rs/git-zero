import test from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { randomBytes } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { fixture } from "./review4-fixture.mjs";
import { object, entry, pack, receiveReport } from "./fixture.mjs";
import { receive } from "./browse-fixture.mjs";
import { PackCache } from "../src/pack-cache.mjs";

test("real Git followers see a remote retry error after a shared build fails mid-stream", { timeout: 10000 }, async () => {
  const f = fixture(), repo = f.repo("repo"), dir = await mkdtemp(join(tmpdir(), "git-zero-cache-failure-")); let server;
  try {
    const created = await repo.fetch(f.request("repo", "api/v1/repository", "admin", { method: "PUT", headers: { "content-type": "application/json" }, body: "{}" })); assert.equal(created.status, 201);
    const blob = object(0, 3, randomBytes(512 * 1024)), tree = object(0, 2, Buffer.concat([Buffer.from("100644 data\0"), blob.id]));
    const commit = object(0, 1, `tree ${tree.id.toString("hex")}\nauthor A <a@b> 0 +0000\ncommitter A <a@b> 0 +0000\n\nfixture\n`);
    const pushed = await repo.fetch(f.request("repo", "git-receive-pack", "write", { method: "POST", headers: { "content-type": "application/x-git-receive-pack-request" }, body: receive(0, [{ name: "refs/heads/main", old: "0".repeat(40), new: commit.id.toString("hex") }], pack(0, [blob, tree, commit].map((o) => entry(o.kind, o.body)))) })); assert.doesNotMatch(receiveReport(await pushed.arrayBuffer()), /ng /);
    const cache = repo.packCache = new PackCache(repo.store, repo.env), read = cache.read.bind(cache), put = repo.store.put.bind(repo.store);
    let firstRead, writes = 0; const started = new Promise((resolve) => { firstRead = resolve; });
    cache.read = async function* (...args) { for await (const part of read(...args)) { firstRead(); yield part; } };
    repo.store.put = async (key, ...args) => { if (key.includes("/fetch-cache/") && ++writes === 2) { await started; throw new Error("injected cache PUT failure"); } return put(key, ...args); };
    server = createServer(async (incoming, outgoing) => {
      try {
        const route = incoming.url.slice("/repo.git/".length), headers = {};
        for (const name of ["content-type", "git-protocol", "content-encoding"]) if (incoming.headers[name]) headers[name] = incoming.headers[name];
        const response = await repo.fetch(f.request("repo", route, "read", { method: incoming.method, headers, ...(incoming.method === "POST" ? { body: incoming, duplex: "half" } : {}) }));
        outgoing.writeHead(response.status, Object.fromEntries(response.headers));
        for await (const part of response.body) { if (!outgoing.write(part)) await once(outgoing, "drain"); } outgoing.end();
      } catch (error) { outgoing.destroy(error); }
    });
    server.listen(0, "127.0.0.1"); await once(server, "listening");
    const child = spawn("git", ["-c", "protocol.version=2", "clone", `http://127.0.0.1:${server.address().port}/repo.git`, join(dir, "clone")], { env: { ...process.env, GIT_TERMINAL_PROMPT: "0" }, stdio: ["ignore", "ignore", "pipe"] });
    let stderr = ""; child.stderr.on("data", (part) => { stderr += part; }); const [code] = await once(child, "exit");
    assert.notEqual(code, 0); assert.match(stderr, /remote:.*shared pack generation failed; retry fetch/); assert.doesNotMatch(stderr, /invalid pkt|bad line length|unexpected disconnect/);
    await repo.queue; assert.equal(cache.metrics.builds, 1); assert.equal(repo.env.BYTE_BUDGET.owners.size, 0);
  } finally { server?.closeAllConnections(); if (server) await new Promise((resolve) => server.close(resolve)); await repo.queue; f.close(); await rm(dir, { recursive: true, force: true }); }
});
