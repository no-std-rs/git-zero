import { decodeSnapshot, loadSnapshot, MAX_SNAPSHOT_BYTES, persistSnapshot, SnapshotError } from "./snapshots.mjs";
import { readRequestBody } from "./bytes.mjs";
import { captureMetadata } from "./metadata.mjs";
import { streamResponse } from "./stream.mjs";
import { importRepository, repositoryResponse } from "./repository.mjs";
import { treeRepository } from "./tree.mjs";

const encoder = new TextEncoder();
const MAX_TOKEN_LENGTH = Math.ceil(MAX_SNAPSHOT_BYTES / 3) * 4;

function encodeToken(bytes) {
  return btoa(String.fromCharCode(...bytes)).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

function decodeToken(token, version = 1) {
  if (token.length > MAX_TOKEN_LENGTH || !/^[A-Za-z0-9_-]+$/.test(token)) throw new Error("Invalid snapshot token");
  const base64 = token.replaceAll("-", "+").replaceAll("_", "/");
  const bytes = Uint8Array.from(atob(base64), (character) => character.charCodeAt(0));
  return decodeSnapshot(bytes, version);
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
  return streamResponse(exports, result);
}

export function createWorker(module) {
  return {
    async fetch(request, env = {}) {
      const url = new URL(request.url);
      if (url.pathname === "/" && request.method === "GET") {
        try {
          const layout = env.SNAPSHOT_LAYOUT ?? "flat";
          if (!["flat", "tree"].includes(layout)) throw new SnapshotError("Snapshot layout must be flat or tree", 503);
          const parts = captureMetadata(request, env);
          const version = layout === "tree" ? 2 : 1;
          let tree;
          if (version === 2) {
            parts[3] = JSON.stringify({ ...JSON.parse(parts[3]), format: 2, layout: "tree" }, null, 2) + "\n";
            tree = await treeRepository(parts);
            try { importRepository(new WebAssembly.Instance(module, {}).exports, tree.repository); }
            catch (error) { throw new SnapshotError("Tree metadata failed repository validation", error.status === 413 ? 413 : 400); }
          }
          const bytes = encoder.encode(JSON.stringify(version === 1 ? parts : { version, layout, format: "sha1", parts }));
          if (bytes.length > MAX_SNAPSHOT_BYTES) throw new SnapshotError("Metadata snapshot exceeds 8 KiB", 413);
          // Storage writes finish before publishing a URL. Reads always verify
          // the immutable record; no isolate-local mutable snapshot is reused.
          const stored = env.SNAPSHOT_STORAGE && env.SNAPSHOT_STORAGE !== "inline"
            ? await persistSnapshot(bytes, env, version) : null;
          const path = stored?.path ?? `/r/${version === 1 ? "" : "v2/"}${encodeToken(bytes)}/repo.git`;
          return Response.json({
            clone_url: `${url.origin}${path}`,
            files: tree?.files ?? ["cf.json", "headers.json", "runtime.json", "vars.json"],
            ...(tree ? { layout } : {}),
            ...(stored ? { expires_at: new Date(stored.expiresAt * 1000).toISOString() } : {}),
          }, { headers: { "Cache-Control": "no-store" } });
        } catch (error) {
          if (!(error instanceof SnapshotError)) throw error;
          return new Response(`${error.message}\n`, { status: error.status, headers: { "Cache-Control": "no-store" } });
        }
      }
      const treeInline = /^\/r\/v2\/([A-Za-z0-9_-]+)\/repo\.git\/(.*)$/.exec(url.pathname);
      const treeStored = /^\/s\/v2\/(kv|r2)\/([a-f0-9]{64})\/repo\.git\/(.*)$/.exec(url.pathname);
      const inline = treeInline ?? /^\/r\/([A-Za-z0-9_-]+)\/repo\.git\/(.*)$/.exec(url.pathname);
      const stored = treeStored ?? /^\/s\/(kv|r2)\/([a-f0-9]{64})\/repo\.git\/(.*)$/.exec(url.pathname);
      const version = treeInline || treeStored ? 2 : 1;
      if (!inline && !stored) return new Response("Repository not found\n", { status: 404 });
      if (!["GET", "POST"].includes(request.method)) return new Response("Method not allowed\n", { status: 405 });
      try {
        const parts = inline ? decodeToken(inline[1], version) : await loadSnapshot(env, stored[1], stored[2], version);
        const body = await readRequestBody(request, 32768);
        const target = `${inline ? inline[2] : stored[3]}${url.search}`;
        if (version === 1) return invoke(module, parts, target, body, request.method, request.headers);
        try {
          const tree = await treeRepository(parts);
          return repositoryResponse(module, tree.repository, target, body, request.method, request.headers);
        } catch (error) {
          throw new SnapshotError("Invalid or oversized tree snapshot", stored ? 502 : error.status === 413 ? 413 : 400);
        }
      } catch (error) {
        if (error instanceof SnapshotError) {
          return new Response(`${error.message}\n`, { status: error.status, headers: { "Cache-Control": "no-store", "Retry-After": "60" } });
        }
        return new Response("Invalid snapshot or request\n", { status: 400 });
      }
    },
  };
}
