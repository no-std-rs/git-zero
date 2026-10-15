import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";
import { promisify } from "node:util";
import { test } from "node:test";
import { createWorker } from "../src/worker.mjs";

const execute = promisify(execFile);
const module = await WebAssembly.compile(await readFile(new URL("../build/git_zero_worker_wasm.wasm", import.meta.url)));

for (const version of [0, 1, 2]) {
  test(`real Git relatively deepens a Worker root boundary over v${version}`, async () => {
    const worker = createWorker(module);
    const server = createServer(async (incoming, outgoing) => {
      try {
        const chunks = [];
        for await (const chunk of incoming) chunks.push(chunk);
        const init = { method: incoming.method, headers: incoming.headers };
        if (incoming.method === "POST") init.body = Buffer.concat(chunks);
        const response = await worker.fetch(new Request(`http://127.0.0.1:${server.address().port}${incoming.url}`, init));
        outgoing.writeHead(response.status, Object.fromEntries(response.headers));
        await pipeline(Readable.fromWeb(response.body), outgoing);
      } catch (error) {
        if (outgoing.headersSent) outgoing.destroy(error);
        else { outgoing.writeHead(500); outgoing.end(String(error)); }
      }
    });
    const directory = await mkdtemp(join(tmpdir(), "git-zero-relative-"));
    await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
    const env = {
      ...Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith("GIT_"))),
      GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_TERMINAL_PROMPT: "0",
    };
    const git = (...args) => execute("git", ["-c", `protocol.version=${version}`, ...args], { env, timeout: 30000 });
    try {
      const origin = `http://127.0.0.1:${server.address().port}`;
      const snapshot = await (await worker.fetch(new Request(origin))).json();
      await git("clone", "-q", "--depth=1", "--no-tags", snapshot.clone_url, directory);
      assert.equal((await git("-C", directory, "rev-parse", "--is-shallow-repository")).stdout.trim(), "true");
      await git("-C", directory, "fetch", "--deepen=1", "--no-tags", "origin");
      assert.equal((await git("-C", directory, "rev-parse", "--is-shallow-repository")).stdout.trim(), "false");
      assert.equal((await git("-C", directory, "rev-list", "--count", "HEAD")).stdout.trim(), "1");
      await git("-C", directory, "fetch", "--deepen=1", "--no-tags", "origin");
      assert.equal((await git("-C", directory, "rev-parse", "--is-shallow-repository")).stdout.trim(), "false");
      await git("-C", directory, "fsck", "--strict");
    } finally {
      server.closeAllConnections();
      await new Promise((resolve) => server.close(resolve));
      await rm(directory, { recursive: true, force: true });
    }
  });
}
