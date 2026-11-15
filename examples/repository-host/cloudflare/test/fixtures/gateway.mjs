// Local acceptance fixture, deliberately no product identity/storage dependencies.
const users = { owner: { password: "owner-development", access: "admin" }, reader: { password: "reader-development", access: "read" }, outsider: { password: "outsider-development" } };
const encode = (value) => new TextEncoder().encode(value);
const b64 = (value) => btoa(value).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
export default {
  async fetch(request, env) {
    const hidden = () => Response.json({ error: "unknown route" }, { status: 404, headers: { "Cache-Control": "no-store" } });
    let name, password;
    try {
      const decoded = atob(request.headers.get("authorization")?.replace(/^Basic /i, "") ?? ""), split = decoded.indexOf(":");
      if (split >= 0) { name = decoded.slice(0, split); password = decoded.slice(split + 1); }
    } catch {}
    const user = Object.hasOwn(users, name ?? "") ? users[name] : null;
    if (!user || user.password !== password) return new Response("gateway authentication required", { status: 401, headers: { "WWW-Authenticate": 'Basic realm="fixture gateway"' } });
    const url = new URL(request.url), match = /^\/alpha\/analysis\.git(\/.*)?$/.exec(url.pathname);
    if (!match || !user.access) return hidden();
    const namespace = b64("alpha/analysis"), expiry = Math.floor(Date.now() / 1000) + 120, actor = b64(name);
    const canonical = `git-zero-grant-v1\n${namespace}\n${user.access}\n${expiry}\n${actor}\n`;
    const key = await crypto.subtle.importKey("raw", encode(env.GRANT_KEY), { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
    const mac = b64(String.fromCharCode(...new Uint8Array(await crypto.subtle.sign("HMAC", key, encode(canonical)))));
    url.pathname = `/${namespace}.git${match[1] ?? ""}`;
    const headers = new Headers(request.headers);
    headers.delete("authorization"); headers.delete("x-git-zero-namespace"); headers.delete("x-git-zero-grant");
    headers.set("x-git-zero-grant", `v1.${namespace}.${user.access}.${expiry}.${actor}.${mac}`);
    // Request cloning preserves protocol selectors, encodings and streamed bodies.
    return env.HOST.fetch(new Request(url, new Request(request, { headers })));
  },
};
