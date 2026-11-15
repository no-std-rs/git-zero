// No identity model lives here: a gateway grants one namespace and access level.
export const REPOSITORY = /^[A-Za-z0-9_-]{1,80}$/;
const encode = (value) => new TextEncoder().encode(value);
const levels = { read: 1, write: 2, admin: 3 };
const hidden = () => Response.json({ error: "unknown route" }, { status: 404, headers: { "Cache-Control": "no-store" } });
const key = (secret, usages) => crypto.subtle.importKey("raw", encode(secret), { name: "HMAC", hash: "SHA-256" }, false, usages);
const base64url = (bytes) => btoa(String.fromCharCode(...bytes)).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
function decode(value) {
  if (!/^[A-Za-z0-9_-]*$/.test(value)) throw new Error("invalid base64url");
  const bytes = Uint8Array.from(atob(value.replaceAll("-", "+").replaceAll("_", "/")), (c) => c.charCodeAt(0));
  if (base64url(bytes) !== value) throw new Error("noncanonical base64url");
  return bytes;
}
export function canonicalGrant(namespace, access, expiry, actor) {
  return `git-zero-grant-v1\n${namespace}\n${access}\n${expiry}\n${actor}\n`;
}

// HMAC verification delegates the fixed-size secret comparison to WebCrypto,
// rather than a JavaScript string/byte comparison. The comparison key is public.
async function equalSecret(expected, candidate) {
  const comparison = await key("git-zero-token-comparison-v1", ["sign", "verify"]);
  const signature = await crypto.subtle.sign("HMAC", comparison, encode(expected));
  return crypto.subtle.verify("HMAC", comparison, signature, encode(candidate));
}
async function token(request, env) {
  if (!env.HOST_TOKEN) return false;
  const authorization = /^([^ ]+) +(.+)$/.exec(request.headers.get("authorization") ?? "");
  if (!authorization) return false;
  const scheme = authorization[1].toLowerCase(), credentials = authorization[2];
  if (scheme === "bearer") return equalSecret(env.HOST_TOKEN, credentials);
  if (scheme !== "basic") return false;
  try {
    const decoded = atob(credentials), split = decoded.indexOf(":");
    return split >= 0 && await equalSecret(env.HOST_TOKEN, decoded.slice(split + 1));
  } catch { return false; }
}

export function accessReady(env) {
  if (env.AUTH_MODE === undefined || env.AUTH_MODE === "token") return !!env.HOST_TOKEN;
  return env.AUTH_MODE === "gateway" && typeof env.GRANT_KEY === "string" && encode(env.GRANT_KEY).length >= 32;
}
export async function verifyGrant(value, secret, namespace, now = Math.floor(Date.now() / 1000)) {
  if (typeof value !== "string" || value.length > 512 || typeof secret !== "string" || encode(secret).length < 32 || !REPOSITORY.test(namespace ?? "")) return null;
  const parts = value.split(".");
  if (parts.length !== 6) return null;
  const [version, scope, access, expiry, actor, mac] = parts;
  if (version !== "v1" || scope !== namespace || !Object.hasOwn(levels, access) || !/^[1-9][0-9]{0,9}$/.test(expiry)) return null;
  const expires = Number(expiry);
  if (expires <= now || expires > now + 300) return null;
  try {
    const label = decode(actor), signature = decode(mac);
    if (label.length > 128 || signature.length !== 32) return null;
    const actorLabel = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(label);
    const valid = await crypto.subtle.verify("HMAC", await key(secret, ["verify"]), signature, encode(canonicalGrant(scope, access, expiry, actor)));
    return valid ? { access, actor: actorLabel, expires } : null;
  } catch { return null; }
}

export function requiredAccess(request) {
  const url = new URL(request.url), parts = url.pathname.split("/");
  if (parts[1] === "_test" || (parts[2] === "api" && parts[3] === "v1")) return "admin";
  if (parts[2] === "git-receive-pack" || url.searchParams.getAll("service").includes("git-receive-pack")) return "write";
  if (parts[2] === "git-upload-pack" || url.searchParams.get("service") === "git-upload-pack") return "read";
  return "admin";
}

export async function authorize(request, env, namespace) {
  if (env.AUTH_MODE === undefined || env.AUTH_MODE === "token") {
    if (await token(request, env)) return { access: "admin", actor: "" };
    return new Response("authentication required", { status: 401, headers: { "WWW-Authenticate": 'Basic realm="git-zero"' } });
  }
  if (env.AUTH_MODE !== "gateway") return hidden();
  const grant = await verifyGrant(request.headers.get("x-git-zero-grant"), env.GRANT_KEY, namespace);
  if (!grant) return hidden();
  const required = requiredAccess(request);
  if (levels[grant.access] >= levels[required]) return grant;
  if (required === "write") {
    const packet = (value) => `${(encode(value).length + 4).toString(16).padStart(4, "0")}${value}`;
    const discovery = request.method === "GET";
    return new Response(`${discovery ? packet("# service=git-receive-pack\n") + "0000" : ""}${packet("ERR write access required; ask your gateway administrator\n")}`, {
      headers: { "Content-Type": `application/x-git-receive-pack-${discovery ? "advertisement" : "result"}`, "Cache-Control": "no-store" },
    });
  }
  return Response.json({ error: "admin access required" }, { status: 403, headers: { "Cache-Control": "no-store" } });
}
