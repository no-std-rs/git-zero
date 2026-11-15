// A tiny JS stand-in for a workers-rs public router. Its default path calls the
// stub directly: the host's fetch entry is deliberately bypassed.
import codec from "../../build/git_zero_host_wasm.wasm";
import { createRepositoryHost } from "../../src/host.mjs";
const host = createRepositoryHost({
  codec, bindings: { repositories: "PRODUCT_REPOS", objects: "PRODUCT_BUCKET", grantKey: "PRODUCT_KEY" },
  authMode: "gateway", routePrefix: "/internal/git", keyPrefix: "product/git/",
  memory: { canonicalCacheBytes: 2 * 1024 * 1024, representationCacheBytes: 1024 * 1024, chunkCacheBytes: 2 * 1024 * 1024, cacheEntries: 128, canonicalCaptureBytes: 1024 * 1024, representationCaptureBytes: 256 * 1024, chunkBytes: 65536 },
  readLimits: { pageSize: 8, blobBytes: 1024 * 1024, pathDepth: 16 },
});
export const ProductRepository = host.Repository;
const namespaces = { sha1: "550e8400-e29b-41d4-a716-446655440001", sha256: "550e8400-e29b-41d4-a716-446655440002" };
const foreign = ["outside-host/object", "product/git-neighbor/object", "product/git/repositories/another/object"];
const users = { owner: "admin", reader: "read", outsider: null };
const b64 = (value) => btoa(value).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
const encode = (value) => new TextEncoder().encode(value);
async function grant(env, namespace, access, actor) {
  const expiry = Math.floor(Date.now() / 1000) + 120, actor64 = b64(actor);
  const message = `git-zero-grant-v1\n${namespace}\n${access}\n${expiry}\n${actor64}\n`;
  const key = await crypto.subtle.importKey("raw", encode(env.PRODUCT_KEY), { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  const mac = b64(String.fromCharCode(...new Uint8Array(await crypto.subtle.sign("HMAC", key, encode(message)))));
  return `v1.${namespace}.${access}.${expiry}.${actor64}.${mac}`;
}
export default { async fetch(request, env) {
  const hidden = () => Response.json({ error: "unknown route" }, { status: 404 });
  const url = new URL(request.url);
  if (url.pathname === "/health") return new Response("product health");
  let user;
  try { const value = atob(request.headers.get("authorization")?.replace(/^Basic /i, "") ?? ""); const split = value.indexOf(":"); const name = value.slice(0, split); if (split >= 0 && Object.hasOwn(users, name) && value.slice(split + 1) === `${name}-development`) user = name; } catch {}
  if (!user) return new Response("product authentication required", { status: 401, headers: { "WWW-Authenticate": 'Basic realm="embedded fixture"' } });
  if (url.pathname === "/_fixture/foreign" && user === "owner") {
    if (request.method === "PUT") for (const key of foreign) await env.PRODUCT_BUCKET.put(key, encode("foreign survives"));
    return Response.json(await Promise.all(foreign.map(async (key) => ({ key, value: await (await env.PRODUCT_BUCKET.get(key))?.text() ?? null }))));
  }
  if (url.pathname === "/_fixture/storage" && user === "owner") return Response.json((await env.PRODUCT_BUCKET.list({ limit: 1000 })).objects.map(({ key, size }) => ({ key, size })));
  let namespace, suffix;
  const wrong = url.pathname === "/_fixture/wrong-stub" && user === "owner";
  if (wrong) { namespace = "another-valid-namespace"; suffix = "/api/v1/repository"; }
  else { const match = /^\/alpha\/analysis-(sha1|sha256)\.git(\/.*)?$/.exec(url.pathname); if (!match || !users[user]) return hidden(); namespace = namespaces[match[1]]; suffix = match[2] ?? ""; }
  url.pathname = `/internal/git/${namespace}.git${suffix}`;
  const headers = new Headers(request.headers); headers.delete("authorization"); headers.delete("x-git-zero-grant"); headers.set("x-git-zero-namespace", namespace);
  headers.set("x-git-zero-grant", await grant(env, namespace, users[user], user));
  const forwarded = new Request(url, new Request(request, { headers }));
  if (url.searchParams.get("via") === "handler") return host.fetch(forwarded, env);
  return env.PRODUCT_REPOS.get(env.PRODUCT_REPOS.idFromName(wrong ? namespaces.sha1 : namespace)).fetch(forwarded);
} };
