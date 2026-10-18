// Opaque, bounded transport proof. This HMAC key is never returned to Native.
export const PROOF_HEADER = "x-git-zero-retirement-proof";
export const DEFERRED_HEADER = "x-git-zero-internal-catalog-deferred";
export const CONFIRM_HEADER = "x-git-zero-internal-retirement-confirmation";
export const DEFERRED_CAPABILITY = Object.freeze({ version: 1, proof_bytes: 178 });
const DOMAIN = "git-zero-retirement-absence";
const encoder = new TextEncoder();
const hex = (bytes) => Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
const unhex = (value) => Uint8Array.from(value.match(/../g), (pair) => Number.parseInt(pair, 16));

export async function debtDigest(keys) {
  if (!Array.isArray(keys) || keys.length > 256 || new Set(keys).size !== keys.length
      || keys.some((key) => typeof key !== "string" || !/^chunks\/[0-9a-f]{64}$/.test(key))) throw new Error("invalid ordered retirement debt");
  return hex(new Uint8Array(await crypto.subtle.digest("SHA-256", encoder.encode(JSON.stringify(keys)))));
}

export async function registrationProof() {
  const key = await crypto.subtle.importKey("raw", crypto.getRandomValues(new Uint8Array(32)),
    { name: "HMAC", hash: "SHA-256" }, false, ["sign", "verify"]);
  return { key, session: hex(crypto.getRandomValues(new Uint8Array(16))) };
}

const message = (repo, session, generation, digest) => encoder.encode(JSON.stringify([DOMAIN, 1, repo, session, generation, digest]));

export async function issueProof(config, repo, context, reply, receipt) {
  const keys = context.keys, retirement = reply.retirement;
  const known = receipt && receipt.totals.do_rows_read !== null && receipt.totals.do_rows_written !== null;
  const deletes = receipt?.operations.r2_delete;
  const cas = receipt?.operations.manifest_cas;
  if (!keys.length || !known || reply.version !== 1 || reply.published !== true
      || !retirement || retirement.generation !== context.generation || !Array.isArray(retirement.outcomes)
      || retirement.outcomes.length !== keys.length
      || retirement.outcomes.some((outcome, index) => !outcome || Object.keys(outcome).length !== 2
        || outcome.key !== keys[index] || outcome.deleted !== true)
      || !deletes || deletes.attempts !== keys.length || deletes.successes !== keys.length || deletes.failures !== 0
      || !cas || cas.attempts !== 1 || cas.successes !== 1 || cas.failures !== 0
      || Object.values(receipt.operations).some((operation) => operation.failures !== 0)) return null;
  const fields = context.uploaded ? ["version", "uploaded", "published", "retirement"] : ["version", "published", "retirement"];
  if (Object.keys(reply).length !== fields.length || fields.some((field) => !Object.hasOwn(reply, field))
      || Object.keys(retirement).length !== 2 || !Object.hasOwn(retirement, "generation") || !Object.hasOwn(retirement, "outcomes")) return null;
  if (context.uploaded) {
    const put = receipt.operations.r2_put;
    if (!reply.uploaded || Object.keys(reply.uploaded).length !== 2
        || reply.uploaded.key !== context.uploaded.key || reply.uploaded.len !== context.uploaded.len
        || !put || put.attempts !== 1 || put.successes !== 1 || put.failures !== 0
        || put.written_bytes !== context.uploaded.len) return null;
  } else if (receipt.operations.r2_put?.attempts) return null;
  const digest = await debtDigest(keys), data = new Uint8Array(89);
  data[0] = 1; data.set(unhex(config.session), 1);
  new DataView(data.buffer).setBigUint64(17, BigInt(context.generation), true);
  data.set(unhex(digest), 25);
  data.set(new Uint8Array(await crypto.subtle.sign("HMAC", config.key,
    message(repo, config.session, context.generation, digest))), 57);
  return hex(data);
}

export async function verifyProof(config, repo, encoded) {
  if (!config || typeof encoded !== "string" || !/^01[0-9a-f]{176}$/.test(encoded)) return null;
  const data = unhex(encoded), session = hex(data.subarray(1, 17));
  const generation = new DataView(data.buffer).getBigUint64(17, true);
  if (session !== config.session || generation > BigInt(Number.MAX_SAFE_INTEGER)) return null;
  const digest = hex(data.subarray(25, 57));
  if (!await crypto.subtle.verify("HMAC", config.key, data.subarray(57), message(repo, session, Number(generation), digest))) return null;
  return JSON.stringify([1, Number(generation), digest]);
}
