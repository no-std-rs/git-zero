import { ApiError, encode } from "./api.mjs";

const b64 = (data) => btoa(String.fromCharCode(...data)).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
const decode = (data) => Uint8Array.from(atob(data.replaceAll("-", "+").replaceAll("_", "/")), (c) => c.charCodeAt(0));

// Epochs survive object eviction, but never a fresh repository incarnation.
// HKDF domain-separates these public capabilities from gateway grant MACs.
export class Proofs {
  constructor(store, generation) {
    this.store = store; this.generation = generation;
    const state = store.exec("SELECT epoch,proof_floor FROM repository_security WHERE id=1")[0];
    this.epoch = state.epoch; this.floor = state.proof_floor;
  }
  async key() {
    if (!this.store.proofKey) this.store.proofKey = (async () => {
      const env = this.store.env;
      const key = await crypto.subtle.importKey("raw", encode(env.AUTH_MODE === "gateway" ? env.GRANT_KEY : env.HOST_TOKEN), "HKDF", false, ["deriveKey"]);
      return crypto.subtle.deriveKey({ name: "HKDF", hash: "SHA-256", salt: encode(this.epoch), info: encode(JSON.stringify(["git-zero-browse-v3", env.SIGNING_CONTEXT ?? "", env.KEY_PREFIX ?? "", this.store.namespace])) }, key, { name: "HMAC", hash: "SHA-256", length: 256 }, false, ["sign", "verify"]);
    })();
    return this.store.proofKey;
  }
  async sign(op, value) {
    const payload = b64(encode(JSON.stringify({ ...value, v: 3, op, namespace: this.store.namespace, epoch: this.epoch, generation: this.generation })));
    return payload + "." + b64(new Uint8Array(await crypto.subtle.sign("HMAC", await this.key(), encode("git-zero-browse-v3\n" + payload))));
  }
  async verify(op, token) {
    if (!token) return null;
    if (token.length > 2048 || !/^[A-Za-z0-9_-]+\.[A-Za-z0-9_-]{43}$/.test(token)) throw new ApiError("invalid proof or cursor");
    let value;
    try {
      const [payload, signature] = token.split("."), raw = decode(payload), mac = decode(signature);
      if (b64(raw) !== payload || b64(mac) !== signature || !await crypto.subtle.verify("HMAC", await this.key(), mac, encode("git-zero-browse-v3\n" + payload))) throw new Error();
      value = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(raw));
    } catch { throw new ApiError("invalid proof or cursor"); }
    if (value?.v !== 3 || value.op !== op || value.namespace !== this.store.namespace || value.epoch !== this.epoch || !Number.isSafeInteger(value.generation) || value.generation < 0) throw new ApiError("invalid proof or cursor");
    if (value.generation > this.generation || (op === "object" && value.generation < this.floor) || (!["object", "changes"].includes(op) && value.generation !== this.generation)) throw new ApiError("proof or cursor is stale after ref publication", 409);
    return value;
  }
}
