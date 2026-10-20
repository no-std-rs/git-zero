// Independent S3 signature verifier and durable opaque-byte fixture. This is
// test storage only; production workerd connects directly to the real R2 API.
import assert from "node:assert/strict";
import { createHash, createHmac, timingSafeEqual } from "node:crypto";
import { mkdir, readFile, rename, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { TOKEN } from "./wrangler.mjs";

const ACCESS = "git-zero-test-access", SECRET = "git-zero-test-secret";
const root = new URL("../", import.meta.url);
const digest = (bytes) => createHash("sha256").update(bytes).digest("hex");
const hmac = (key, bytes) => createHmac("sha256", key).update(bytes).digest();

async function s3(directory, getFailure, deleteFailures) {
  await mkdir(directory, { recursive: true });
  const faults = { injected: 0, deleteFailures: 0 };
  const server = createServer(async (request, response) => {
    try {
      const parts = []; let length = 0;
      for await (const part of request) { length += part.length; assert.ok(length <= 8 * 1024 * 1024); parts.push(part); }
      const body = Buffer.concat(parts);
      const date = request.headers["x-amz-date"], payload = request.headers["x-amz-content-sha256"];
      assert.equal(payload, digest(body));
      const auth = /^AWS4-HMAC-SHA256 Credential=([^/]+)\/(\d{8})\/auto\/s3\/aws4_request, SignedHeaders=([^,]+), Signature=([0-9a-f]{64})$/.exec(request.headers.authorization ?? "");
      assert.ok(auth); assert.equal(auth[1], ACCESS); assert.equal(auth[2], date.slice(0, 8));
      assert.equal(auth[3], "host;x-amz-content-sha256;x-amz-date");
      const canonical = [request.method, request.url, "", `host:${request.headers.host}\nx-amz-content-sha256:${payload}\nx-amz-date:${date}\n`, auth[3], payload].join("\n");
      let key = hmac(`AWS4${SECRET}`, auth[2]);
      for (const value of ["auto", "s3", "aws4_request"]) key = hmac(key, value);
      const expected = hmac(key, `AWS4-HMAC-SHA256\n${date}\n${auth[2]}/auto/s3/aws4_request\n${digest(canonical)}`);
      assert.ok(timingSafeEqual(expected, Buffer.from(auth[4], "hex")));
      const path = join(directory, digest(request.url));
      if (request.method === "PUT") {
        const temporary = `${path}.pending`; await writeFile(temporary, body); await rename(temporary, path);
        response.writeHead(200); response.end();
      } else if (request.method === "DELETE") {
        if (faults.deleteFailures < deleteFailures) { faults.deleteFailures++; response.writeHead(503); response.end(); return; }
        await rm(path, { force: true }); response.writeHead(204); response.end();
      } else if (request.method === "GET") {
        try {
          const value = await readFile(path);
          if (getFailure && faults.injected === 0) {
            faults.injected++;
            if (getFailure === "status") { response.writeHead(503); response.end(); return; }
            if (getFailure === "body") {
              response.writeHead(200, { "Content-Length": value.length });
              response.write(value.subarray(0, Math.floor(value.length / 2)));
              response.flushHeaders(); setImmediate(() => response.destroy()); return;
            }
            request.socket.destroy(); return;
          }
          response.writeHead(200, { "Content-Length": value.length }); response.end(value);
        }
        catch (error) { if (error.code !== "ENOENT") throw error; response.writeHead(404); response.end(); }
      } else { response.writeHead(405); response.end(); }
    } catch (error) { response.writeHead(403); response.end(String(error.message)); }
  });
  server.listen(0, "127.0.0.1"); await once(server, "listening");
  return { faults, url: `http://127.0.0.1:${server.address().port}/bucket`, stop: async () => { server.closeAllConnections(); await new Promise((resolve) => server.close(resolve)); } };
}

export async function startStandalone(persist, { format = 0, cwd = fileURLToPath(root), getFailure, deleteFailures = 0 } = {}) {
  const state = join(persist, "sqlite"); await mkdir(state, { recursive: true });
  const storage = await s3(join(persist, "r2"), getFailure, deleteFailures);
  const child = spawn(fileURLToPath(new URL("node_modules/workerd/bin/workerd", root)), [
    "serve", "workerd.capnp", "--experimental", "--directory-path", `state=${state}`,
    "--socket-addr", "http=127.0.0.1:0", "--control-fd", "1",
  ], { cwd, env: { ...process.env, HOST_TOKEN: TOKEN, TEST_MODE: "true", REPO_FORMAT: String(format), R2_S3_URL: storage.url, R2_ACCESS_KEY_ID: ACCESS, R2_SECRET_ACCESS_KEY: SECRET }, stdio: ["ignore", "pipe", "pipe"] });
  let logs = "", exited = false;
  child.once("exit", () => { exited = true; });
  const stop = async (signal = "SIGTERM") => {
    try {
      if (!exited) {
        const done = once(child, "exit"); child.kill(signal);
        const timer = setTimeout(() => child.kill("SIGKILL"), 5000);
        try { await done; } finally { clearTimeout(timer); }
      }
    } finally { await storage.stop(); }
  };
  try {
    const port = await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error(`workerd startup timeout\n${logs}`)), 30000);
      child.stdout.on("data", (chunk) => {
        logs = (logs + chunk).slice(-20000);
        for (const line of logs.split("\n")) {
          try { const event = JSON.parse(line); if (event.event === "listen" && event.socket === "http") { clearTimeout(timer); resolve(event.port); } } catch {}
        }
      });
      child.stderr.on("data", (chunk) => { logs = (logs + chunk).slice(-20000); });
      child.once("error", (error) => { clearTimeout(timer); reject(error); });
      child.once("exit", (code) => { clearTimeout(timer); reject(new Error(`workerd exited ${code}\n${logs}`)); });
    });
    return { origin: `http://127.0.0.1:${port}`, stop, crash: () => stop("SIGKILL"), logs: () => logs, runtimePid: child.pid, storageFaults: storage.faults };
  } catch (error) { await stop(); throw error; }
}
