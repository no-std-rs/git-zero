import { Codec, CodecError, hex, integer } from "./codec.mjs";
import { ObjectSource } from "./source.mjs";
import { bytes, PAGE_BYTES } from "./storage.mjs";
import { ApiError, encode, json } from "./api.mjs";

export const READ_LIMITS = Object.freeze({
  visibleObjects: 20000, edges: 100000, refs: 20000, pageSize: 100,
  treeBytes: 1024 * 1024, headerBytes: 128 * 1024, messageBytes: 32 * 1024,
  metadataBytes: 8 * 1024 * 1024, blobBytes: 8 * 1024 * 1024,
  pathBytes: 4096, pathDepth: 64, tagDepth: 32, parents: 256,
  identityBytes: 4096, jsonBytes: 512 * 1024, workBytes: 128 * 1024 * 1024, storageBytes: 128 * 1024 * 1024,
});
const text = (data) => new TextDecoder("utf-8", { ignoreBOM: true }).decode(data);
const fromHex = (id) => Uint8Array.from(id.match(/../g), (part) => Number.parseInt(part, 16));
function b64(data) {
  const parts = [];
  for (let at = 0; at < data.length; at += 4096) parts.push(String.fromCharCode(...data.subarray(at, at + 4096)));
  return btoa(parts.join("")).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}
function utf8(data) { try { return new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(data); } catch { return null; } }
const named = (data) => ({ name: utf8(bytes(data)), name64: b64(bytes(data)) });
const missing = () => new ApiError("revision or path not found", 404);
function number(value, fallback, maximum, minimum = 0) {
  if (value === null || value === undefined) return fallback;
  if (!/^(0|[1-9][0-9]*)$/.test(String(value))) throw new ApiError("invalid numeric parameter");
  const result = Number(value);
  if (!Number.isSafeInteger(result) || result < minimum || result > maximum) throw new ApiError("numeric parameter exceeds limit");
  return result;
}
function identity(line, offset, maximum) {
  const left = line.lastIndexOf(60), right = line.lastIndexOf(62);
  if (left < offset || right < left) throw new ApiError("unsupported commit identity", 422);
  let end = left; while (end > offset && line[end - 1] === 32) end--;
  const name = line.subarray(offset, end), email = line.subarray(left + 1, right);
  if (name.length + email.length > maximum) throw new ApiError("commit identity exceeds byte limit", 413);
  const stamp = /^ ([+-]?[0-9]+) ([+-][0-9]{4})$/.exec(text(line.subarray(right + 1)));
  if (!stamp) throw new ApiError("unsupported commit identity time", 422);
  return { name: text(name), email: text(email), name64: b64(name), email64: b64(email), time: stamp[1], timezone: stamp[2] };
}

export class ReadApi {
  constructor(store, module, format, limits = {}) {
    this.store = store; this.module = module; this.format = format;
    this.limits = { ...READ_LIMITS, ...limits }; this.generation = store.exec("SELECT generation FROM repository WHERE id=1")[0].generation;
    this.metadataBytes = 0; this.metadata = new Map(); this.visible = false;
  }
  source() {
    if (!this.objects) {
      const limits = { maxObjectBytes: 64 * 1024 * 1024, maxCanonicalBytes: 32 * 1024 * 1024, maxTemporaryBytes: 0, maxWorkBytes: this.limits.workBytes, maxTotalWorkBytes: this.limits.workBytes, maxLinks: this.limits.edges };
      for (const [key, value] of Object.entries(this.store.env?.CODEC_LIMITS ?? {})) limits[key] = key in limits ? Math.min(limits[key], value) : value;
      this.codec = new Codec(this.module, this.format, limits);
      this.objects = new ObjectSource(this.store, this.codec, `browse:${crypto.randomUUID()}`, { storageBytes: this.limits.storageBytes });
    }
    return this.objects;
  }
  reset() { this.objects?.releaseCaches(); this.store.exec("DELETE FROM browse_visible"); }
  result(value) {
    if (encode(JSON.stringify(value)).length > this.limits.jsonBytes) throw new ApiError("JSON response exceeds byte limit; lower page limit", 413);
    return json(value);
  }
  cursor(op, value) { return b64(encode(JSON.stringify({ v: 1, op, namespace: this.store.namespace, generation: this.generation, ...value }))); }
  decodeCursor(op, value) {
    if (!value) return null;
    if (value.length > 1024 || !/^[A-Za-z0-9_-]+$/.test(value)) throw new ApiError("invalid cursor");
    let cursor;
    try {
      const decoded = Uint8Array.from(atob(value.replaceAll("-", "+").replaceAll("_", "/")), (c) => c.charCodeAt(0));
      if (b64(decoded) !== value) throw new Error();
      cursor = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(decoded));
    } catch { throw new ApiError("invalid cursor"); }
    if (!cursor || Array.isArray(cursor) || typeof cursor !== "object" || !Number.isSafeInteger(cursor.generation) || cursor.generation < 0) throw new ApiError("invalid cursor");
    if (cursor.v !== 1 || cursor.op !== op || cursor.namespace !== this.store.namespace) throw new ApiError("invalid cursor");
    if (cursor.generation !== this.generation) throw new ApiError("cursor is stale after ref publication; restart pagination", 409);
    return cursor;
  }
  head() {
    const head = this.store.exec("SELECT head FROM repository WHERE id=1")[0].head;
    return { symbolic: utf8(bytes(head)), symbolic64: b64(bytes(head)), oid: this.store.exec("SELECT oid FROM refs WHERE name=?", head)[0]?.oid ?? null };
  }
  refsPage(params) {
    const limit = number(params.get("limit"), Math.min(20, this.limits.pageSize), this.limits.pageSize, 1), cursor = this.decodeCursor("refs", params.get("cursor"));
    const offset = number(cursor?.offset, 0, this.limits.refs);
    const rows = this.store.exec("SELECT name,oid,peeled FROM refs ORDER BY name LIMIT ? OFFSET ?", limit + 1, offset);
    if (offset + rows.length > this.limits.refs) throw new ApiError("ref enumeration budget exceeded", 413);
    const head = this.head(), refs = []; let size = encode(JSON.stringify(head)).length + 512;
    for (const row of rows.slice(0, limit)) {
      const entry = { ...named(row.name), oid: row.oid, peeled: row.peeled, kind: text(bytes(row.name)).startsWith("refs/heads/") ? "branch" : text(bytes(row.name)).startsWith("refs/tags/") ? "tag" : "other" };
      const cost = encode(JSON.stringify(entry)).length + 1;
      if (refs.length && size + cost > this.limits.jsonBytes) break;
      refs.push(entry); size += cost;
    }
    return this.result({ generation: this.generation, head, refs, nextCursor: rows.length > refs.length ? this.cursor("refs", { offset: offset + refs.length }) : null });
  }
  walk() {
    if (this.visible) return;
    this.store.exec("DELETE FROM browse_visible");
    if (this.store.exec("SELECT 1 FROM refs ORDER BY name LIMIT 1 OFFSET ?", this.limits.refs).length) throw new ApiError("ref traversal budget exceeded", 413);
    this.store.exec("INSERT OR IGNORE INTO browse_visible(oid) SELECT oid FROM refs");
    let objects = this.store.exec("SELECT COUNT(*) AS n FROM browse_visible")[0].n, edges = 0;
    if (objects > this.limits.visibleObjects) throw new ApiError("reachability budget exceeded", 413);
    for (;;) {
      const page = this.store.exec("SELECT oid FROM browse_visible WHERE walked=0 ORDER BY oid LIMIT 64");
      if (!page.length) break;
      for (const item of page) {
        const row = this.store.published(item.oid);
        if (!row?.scanned) throw new Error("published graph metadata is absent");
        let relation = -1, after = this.store.binaryEdges ? new ArrayBuffer(0) : "";
        for (;;) {
          const links = this.store.exec(`SELECT dst,relation,${this.store.edgeTargetSQL()} AS oid FROM stage_edges WHERE stage=? AND node=? AND (relation,dst)>(?,?) ORDER BY relation,dst LIMIT 64`, this.store.edgeStage(row.stage), row.node, relation, after);
          if (!links.length) break;
          for (const link of links) {
            if (++edges > this.limits.edges) throw new ApiError("reachability edge budget exceeded", 413);
            if (this.store.exec("INSERT OR IGNORE INTO browse_visible(oid) VALUES(?) RETURNING oid", link.oid).length && ++objects > this.limits.visibleObjects) throw new ApiError("reachability budget exceeded", 413);
            relation = link.relation; after = this.store.binaryEdges ? bytes(link.dst).slice().buffer : link.dst;
          }
        }
        this.store.exec("UPDATE browse_visible SET walked=1 WHERE oid=?", item.oid);
      }
    }
    this.visible = true;
  }
  object(oid) {
    this.walk();
    if (typeof oid !== "string" || !new RegExp(`^[0-9a-f]{${this.format ? 64 : 40}}$`).test(oid) || !this.store.exec("SELECT 1 FROM browse_visible WHERE oid=?", oid).length) throw missing();
    const row = this.store.published(oid); if (!row?.scanned) throw missing(); return row;
  }
  resolve(revision = "HEAD") {
    if (revision.startsWith("oid:")) return this.object(revision.slice(4).toLowerCase());
    if (revision === "HEAD") { const id = this.head().oid; if (!id) throw missing(); return this.object(id); }
    if (encode(revision).length > 65520) throw new ApiError("revision name exceeds byte limit", 413);
    const names = revision.startsWith("refs/") ? [revision] : [revision, `refs/${revision}`, `refs/heads/${revision}`, `refs/tags/${revision}`];
    const candidates = this.store.exec(`SELECT name,oid FROM refs WHERE name IN (${names.map(() => "?").join(",")})`, ...names.map((name) => encode(name).buffer));
    this.walk();
    const oid = new RegExp(`^[0-9a-fA-F]{${this.format ? 64 : 40}}$`).test(revision) && this.store.exec("SELECT 1 FROM browse_visible WHERE oid=?", revision.toLowerCase()).length ? revision.toLowerCase() : null;
    if (candidates.length + Number(Boolean(oid)) > 1) throw new ApiError("ambiguous revision; use a full ref name or oid:<id>", 409);
    return this.object(oid ?? candidates[0]?.oid);
  }
  peel(row) {
    for (let depth = 0; row.kind === 4; depth++) {
      if (depth >= this.limits.tagDepth) throw new ApiError("tag depth budget exceeded", 413);
      const edge = this.store.exec(`SELECT ${this.store.edgeTargetSQL()} AS oid FROM stage_edges WHERE stage=? AND node=? AND relation=2 LIMIT 1`, this.store.edgeStage(row.stage), row.node)[0];
      if (!edge) throw new Error("tag target metadata is absent"); row = this.object(edge.oid);
    }
    return row;
  }
  async prefix(row, maximum) {
    const length = Math.min(row.size, maximum);
    if (this.metadataBytes + length > this.limits.metadataBytes) throw new ApiError("metadata read budget exceeded; lower page limit", 413);
    this.metadataBytes += length;
    const data = new Uint8Array(length); let at = 0;
    for await (const part of this.source().stream(row, { offset: 0, length })) { data.set(part, at); at += part.length; }
    return data;
  }
  async commit(row) {
    row = this.peel(row);
    if (row.kind !== 1) throw new ApiError("revision is not a commit", 422);
    if (this.metadata.has(row.oid)) return this.metadata.get(row.oid);
    const data = await this.prefix(row, this.limits.headerBytes + this.limits.messageBytes);
    const result = { oid: row.oid, parents: [], tree: null, author: null, committer: null };
    let start = 0, messageStart = -1;
    while (start < data.length) {
      const end = data.indexOf(10, start);
      if (end < 0 || end > this.limits.headerBytes) throw new ApiError("commit headers exceed byte limit", 413);
      if (end === start) { messageStart = end + 1; break; }
      const line = data.subarray(start, end), header = text(line.subarray(0, 10));
      if (header.startsWith("tree ")) result.tree = text(line.subarray(5));
      else if (header.startsWith("parent ")) { if (result.parents.length >= this.limits.parents) throw new ApiError("commit parent limit exceeded", 413); result.parents.push(text(line.subarray(7))); }
      else if (header.startsWith("author ")) result.author = identity(line, 7, this.limits.identityBytes);
      else if (header.startsWith("committer ")) result.committer = identity(line, 10, this.limits.identityBytes);
      else if (header.startsWith("encoding ")) result.encoding = text(line.subarray(9, Math.min(line.length, 137)));
      start = end + 1;
    }
    if (messageStart < 0 || !result.tree || !result.author || !result.committer) throw new ApiError("unsupported commit metadata", 422);
    const message = data.subarray(messageStart, Math.min(data.length, messageStart + this.limits.messageBytes));
    const lines = text(message).split("\n"); let begin = 0, end, body;
    while (begin < lines.length && /^[ \t\r]*$/.test(lines[begin])) begin++;
    end = begin; while (end < lines.length && !/^[ \t\r]*$/.test(lines[end])) end++;
    body = end; while (body < lines.length && /^[ \t\r]*$/.test(lines[body])) body++;
    result.message = { subject: lines.slice(begin, end).map((line) => line.replace(/^[ \t\r]+|[ \t\r]+$/g, "")).join(" "), body: lines.slice(body).join("\n"), truncated: row.size - messageStart > message.length, raw64: b64(message) };
    this.metadata.set(row.oid, result); return result;
  }
  path(value) {
    value ??= "";
    if (encode(value).length > this.limits.pathBytes || value.includes("\0")) throw new ApiError("path exceeds byte limit", 413);
    const parts = value ? value.split("/") : [];
    if (parts.length > this.limits.pathDepth || parts.some((part) => !part || part === "." || part === "..")) throw new ApiError("invalid path or path depth exceeded");
    return parts;
  }
  async treeEntries(row, visit) {
    if (row.kind !== 2) throw new ApiError("revision or path is not a tree", 422);
    if (row.size > this.limits.treeBytes) throw new ApiError("tree exceeds metadata byte limit", 413);
    const data = await this.prefix(row, row.size), codec = this.codec;
    codec.graphInit({ kind: 2, size: row.size });
    let at = 0;
    for (;;) {
      const end = Math.min(at + PAGE_BYTES, data.length);
      const event = codec.graphFeed(data.subarray(at, end), end === data.length); at += event.consumed;
      if (event.tag === 32) visit({ name: event.data, oid: hex(event.id), mode: integer(event.a), kind: event.flags & 255 });
      if (event.tag === 33) { if (hex(event.id) !== row.oid) throw new Error("tree digest differs"); break; }
      if (event.tag === 30 && at === data.length) throw new Error("tree scan ended early");
    }
  }
  async atPath(revision, path) {
    let row = this.peel(this.resolve(revision));
    if (row.kind === 1) {
      const tree = this.store.exec(`SELECT ${this.store.edgeTargetSQL()} AS oid FROM stage_edges WHERE stage=? AND node=? AND relation=0 LIMIT 1`, this.store.edgeStage(row.stage), row.node)[0];
      if (!tree) throw new Error("commit tree metadata is absent"); row = this.object(tree.oid);
    }
    for (const part of this.path(path)) {
      let found;
      await this.treeEntries(row, (entry) => { if (utf8(entry.name) === part) found = entry; });
      if (!found) throw missing();
      if (!found.kind) throw new ApiError("submodule content belongs to another repository", 422);
      row = this.object(found.oid);
    }
    return row;
  }
  async history(params) {
    const cursor = this.decodeCursor("history", params.get("cursor")), limit = number(params.get("limit"), Math.min(20, this.limits.pageSize), this.limits.pageSize, 1);
    if (!cursor && !params.has("rev") && !this.head().oid) return this.result({ generation: this.generation, commits: [], nextCursor: null });
    const first = this.peel(this.resolve(params.get("rev") ?? (cursor ? `oid:${cursor.start}` : "HEAD")));
    if (cursor && cursor.start !== first.oid) throw new ApiError("cursor revision differs");
    let row = cursor ? this.object(cursor.next) : first, next = null, size = 256;
    const commits = [];
    while (row) {
      if (commits.length && this.metadataBytes + Math.min(row.size, this.limits.headerBytes + this.limits.messageBytes) > this.limits.metadataBytes) { next = row.oid; break; }
      const entry = await this.commit(row), entrySize = encode(JSON.stringify(entry)).length;
      if (commits.length && size + entrySize > this.limits.jsonBytes - 256) { next = row.oid; break; }
      commits.push(entry); size += entrySize;
      const parent = entry.parents[0];
      if (commits.length === limit) { next = parent ?? null; break; }
      row = parent ? this.object(parent) : null;
    }
    return this.result({ generation: this.generation, commits, nextCursor: next ? this.cursor("history", { start: first.oid, next }) : null });
  }
  async tree(params) {
    const cursor = this.decodeCursor("tree", params.get("cursor")), limit = number(params.get("limit"), Math.min(20, this.limits.pageSize), this.limits.pageSize, 1);
    let row, revision;
    if (cursor && !params.has("rev") && !params.has("path")) {
      row = this.object(cursor.directory); revision = cursor.revision;
    } else {
      const root = this.resolve(params.get("rev") ?? (cursor ? `oid:${cursor.revision}` : "HEAD"));
      revision = root.oid; row = await this.atPath(`oid:${revision}`, params.get("path") ?? "");
      if (cursor && (cursor.directory !== row.oid || cursor.revision !== revision)) throw new ApiError("cursor path or revision differs");
    }
    const offset = number(cursor?.offset, 0, this.limits.treeBytes), entries = [];
    let count = 0, more = false, size = 768;
    await this.treeEntries(row, (entry) => {
      if (count++ < offset || more) return;
      if (entries.length >= limit) { more = true; return; }
      const child = entry.kind ? this.object(entry.oid) : null;
      const result = { ...named(entry.name), mode: entry.mode.toString(8).padStart(6, "0"), kind: entry.kind === 0 ? "submodule" : entry.kind === 2 ? "tree" : entry.mode === 0o120000 ? "symlink" : "blob", oid: entry.oid, size: child?.kind === 3 ? child.size : null };
      const cost = encode(JSON.stringify(result)).length + 1;
      if (entries.length && size + cost > this.limits.jsonBytes) { more = true; return; }
      entries.push(result); size += cost;
    });
    return this.result({ generation: this.generation, oid: row.oid, entries, nextCursor: more ? this.cursor("tree", { revision, directory: row.oid, offset: offset + entries.length }) : null });
  }
  async handle(operation, request, stream) {
    let streaming = false;
    try {
      if (request.method !== "GET") return json({ error: "method not allowed" }, 405);
      const url = new URL(request.url);
      try { decodeURIComponent(url.search.replaceAll("+", "%20")); } catch { throw new ApiError("query must be valid UTF-8"); }
      const params = url.searchParams;
      if (operation === "refs") return this.refsPage(params);
      if (operation === "history") return await this.history(params);
      if (operation === "tree") return await this.tree(params);
      if (operation === "commit") return this.result(await this.commit(this.resolve(params.get("rev") ?? "HEAD")));
      if (operation === "blob") {
        if (params.has("oid") && (params.has("rev") || params.has("path"))) throw new ApiError("oid cannot be combined with rev/path");
        const row = params.has("oid") ? this.object(params.get("oid").toLowerCase()) : await this.atPath(params.get("rev") ?? "HEAD", params.get("path") ?? "");
        if (row.kind !== 3) throw new ApiError("object or path is not a blob", 422);
        if (row.size > number(params.get("maxBytes"), this.limits.blobBytes, this.limits.blobBytes)) throw new ApiError("blob exceeds byte limit", 413);
        const owner = this;
        async function* body() { try { yield* owner.source().stream(row); } finally { owner.reset(); } }
        const response = stream(body(), "application/octet-stream", row.size);
        response.headers.set("Content-Length", String(row.size)); response.headers.set("X-Content-Type-Options", "nosniff"); response.headers.set("Content-Disposition", "attachment"); response.headers.set("X-Object-Id", row.oid);
        streaming = true; return response;
      }
      throw new ApiError("unknown route", 404);
    } catch (error) {
      if (error instanceof CodecError && error.code === -14) throw new ApiError("object reconstruction budget exceeded", 413);
      throw error;
    } finally { if (!streaming) this.reset(); }
  }
}
