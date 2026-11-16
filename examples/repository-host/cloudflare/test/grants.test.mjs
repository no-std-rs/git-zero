import test from "node:test";
import assert from "node:assert/strict";
import { authorize, verifyGrant, accessReady } from "../src/access.mjs";
import { gateway, OwnRepository } from "./host.mjs";

import { GRANT_KEY, mint } from "./grant-fixture.mjs";
const env = { AUTH_MODE: "gateway", GRANT_KEY };
const request = (namespace, grant, suffix = "/info/refs?service=git-upload-pack", init = {}) => new Request(`http://host/${namespace}.git${suffix}`, { ...init, headers: { ...init.headers, ...(grant === null ? {} : { "x-git-zero-grant": grant }) } });

test("versioned grant vector and bounded opaque UTF-8 actor", async () => {
  const vector = mint("550e8400-e29b-41d4-a716-446655440000", "write", { expiry: 2000000120, actor: "user:42" });
  assert.equal(vector, "v1.550e8400-e29b-41d4-a716-446655440000.write.2000000120.dXNlcjo0Mg.uFYwfIrtliIzA2H2Ypu6AZQJnjSJp4otWEMqop0kku0");
  assert.deepEqual(await verifyGrant(vector, GRANT_KEY, "550e8400-e29b-41d4-a716-446655440000", 2000000000), { access: "write", actor: "user:42", expires: 2000000120 });
  for (const actor of ["", "\ufeffopaque", "☃".repeat(42), "x".repeat(128), "\nopaque\0label"]) assert.equal((await verifyGrant(mint("repo", "read", { actor }), GRANT_KEY, "repo")).actor, actor);
  assert.equal(await verifyGrant(mint("repo", "read", { actor: "x".repeat(129) }), GRANT_KEY, "repo"), null);
});

test("invalid grants fail closed: expiry, encoding, key, scope, tampering, upgrade", async () => {
  const now = Math.floor(Date.now() / 1000), valid = mint("repo");
  const tamperedMac = valid.slice(0, -43) + (valid.at(-43) === "A" ? "B" : "A") + valid.slice(-42);
  const invalid = [
    null, "", "x".repeat(513), valid + ".extra", valid.replace("v1.", "v2."),
    valid.replace(".read.", ".write."), valid.replace(".read.", ".admin."),
    valid.replace(Buffer.from("fixture-user").toString("base64url"), Buffer.from("other-user").toString("base64url")),
    tamperedMac, mint("repo", "read", { expiry: now }), mint("repo", "read", { expiry: now - 1 }),
    mint("repo", "read", { expiry: now + 301 }), mint("other"), mint("repo", "owner"), mint("repo", "toString"),
    mint("repo", "read", { actor: Buffer.from([255]) }), mint("repo", "read", { expiry: "02000000120" }),
    mint("repo", "read", { secret: "wrong-key" }), valid.replace(/\.[^.]+$/, ".%%%"),
    valid.replace(/\.[^.]+$/, "."), valid.replace(/\.([^.]+)\.([^.]+)$/, ".$1=.$2"),
  ];
  for (const value of invalid) assert.equal(await verifyGrant(value, GRANT_KEY, "repo", now), null, String(value));
  assert.equal(await verifyGrant(valid, "short", "repo"), null);
  for (const AUTH_MODE of ["gateway", "typo"]) assert.equal(accessReady({ AUTH_MODE, HOST_TOKEN: "configured" }), false);
});

test("read is refused before parsing every receive transport variant; admin diagnostics stay private", async () => {
  for (const access of ["read", "write", "admin"]) {
    for (const protocol of ["version=0", "version=1", "version=2", "invalid"])
      for (const coding of ["identity", "gzip", "x-gzip", "invalid"])
        for (const suffix of ["/info/refs?service=git-receive-pack", "/git-receive-pack"]) {
          const req = request("repo", mint("repo", access), suffix, { method: suffix.includes("info") ? "GET" : "POST", headers: { "git-protocol": protocol, "content-encoding": coding } });
          const result = await authorize(req, env, "repo");
          if (access === "read") { assert.ok(result instanceof Response); assert.match(await result.text(), /ERR write access required/); }
          else assert.equal(result.access, access);
        }
    const diagnostic = new Request("http://host/_test/repo/intake", { method: "POST", headers: { "x-git-zero-grant": mint("repo", access) } });
    const result = await authorize(diagnostic, env, "repo");
    assert.equal(result instanceof Response ? result.status : result.access, access === "admin" ? "admin" : 403);
  }
});

test("Worker and Durable Object independently verify, before any storage; no gateway challenges", async () => {
  for (const grant of [null, mint("other"), mint("repo").replace(".read.", ".write.")]) {
    const req = request("repo", grant);
    const response = await gateway.fetch(req, env);
    assert.equal(response.status, 404); assert.equal(response.headers.has("www-authenticate"), false);
    assert.deepEqual(await response.json(), { error: "unknown route" });
    const headers = new Headers(req.headers); headers.set("x-git-zero-namespace", "repo");
    const repository = new OwnRepository({}, env);
    assert.equal((await repository.fetch(new Request(req, { headers }))).status, 404);
  }
  const tokenFallback = await gateway.fetch(request("repo", null, undefined, { headers: { authorization: "Bearer fixture-token" } }), { ...env, HOST_TOKEN: "fixture-token" });
  assert.equal(tokenFallback.status, 404);
  assert.equal(tokenFallback.headers.has("www-authenticate"), false);
  const repository = new OwnRepository({}, env);
  const req = request("repo", mint("repo"), "/git-receive-pack", { method: "POST", headers: { "x-git-zero-namespace": "repo" } });
  assert.match(await (await repository.fetch(req)).text(), /ERR write access required/);
  const mismatch = request("repo", mint("other"), "/info/refs?service=git-upload-pack", { headers: { "x-git-zero-namespace": "other" } });
  assert.equal((await repository.fetch(mismatch)).status, 404);
});
