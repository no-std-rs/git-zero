const FETCH_AUTH = "x-git-zero-internal-fetch-auth";
const FETCH_BODY_MAX = 128 * 1024;
const META_MAX = 15e5, BULK_MAX = 16 * 1024 * 1024 + 2060, HEADER = 20;
const FIRST_READ_CAPABILITY = { version: 1, body_bytes: FETCH_BODY_MAX, frame_bytes: HEADER + META_MAX + FETCH_BODY_MAX + BULK_MAX };
const encoder = new TextEncoder();
const HEX = /^[0-9a-f]{64}$/;
function fail(message) {
  throw Object.assign(new Error(message), { status: 502 });
}
function hex(data) {
  return Array.from(data, (b) => b.toString(16).padStart(2, "0")).join("");
}
function validCount(n) {
  return Number.isSafeInteger(n) && n >= 0;
}
function plain(v) {
  return v !== null && typeof v === "object" && !Array.isArray(v);
}
function exact(v, names) {
  return plain(v) && Object.keys(v).length === names.length && names.every((k) => Object.hasOwn(v, k));
}
function utf8compare(a, b) {
  const x = encoder.encode(a), y = encoder.encode(b);
  for (let i = 0; i < Math.min(x.length, y.length); i++) {
    if (x[i] !== y[i]) return x[i] - y[i];
  }
  return x.length - y.length;
}
function joined(parts) {
  const size = parts.reduce((n, p) => n + p.length, 0);
  const out = new Uint8Array(size);
  let offset = 0;
  for (const p of parts) {
    out.set(p, offset);
    offset += p.length;
  }
  return out;
}
function u64(n) {
  if (!validCount(n)) fail("invalid canonical integer");
  const out = new Uint8Array(8);
  new DataView(out.buffer).setBigUint64(0, BigInt(n), true);
  return out;
}
function u32(n) {
  if (!validCount(n) || n > 4294967295) fail("invalid canonical u32");
  const out = new Uint8Array(4);
  new DataView(out.buffer).setUint32(0, n, true);
  return out;
}
function field(s) {
  const data = typeof s === "string" ? encoder.encode(s) : s;
  return [u64(data.length), data];
}
async function digest(parts) {
  return hex(new Uint8Array(await crypto.subtle.digest("SHA-256", joined(parts))));
}
async function manifestFingerprint(m) {
  if (!plain(m) || !validCount(m.generation) || !plain(m.refs) || !Array.isArray(m.packs) || !Array.isArray(m.garbage ?? [])) fail("invalid canonical manifest");
  const parts = [encoder.encode("git-zero-fetch-manifest-v1"), u64(m.generation), u64(m.packs.length)];
  for (const p of m.packs) {
    parts.push(...field(p.id), u64(p.pack_bytes), u64(p.index_bytes), u32(p.level));
    for (const chunks of [p.pack, p.index, p.bundle ?? []]) {
      if (!Array.isArray(chunks)) fail("invalid canonical chunks");
      parts.push(u64(chunks.length));
      for (const c of chunks) parts.push(...field(c.key), u64(c.len));
    }
  }
  const refs = Object.keys(m.refs).sort(utf8compare);
  parts.push(u64(refs.length));
  for (const name of refs) parts.push(...field(name), ...field(m.refs[name]));
  const garbage = m.garbage ?? [];
  parts.push(u64(garbage.length));
  for (const key of garbage) parts.push(...field(key));
  return digest(parts);
}
function normalized(headers) {
  return [...headers].filter(([n]) => !["host", "connection", "content-length", "transfer-encoding", "accept-encoding", "cf-worker", "cf-connecting-ip", FETCH_AUTH].includes(n)).sort(([a], [b]) => utf8compare(a, b));
}
async function requestFingerprint(logical, body) {
  const headers = normalized(logical.headers);
  const parts = [encoder.encode("git-zero-fetch-request-v1"), ...field(logical.method), ...field(logical.target), u64(headers.length)];
  for (const [n, v] of headers) parts.push(...field(n), ...field(v));
  parts.push(...field(body));
  return digest(parts);
}
async function registrationFirstRead(session) {
  const raw = crypto.getRandomValues(new Uint8Array(32));
  const key = await crypto.subtle.importKey("raw", raw, { name: "HMAC", hash: "SHA-256" }, false, ["sign", "verify"]);
  return { private: { key, session }, response: { ...FIRST_READ_CAPABILITY, key: hex(raw), session } };
}
function eligibleFirstRead(request, target, registration, logicalHeaders) {
  const n = request.headers.get("content-length");
  const receive = target === "/repo.git/git-receive-pack" && registration?.firstRead?.receive;
  if (!registration?.firstRead || request.method !== "POST" || (!receive && target !== "/repo.git/git-upload-pack") || request.headers.get("content-type") !== (receive ? "application/x-git-receive-pack-request" : "application/x-git-upload-pack-request") || (receive && (!([null,"","version=0"].includes(request.headers.get("git-protocol"))) || ![null,"identity"].includes(request.headers.get("content-encoding")?.toLowerCase() ?? null))) || n === null || !/^\d+$/.test(n) || Number(n) <= 0 || Number(n) > FETCH_BODY_MAX || request.headers.has("transfer-encoding")) return false;
  const logical = [...logicalHeaders];
  let total = target.length + 16;
  if (logical.length > 256 || logical.some(([name,value]) => {
    const line = encoder.encode(name).length + encoder.encode(value).length + 4;
    total += line;
    return line > 8192 || !/^[a-z0-9!#$%&'*+.^_`|~-]+$/.test(name)
      || [...value].some(c => (c.charCodeAt(0) < 32 && c !== "\t") || c.charCodeAt(0) === 127);
  }) || total > 65536) return false;
  return true;
}
function frame(meta, body = new Uint8Array(), bulk = new Uint8Array()) {
  const data = encoder.encode(JSON.stringify(meta));
  if (data.length > META_MAX || body.length > FETCH_BODY_MAX || bulk.length > BULK_MAX) fail("private frame exceeds bounds");
  const header = new Uint8Array(HEADER);
  header.set([71, 90, 70, 82]);
  const d = new DataView(header.buffer);
  d.setUint32(4, 1, true);
  d.setUint32(8, data.length, true);
  d.setUint32(12, body.length, true);
  d.setUint32(16, bulk.length, true);
  return joined([header, data, body, bulk]);
}
function parseFrame(data, strictJson) {
  if (data.length < HEADER || data.length > FIRST_READ_CAPABILITY.frame_bytes || data[0] !== 71 || data[1] !== 90 || data[2] !== 70 || data[3] !== 82) fail("invalid private frame");
  const d = new DataView(data.buffer, data.byteOffset, data.byteLength);
  const version = d.getUint32(4, true), meta = d.getUint32(8, true), body = d.getUint32(12, true), bulk = d.getUint32(16, true);
  if (version !== 1 || meta > META_MAX || body > FETCH_BODY_MAX || bulk > BULK_MAX || HEADER + meta + body + bulk !== data.length) fail("invalid private frame lengths");
  return { meta: strictJson(data.subarray(HEADER, HEADER + meta)), body: data.subarray(HEADER + meta, HEADER + meta + body), bulk: data.subarray(HEADER + meta + body) };
}
async function auth(config, domain, data) {
  return hex(new Uint8Array(await crypto.subtle.sign("HMAC", config.key, joined([encoder.encode(domain), new Uint8Array([0]), data]))));
}
async function verify(config, domain, data, value) {
  if (typeof value !== "string" || !HEX.test(value)) fail("missing private response authenticator");
  const signature = Uint8Array.from(value.match(/../g), (h) => parseInt(h, 16));
  if (!await crypto.subtle.verify("HMAC", config.key, signature, joined([encoder.encode(domain), new Uint8Array([0]), data]))) fail("invalid private response authenticator");
}
async function bounded(response, maximum) {
  if (!response.body) return new Uint8Array();
  const reader = response.body.getReader();
  const parts = [];
  let size = 0;
  for (; ; ) {
    const item = await reader.read();
    if (item.done) break;
    size += item.value.length;
    if (size > maximum) {
      await reader.cancel();
      fail("private response too large");
    }
    parts.push(item.value);
  }
  return joined(parts);
}
async function gatewayFirstRead({ repo, target, headers, body, registration, current, dispatch, witness, strictJson }) {
  const config = registration.firstRead;
  const check = () => {
    if (!current()) fail("registration changed during private fetch");
  };
  const nonce = hex(crypto.getRandomValues(new Uint8Array(16)));
  const logical = { method: "POST", target, headers: [...headers].sort(([a], [b]) => utf8compare(a, b)) };
  const base = { version: 1, session: config.session, nonce, repo, request: logical };
  const requestHash = await requestFingerprint(logical, body);
  check();
  const planning = frame(base, body);
  const signature = await auth(config, "git-zero-fetch-plan-request-v1", planning);
  check();
  const response = await fetch(new Request(registration.origin + "/_git_zero_internal/fetch-plan", { method: "POST", headers: { "content-type": "application/octet-stream", [FETCH_AUTH]: signature }, body: planning, redirect: "manual" }));
  check();
  if (!response.ok) fail("Native fetch planning failed");
  const raw = await bounded(response, META_MAX + HEADER);
  check();
  await verify(config, "git-zero-fetch-plan-response-v1", raw, response.headers.get(FETCH_AUTH));
  check();
  const decoded = parseFrame(raw, strictJson), reply = decoded.meta;
  if (decoded.body.length || decoded.bulk.length || !exact(reply, ["version", "session", "nonce", "request_hash", "status", "plan"]) || reply.version !== 1 || reply.session !== config.session || reply.nonce !== nonce || reply.request_hash !== requestHash) fail("Native fetch plan identity mismatch");
  if (reply.status === "unsupported" && reply.plan === null) return null;
  const plan = reply.plan;
  if (reply.status !== "supported" || !exact(plan, ["generation", "manifest", "selection", "first"]) || !validCount(plan.generation) || !HEX.test(plan.manifest) || !HEX.test(plan.selection)) fail("invalid Native fetch plan");
  validateFirstBatch(plan.first);
  const authority = await dispatch(new Request("https://local/_services/manifest/first-read", { method: "POST", headers: { "x-hosting-repo": repo, "x-hosting-phase": target === "/repo.git/git-receive-pack" ? "push" : "fetch", "content-type": "application/json" }, body: JSON.stringify({ version: 1, generation: plan.generation, manifest: plan.manifest, chunks: plan.first }) }));
  check();
  if (!authority.ok) fail("authority plus first read failed");
  const generationText = authority.headers.get("x-git-zero-internal-first-generation"), matchedText = authority.headers.get("x-git-zero-internal-first-matched");
  if (!generationText || !/^\d+$/.test(generationText) || !validCount(Number(generationText)) || !["0", "1"].includes(matchedText)) fail("invalid first read authority reply");
  const generation = Number(generationText), matched = matchedText === "1";
  if (matched ? generation !== plan.generation : generation === plan.generation) fail("inconsistent authority generation");
  const bulk = await bounded(authority, BULK_MAX);
  check();
  if (!matched && bulk.length) fail("mismatching authority returned object bytes");
  const proof = await witness(generation);
  check();
  const continuation = frame({ ...base, request_hash: requestHash, plan, generation, matched, witness: proof }, body, bulk);
  const continuationAuth = await auth(config, "git-zero-fetch-continue-v1", continuation);
  check();
  const result = await fetch(new Request(registration.origin + "/_git_zero_internal/fetch-continue", { method: "POST", headers: { "content-type": "application/octet-stream", [FETCH_AUTH]: continuationAuth }, body: continuation, redirect: "manual" }));
  check();
  return result;
}
function validateFirstBatch(chunks) {
  if (!Array.isArray(chunks) || chunks.length === 0 || chunks.length > 256) fail("invalid first batch shape");
  let payload = 0;
  const sizes = /* @__PURE__ */ new Map();
  for (const c of chunks) {
    if (!exact(c, ["key", "len"]) || !/^chunks\/[0-9a-f]{64}$/.test(c.key) || !validCount(c.len) || c.len === 0 || c.len > 16 * 1024 * 1024 || sizes.has(c.key) && sizes.get(c.key) !== c.len) fail("invalid first batch chunk");
    sizes.set(c.key, c.len);
    payload += c.len;
    if (payload > 16 * 1024 * 1024) fail("first batch payload too large");
  }
  return 12 + 8 * chunks.length + payload;
}
export {
  FETCH_AUTH,
  FETCH_BODY_MAX,
  FIRST_READ_CAPABILITY,
  eligibleFirstRead,
  gatewayFirstRead,
  manifestFingerprint,
  registrationFirstRead,
  validateFirstBatch
};
