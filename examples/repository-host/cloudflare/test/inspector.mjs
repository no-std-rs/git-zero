import { createServer } from "node:net";
import { once } from "node:events";
export async function inspectorPort() {
  const server = createServer(); server.listen(0, "127.0.0.1"); await once(server, "listening");
  const port = server.address().port; await new Promise((r) => server.close(r)); return port;
}
export async function inspectIsolate(port) {
  const response = await fetch(`http://127.0.0.1:${port}/json/list`), targets = await response.json();
  const target = targets.find((entry) => entry.id.includes("repository")) ?? targets[0];
  if (!target) throw new Error("workerd isolate inspector unavailable");
  const ws = new WebSocket(target.webSocketDebuggerUrl); await once(ws, "open");
  let seq = 0; const pending = new Map(), contexts = [];
  ws.addEventListener("message", (event) => { const data = JSON.parse(event.data); if (data.method === "Runtime.executionContextCreated") contexts.push(data.params.context); const callback = pending.get(data.id); if (callback) { pending.delete(data.id); clearTimeout(callback.timer); data.error ? callback.reject(new Error(data.error.message)) : callback.resolve(data.result); } });
  const fail = () => { for (const callback of pending.values()) { clearTimeout(callback.timer); callback.reject(new Error("workerd inspector disconnected")); } pending.clear(); };
  ws.addEventListener("close", fail); ws.addEventListener("error", fail);
  return { contexts, command(method, params = {}) { return new Promise((resolve, reject) => { const id = ++seq; const timer = setTimeout(() => { pending.delete(id); reject(new Error(`workerd inspector ${method} timed out`)); }, 5000); pending.set(id, { resolve, reject, timer }); ws.send(JSON.stringify({ id, method, params })); }); }, close() { ws.close(); } };
}
