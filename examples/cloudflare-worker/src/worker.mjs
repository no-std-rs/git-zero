const encoder = new TextEncoder();
const decoder = new TextDecoder("utf-8", { fatal: true });
const MAX_SNAPSHOT_BYTES = 8192;
const MAX_TOKEN_LENGTH = Math.ceil(MAX_SNAPSHOT_BYTES / 3) * 4;
const CONTENT_TYPES = [
  "text/plain; charset=utf-8",
  "application/x-git-upload-pack-advertisement",
  "application/x-git-upload-pack-result",
  "application/x-git-loose-object",
];

function encodeToken(bytes) {
  return btoa(String.fromCharCode(...bytes)).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

function decodeToken(token) {
  if (token.length > MAX_TOKEN_LENGTH || !/^[A-Za-z0-9_-]+$/.test(token)) throw new Error("Invalid snapshot token");
  const base64 = token.replaceAll("-", "+").replaceAll("_", "/");
  const bytes = Uint8Array.from(atob(base64), (character) => character.charCodeAt(0));
  if (bytes.length > MAX_SNAPSHOT_BYTES) throw new Error("Snapshot exceeds 8 KiB");
  const snapshot = JSON.parse(decoder.decode(bytes));
  if (!Array.isArray(snapshot) || snapshot.length !== 4 || snapshot.some((part) => typeof part !== "string")) {
    throw new Error("Invalid snapshot data");
  }
  return snapshot;
}

function capture(request, env) {
  // Cloudflare does not expose the distinction between a string var and a secret
  // at runtime. Only explicitly named public bindings are included in this demo.
  const vars = {};
  for (const name of String(env.EXPOSED_VARS ?? "").split(",").map((name) => name.trim()).filter(Boolean)) {
    const value = env[name];
    if (value !== undefined && (value === null || typeof value !== "object" || Object.getPrototypeOf(value) === Object.prototype || Array.isArray(value))) {
      vars[name] = value;
    }
  }
  const headers = Object.fromEntries([...request.headers].filter(([name]) => name.startsWith("cf-")));
  const json = (value) => `${JSON.stringify(value, null, 2)}\n`;
  return [
    json(request.cf ?? {}), json(headers), json(vars),
    json({ platform: "Cloudflare Workers", format: 1, history: "one root commit; no parents" }),
  ];
}

function invoke(module, parts, target, body, method) {
  // Each synchronous call owns an instance, so overlapping fetches cannot share
  // Rust's fixed input/output arrays. The compiled module itself is reusable.
  const { exports } = new WebAssembly.Instance(module, {});
  const encoded = parts.map((part) => encoder.encode(part));
  const targetBytes = encoder.encode(target);
  const length = 16 + encoded.reduce((total, part) => total + part.length, 0) + 4 + targetBytes.length + body.length;
  if (length > exports.input_capacity()) return new Response("Request exceeds Wasm capacity\n", { status: 413 });
  const input = new Uint8Array(exports.memory.buffer, exports.input_ptr(), length);
  const view = new DataView(input.buffer, input.byteOffset, input.byteLength);
  encoded.forEach((part, index) => view.setUint32(index * 4, part.length, true));
  let offset = 16;
  for (const part of encoded) {
    input.set(part, offset);
    offset += part.length;
  }
  view.setUint32(offset, targetBytes.length, true);
  offset += 4;
  input.set(targetBytes, offset);
  offset += targetBytes.length;
  input.set(body, offset);
  const result = exports.serve(length, method === "POST" ? 1 : 0);
  if (result < 0) return new Response("Invalid or oversized Git request\n", { status: result === -1 ? 413 : 400 });
  const output = new Uint8Array(exports.memory.buffer, exports.output_ptr(), result);
  const status = new DataView(output.buffer, output.byteOffset, result).getUint16(0, true);
  return new Response(output.slice(3), {
    status,
    headers: { "Content-Type": CONTENT_TYPES[output[2]], "Cache-Control": "no-store" },
  });
}

async function readBody(request, maximum) {
  const declared = request.headers.get("content-length");
  if (declared !== null && (!/^\d+$/.test(declared) || Number(declared) > maximum)) throw new Error("Request too large");
  if (!request.body) return new Uint8Array();
  const reader = request.body.getReader();
  const storage = new Uint8Array(maximum);
  let length = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      if (length + value.length > maximum) throw new Error("Request too large");
      storage.set(value, length);
      length += value.length;
    }
  } catch (error) {
    await reader.cancel();
    throw error;
  } finally {
    reader.releaseLock();
  }
  return storage.subarray(0, length);
}

export function createWorker(module) {
  return {
    async fetch(request, env = {}) {
      const url = new URL(request.url);
      if (url.pathname === "/" && request.method === "GET") {
        const parts = capture(request, env);
        const bytes = encoder.encode(JSON.stringify(parts));
        if (bytes.length > MAX_SNAPSHOT_BYTES) return new Response("Metadata snapshot exceeds 8 KiB\n", { status: 413 });
        // The URL carries an immutable public snapshot. Any isolate can rebuild
        // exactly the same objects, without KV, local state, or redirect races.
        const cloneUrl = `${url.origin}/r/${encodeToken(bytes)}/repo.git`;
        return Response.json({ clone_url: cloneUrl, files: ["cf.json", "headers.json", "runtime.json", "vars.json"] }, {
          headers: { "Cache-Control": "no-store" },
        });
      }
      const match = /^\/r\/([A-Za-z0-9_-]+)\/repo\.git\/(.*)$/.exec(url.pathname);
      if (!match) return new Response("Repository not found\n", { status: 404 });
      if (!["GET", "POST"].includes(request.method)) return new Response("Method not allowed\n", { status: 405 });
      try {
        const parts = decodeToken(match[1]);
        const body = await readBody(request, 32768);
        return invoke(module, parts, `${match[2]}${url.search}`, body, request.method);
      } catch {
        return new Response("Invalid snapshot or request\n", { status: 400 });
      }
    },
  };
}
