// Authenticated local Native -> Worker RPC transport. Each complete application
// request dispatches the existing DO HTTP bridge exactly once. No DO socket or
// Worker self-fetch is used. All test/benchmark executables remain Rust.
const PIECE = 16 * 1024;
const DATA_HEADER = 24;
const DATA_BYTES = PIECE - DATA_HEADER;
const HEADER_BYTES = 4096;
const BULK = 16 * 1024 * 1024 + 2060;
const METADATA = 1_500_000;
const PHASES = new Set(["startup", "push", "fetch", "maintenance", "stats"]);
const PROOF = "x-git-zero-retirement-proof";
export const SOCKET_CAPABILITY = Object.freeze({ version: 1, protocol: "git-zero-service-rpc.v1",
  piece_bytes: PIECE, header_bytes: HEADER_BYTES, body_bytes: BULK, rpcs: 16 });

export function socketRegistration() {
  const session = Array.from(crypto.getRandomValues(new Uint8Array(16)), b => b.toString(16).padStart(2, "0")).join("");
  return { session, active: null };
}

export function closeRegisteredSocket(registration) {
  const socket = registration?.socket?.active;
  // Cross-request close is best effort in workerd. Captured identity is also
  // checked on every message before dispatch, so an old idle socket cannot
  // turn its registration into a lease if this close cannot run in its context.
  if (socket) { registration.socket.active = null; try { socket.close(1000, "registration changed"); } catch { /* The next old-session message closes before I/O. */ } }
}

function routeBound(method, route) {
  if (method === "POST" && route === "blobs/read") return { request: METADATA, response: BULK };
  if (method === "POST" && route === "manifest/upload-retire") return { request: BULK, response: 32 * 1024 };
  if (method === "PUT" && /^blob\/chunks\/[0-9a-f]{64}$/.test(route)) return { request: 16 * 1024 * 1024, response: 32 * 1024 };
  return null;
}

function magic(bytes, value) { return value.split("").every((c, i) => bytes[i] === c.charCodeAt(0)); }
function frame(magicValue, values, payload = new Uint8Array()) {
  const header = 4 + values.length * 4;
  const result = new Uint8Array(header + payload.length);
  result.set(new TextEncoder().encode(magicValue));
  const view = new DataView(result.buffer);
  values.forEach((value, i) => view.setUint32(4 + i * 4, value, true));
  result.set(payload, header);
  return result;
}

export function upgradeServiceSocket(request, options) {
  const { registration, current, diagnostic, fail } = options;
  if (request.method !== "GET" || request.headers.get("upgrade")?.toLowerCase() !== "websocket"
      || request.headers.get("sec-websocket-protocol") !== SOCKET_CAPABILITY.protocol) fail(426, "expected service WebSocket Upgrade");
  if (!registration?.socket || !current()
      || request.headers.get("x-hosting-socket-session") !== registration.socket.session) fail(409, "invalid socket registration session");
  if (registration.socket.active) fail(429, "one active service socket per registration");
  const pair = new WebSocketPair();
  const [client, server] = Object.values(pair);
  server.binaryType = "arraybuffer";
  server.accept();
  registration.socket.active = server;
  diagnostic("upgrades_accepted");
  let dispatched = 0, running = false, awaitingAck = 0, closed = false;
  const close = (reason, code = 1008) => {
    if (closed) return;
    closed = true;
    diagnostic("closes");
    diagnostic(reason);
    if (registration.socket.active === server) registration.socket.active = null;
    try { server.close(code, reason); } catch { /* Already closed. */ }
  };
  server.addEventListener("close", () => close("peer_closed", 1000));
  server.addEventListener("error", () => close("transport_errors", 1011));
  server.addEventListener("message", async (event) => {
    diagnostic("incoming_messages");
    if (closed || !current() || registration.socket.active !== server) return close("stale_sessions");
    if (!(event.data instanceof ArrayBuffer)) return close("invalid_messages");
    const bytes = new Uint8Array(event.data);
    diagnostic("incoming_bytes", bytes.length);
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    if (awaitingAck && bytes.length === 12 && magic(bytes, "GZSA")
        && view.getUint32(4, true) === 1 && view.getUint32(8, true) === awaitingAck) {
      awaitingAck = 0;
      diagnostic("acknowledged_rpcs");
      if (dispatched === SOCKET_CAPABILITY.rpcs) close("rotations", 1000);
      return;
    }
    if (running || awaitingAck) return close("pipelined_messages");
    try {
      if (bytes.length < 20 || bytes.length > 20 + HEADER_BYTES + BULK || !magic(bytes, "GZSQ")
          || view.getUint32(4, true) !== 1) fail(400, "invalid socket request header");
      const id = view.getUint32(8, true), headerLength = view.getUint32(12, true), bodyLength = view.getUint32(16, true);
      if (id !== dispatched + 1 || id > SOCKET_CAPABILITY.rpcs || headerLength > HEADER_BYTES
          || bytes.length !== 20 + headerLength + bodyLength) fail(400, "invalid socket request identity or lengths");
      const header = options.strictJson(bytes.subarray(20, 20 + headerLength));
      if (!header || typeof header !== "object" || Array.isArray(header)
          || Object.keys(header).sort().join(",") !== "method,phase,proof,route"
          || !PHASES.has(header.phase) || typeof header.method !== "string" || typeof header.route !== "string"
          || !(header.proof === null || (typeof header.proof === "string" && header.proof.length <= 512))) fail(400, "invalid socket request metadata");
      const bounds = routeBound(header.method, header.route);
      if (!bounds || bodyLength > bounds.request) fail(400, "ineligible socket route or body");
      const headers = new Headers({ "x-hosting-repo": options.repo, "x-hosting-phase": header.phase,
        "content-type": "application/octet-stream" });
      if (header.proof) headers.set(PROOF, header.proof);
      const internal = new Request(`https://local/_services/${header.route}`, {
        method: header.method, headers, body: bytes.subarray(20 + headerLength),
      });
      // A complete bounded message and exact registration are validated before
      // any binding I/O. Busy/ACK gates prevent a response payload queue.
      if (!current()) return close("stale_sessions");
      running = true;
      dispatched = id;
      diagnostic("dispatched_rpcs");
      let response;
      try { response = await options.dispatch(internal, header.phase); }
      catch (error) { response = options.json({ error: String(error.message ?? error) }, error.status ?? 500); }
      if (!current() || closed) return close("stale_sessions");
      // The common bridge already recorded the DO receipt. Framing/send errors
      // cannot erase it; ambiguous dispatch remains explicitly incomplete.
      const body = await options.bytes(response, bounds.response);
      if (!current() || closed) return close("stale_sessions");
      const metadata = new TextEncoder().encode(JSON.stringify({ status: response.status, headers: Object.fromEntries(response.headers) }));
      if (metadata.length > HEADER_BYTES) fail(502, "socket response headers exceed bound");
      const pieces = Math.ceil(body.length / DATA_BYTES);
      awaitingAck = id;
      server.send(frame("GZSR", [1, id, metadata.length, body.length, pieces], metadata));
      diagnostic("outgoing_messages");
      diagnostic("outgoing_bytes", 24 + metadata.length);
      for (let index = 0, offset = 0; offset < body.length; index++) {
        const part = body.subarray(offset, Math.min(body.length, offset + DATA_BYTES));
        const piece = frame("GZSD", [id, index, offset, body.length, part.length], part);
        server.send(piece);
        diagnostic("outgoing_messages"); diagnostic("outgoing_bytes", piece.length);
        offset += part.length;
      }
      server.send(frame("GZSE", [1, id, body.length, pieces]));
      diagnostic("outgoing_messages"); diagnostic("outgoing_bytes", 20);
      diagnostic("completed_rpcs");
      if (!response.ok) diagnostic("error_status_rpcs");
    } catch {
      diagnostic("failed_messages");
      close("protocol_errors", 1011);
    } finally { running = false; }
  });
  return new Response(null, { status: 101, webSocket: client,
    headers: { "Sec-WebSocket-Protocol": SOCKET_CAPABILITY.protocol } });
}
