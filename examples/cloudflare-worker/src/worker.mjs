import { decodeSnapshot, loadSnapshot, MAX_SNAPSHOT_BYTES, persistSnapshot, SnapshotError } from "./snapshots.mjs";
import { readBytes } from "./bytes.mjs";
import { captureMetadata } from "./metadata.mjs";

const encoder = new TextEncoder();
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
  return decodeSnapshot(bytes);
}

function invoke(module, parts, target, body, method, headers) {
  // Each response owns an instance, so overlapping fetches cannot share
  // Rust's fixed input/output arrays. The compiled module itself is reusable.
  const { exports } = new WebAssembly.Instance(module, {});
  const encoded = parts.map((part) => encoder.encode(part));
  const targetBytes = encoder.encode(target);
  const metadata = [headers.get("git-protocol") ?? "", headers.get("content-type") ?? ""].map((value) => encoder.encode(value));
  const length = 16 + encoded.reduce((total, part) => total + part.length, 0) + 4 + targetBytes.length + 8 + metadata.reduce((total, value) => total + value.length, 0) + body.length;
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
  metadata.forEach((value, index) => view.setUint32(offset + index * 4, value.length, true));
  offset += 8;
  for (const value of metadata) {
    input.set(value, offset);
    offset += value.length;
  }
  input.set(body, offset);
  const result = exports.stream_start(length, method === "POST" ? 1 : 0, 0);
  if (result < 0) return new Response("Invalid or oversized Git request\n", { status: result === -1 ? 413 : 400 });
  const output = new Uint8Array(exports.memory.buffer, exports.output_ptr(), 3);
  const status = new DataView(output.buffer, output.byteOffset, 3).getUint16(0, true);
  const type = CONTENT_TYPES[output[2]];
  const stream = new ReadableStream({
    pull(controller) {
      const count = exports.stream_read(16384);
      if (count < 0) {
        exports.stream_cancel();
        controller.error(new Error("Git response failed"));
      } else if (count === 0) {
        exports.stream_cancel();
        controller.close();
      } else {
        // Rust reuses OUTPUT on each pull; queued JS chunks own their bytes.
        controller.enqueue(new Uint8Array(exports.memory.buffer, exports.output_ptr(), count).slice());
      }
    },
    cancel() { exports.stream_cancel(); },
  }, { highWaterMark: 0 });
  return new Response(stream, {
    status,
    headers: { "Content-Type": type, "Cache-Control": "no-store" },
  });
}

async function readBody(request, maximum) {
  const declared = request.headers.get("content-length");
  if (declared !== null && (!/^\d+$/.test(declared) || Number(declared) > maximum)) throw new Error("Request too large");
  if (!request.body) return new Uint8Array();
  const encoding = request.headers.get("content-encoding")?.toLowerCase();
  if (encoding && !["gzip", "identity"].includes(encoding)) throw new Error("Unsupported content encoding");
  let wireLength = 0;
  const bounded = request.body.pipeThrough(new TransformStream({
    transform(chunk, controller) {
      wireLength += chunk.length;
      if (wireLength > maximum) throw new Error("Request too large");
      controller.enqueue(chunk);
    },
  }));
  const stream = encoding === "gzip" ? bounded.pipeThrough(new DecompressionStream("gzip")) : bounded;
  return readBytes(stream, maximum);
}

export function createWorker(module) {
  return {
    async fetch(request, env = {}) {
      const url = new URL(request.url);
      if (url.pathname === "/" && request.method === "GET") {
        try {
          const parts = captureMetadata(request, env);
          const bytes = encoder.encode(JSON.stringify(parts));
          if (bytes.length > MAX_SNAPSHOT_BYTES) throw new SnapshotError("Metadata snapshot exceeds 8 KiB", 413);
          // Storage writes finish before publishing a URL. Reads always verify
          // the immutable record; no isolate-local mutable snapshot is reused.
          const stored = env.SNAPSHOT_STORAGE && env.SNAPSHOT_STORAGE !== "inline"
            ? await persistSnapshot(bytes, env) : null;
          const path = stored?.path ?? `/r/${encodeToken(bytes)}/repo.git`;
          return Response.json({
            clone_url: `${url.origin}${path}`,
            files: ["cf.json", "headers.json", "runtime.json", "vars.json"],
            ...(stored ? { expires_at: new Date(stored.expiresAt * 1000).toISOString() } : {}),
          }, { headers: { "Cache-Control": "no-store" } });
        } catch (error) {
          if (!(error instanceof SnapshotError)) throw error;
          return new Response(`${error.message}\n`, { status: error.status, headers: { "Cache-Control": "no-store" } });
        }
      }
      const inline = /^\/r\/([A-Za-z0-9_-]+)\/repo\.git\/(.*)$/.exec(url.pathname);
      const stored = /^\/s\/(kv|r2)\/([a-f0-9]{64})\/repo\.git\/(.*)$/.exec(url.pathname);
      if (!inline && !stored) return new Response("Repository not found\n", { status: 404 });
      if (!["GET", "POST"].includes(request.method)) return new Response("Method not allowed\n", { status: 405 });
      try {
        const parts = inline ? decodeToken(inline[1]) : await loadSnapshot(env, stored[1], stored[2]);
        const body = await readBody(request, 32768);
        return invoke(module, parts, `${inline ? inline[2] : stored[3]}${url.search}`, body, request.method, request.headers);
      } catch (error) {
        if (error instanceof SnapshotError) {
          return new Response(`${error.message}\n`, { status: error.status, headers: { "Cache-Control": "no-store", "Retry-After": "60" } });
        }
        return new Response("Invalid snapshot or request\n", { status: 400 });
      }
    },
  };
}
