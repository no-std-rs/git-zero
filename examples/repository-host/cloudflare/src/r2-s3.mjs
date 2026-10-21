// Opaque storage adapter for the same Wasm host running directly in workerd.
// Cloudflare deployments continue to use their native R2 binding. S3 requests
// use AWS Signature V4; Git bytes and object metadata are never interpreted here.
import { hex } from "./codec.mjs";
const encode = (value) => new TextEncoder().encode(value);
const hash = async (bytes) => hex(new Uint8Array(await crypto.subtle.digest("SHA-256", bytes)));
const hmac = async (key, value) => {
  const imported = await crypto.subtle.importKey("raw", key, { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  return new Uint8Array(await crypto.subtle.sign("HMAC", imported, encode(value)));
};

export class R2S3Bucket {
  constructor(env) {
    this.url = new URL(env.R2_S3_URL);
    if (this.url.search || this.url.hash || this.url.username || this.url.password
      || !(this.url.protocol === "https:" || (this.url.protocol === "http:" && ["127.0.0.1", "localhost", "[::1]"].includes(this.url.hostname)))) throw new Error("invalid R2 S3 bucket URL");
    if (!env.R2_ACCESS_KEY_ID || !env.R2_SECRET_ACCESS_KEY) throw new Error("R2 S3 credentials are missing");
    this.accessKey = env.R2_ACCESS_KEY_ID; this.secret = env.R2_SECRET_ACCESS_KEY;
    this.signing = null;
  }
  async signingKey(date) {
    if (this.signing?.date === date) return this.signing.key;
    let key = await hmac(encode(`AWS4${this.secret}`), date);
    for (const value of ["auto", "s3", "aws4_request"]) key = await hmac(key, value);
    this.signing = { date, key }; return key;
  }
  async request(method, key, data = null, consume = null) {
    const url = new URL(this.url);
    url.pathname = `${url.pathname.replace(/\/$/, "")}/${key.split("/").map(encodeURIComponent).join("/")}`;
    const time = new Date().toISOString().replace(/[:-]|\.\d{3}/g, ""), date = time.slice(0, 8);
    const digest = await hash(data ?? new Uint8Array());
    const signed = "host;x-amz-content-sha256;x-amz-date";
    const canonical = [method, url.pathname, "", `host:${url.host}\nx-amz-content-sha256:${digest}\nx-amz-date:${time}\n`, signed, digest].join("\n");
    const scope = `${date}/auto/s3/aws4_request`;
    const signature = hex(await hmac(await this.signingKey(date), `AWS4-HMAC-SHA256\n${time}\n${scope}\n${await hash(encode(canonical))}`));
    const controller = new AbortController();
    const timeout = new Error("R2 S3 request deadline exceeded");
    timeout.retryable = true;
    const timer = setTimeout(() => controller.abort(timeout), 30000);
    try {
      const response = await fetch(url, { method, body: data, redirect: "manual", signal: controller.signal, headers: {
        "x-amz-date": time, "x-amz-content-sha256": digest,
        authorization: `AWS4-HMAC-SHA256 Credential=${this.accessKey}/${scope}, SignedHeaders=${signed}, Signature=${signature}`,
      } });
      if (!response.ok && !(method === "GET" && response.status === 404)) {
        await response.body?.cancel();
        const error = new Error(`R2 S3 ${method} failed (${response.status})`);
        error.retryable = response.status === 408 || response.status === 429 || response.status >= 500;
        throw error;
      }
      return consume ? await consume(response) : await response.body?.cancel();
    } catch (error) {
      if (controller.signal.aborted) throw timeout;
      throw error;
    } finally { clearTimeout(timer); }
  }
  async put(key, data) { await this.request("PUT", key, data); }
  async get(key) {
    return this.request("GET", key, null, async (response) => {
      if (response.status === 404) { await response.body?.cancel(); return null; }
      const size = Number(response.headers.get("content-length"));
      if (!Number.isSafeInteger(size) || size < 0 || size > 8 * 1024 * 1024 || !response.headers.has("content-length")) { await response.body?.cancel(); throw new Error("R2 S3 response size is absent or invalid"); }
      const data = await response.arrayBuffer();
      return { size, arrayBuffer: async () => data };
    });
  }
  async delete(keys) {
    for (const key of typeof keys === "string" ? [keys] : keys) await this.request("DELETE", key);
  }
}

export function bucket(env) {
  if (env.BLOBS) return env.BLOBS;
  if (!env.R2_S3_URL) throw new Error("R2 binding or S3 bucket URL is required");
  return new R2S3Bucket(env);
}
