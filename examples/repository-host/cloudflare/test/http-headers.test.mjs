import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { packet, project, receiveReport, wire } from "./fixture.mjs";
import { headers, start, TOKEN } from "./wrangler.mjs";
import { startStandalone } from "./standalone.mjs";

for (const [runtime, serve] of [["Wrangler R2/SQLite", start], ["direct workerd with S3/SQLite", startStandalone]]) test(`${runtime} preserves HTTP token and credential casing rules`, { timeout: 60000 }, async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "git-zero-http-headers-"));
  let server;
  try {
    server = await serve(directory);
    for (const format of [0, 1]) {
      const namespace = `headers-${format}-${randomUUID()}`, remote = `${server.origin}/${namespace}.git`;
      const initialized = await fetch(`${server.origin}/_test/${namespace}/initialize`, { method: "POST", headers: headers(format) });
      assert.equal(initialized.status, 200); await initialized.arrayBuffer();
      await t.test(`authentication format=${format}`, async () => {
        const basic = Buffer.from(`git:${TOKEN}`).toString("base64");
        for (const authorization of [`Bearer ${TOKEN}`, `bearer ${TOKEN}`, `bEaReR ${TOKEN}`, `Basic ${basic}`, `basic ${basic}`, `bAsIc ${basic}`]) {
          const response = await fetch(`${remote}/info/refs?service=git-upload-pack`, { headers: { authorization } });
          assert.equal(response.status, 200, authorization.split(" ")[0]); await response.arrayBuffer();
        }
        for (const authorization of [`bearer ${TOKEN.toUpperCase()}`, `basic ${Buffer.from(`git:${TOKEN.toUpperCase()}`).toString("base64")}`, "Basic invalid", "Bearer"]) {
          const response = await fetch(`${remote}/info/refs?service=git-upload-pack`, { headers: { authorization } });
          assert.equal(response.status, 401, "credentials remain case-sensitive"); await response.arrayBuffer();
        }
      });
      await t.test(`media types format=${format}`, async () => {
        const fixture = project(format);
        for (const [index, type] of ["application/x-git-receive-pack-request", "Application/X-Git-Receive-Pack-Request", "APPLICATION/X-GIT-RECEIVE-PACK-REQUEST", "Application/X-Git-Receive-Pack-Request ; charset=UTF-8"].entries()) {
          const name = `refs/heads/type-${index}`;
          const response = await fetch(`${remote}/git-receive-pack`, { method: "POST", headers: { ...headers(), "content-type": type }, body: wire(format, fixture.pack, fixture.root.id, name) });
          assert.equal(response.status, 200, type);
          const report = receiveReport(await response.arrayBuffer());
          assert.ok(report.includes(`ok ${name}`), `${type}: ${report}`);
        }
        const control = Buffer.concat([packet(`want ${fixture.root.id.toString("hex")} object-format=${format ? "sha256" : "sha1"}\n`), Buffer.from("0000"), packet("done\n")]);
        for (const type of ["application/x-git-upload-pack-request", "Application/X-Git-Upload-Pack-Request", "APPLICATION/X-GIT-UPLOAD-PACK-REQUEST", "Application/X-Git-Upload-Pack-Request ; charset=UTF-8"]) {
          const response = await fetch(`${remote}/git-upload-pack`, { method: "POST", headers: { ...headers(), "content-type": type }, body: control });
          assert.equal(response.status, 200, type); assert.ok(Buffer.from(await response.arrayBuffer()).includes(Buffer.from("PACK")));
        }
        for (const service of ["git-receive-pack", "git-upload-pack"]) {
          const response = await fetch(`${remote}/${service}`, { method: "POST", headers: { ...headers(), "content-type": "application/json" }, body: "{}" });
          assert.equal(response.status, 404); await response.arrayBuffer();
        }
      });
    }
  } finally { await server?.stop(); await rm(directory, { recursive: true, force: true }); }
});
