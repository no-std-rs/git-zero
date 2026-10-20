import { spawn } from "node:child_process";
import { once } from "node:events";
import { fileURLToPath } from "node:url";

export const TOKEN = "git-zero-local-storage-test";
const root = new URL("../", import.meta.url);

export async function start(persist) {
  const child = spawn(process.execPath, [
    fileURLToPath(new URL("node_modules/wrangler/bin/wrangler.js", root)), "dev",
    "--config", "wrangler.streamed.toml", "--local", "--ip", "127.0.0.1", "--port", "0", "--inspector-port", "0",
    "--persist-to", persist, "--var", `HOST_TOKEN:${TOKEN}`, "--var", "TEST_MODE:true", "--show-interactive-dev-session=false",
  ], { cwd: fileURLToPath(root), env: { ...process.env, WRANGLER_SEND_METRICS: "false", NO_COLOR: "1" }, stdio: ["ignore", "pipe", "pipe"], detached: process.platform !== "win32" });
  let logs = "", exited = false;
  child.once("exit", () => { exited = true; });
  const stop = async () => {
    if (exited) return;
    const exit = once(child, "exit");
    try { if (process.platform === "win32") child.kill("SIGTERM"); else process.kill(-child.pid, "SIGTERM"); } catch (error) { if (error.code !== "ESRCH") throw error; }
    let timeout;
    await Promise.race([exit, new Promise((resolve) => { timeout = setTimeout(resolve, 5000); })]);
    clearTimeout(timeout);
    if (!exited) {
      try { if (process.platform === "win32") child.kill("SIGKILL"); else process.kill(-child.pid, "SIGKILL"); } catch (error) { if (error.code !== "ESRCH") throw error; }
      await exit;
    }
  };
  try {
    const origin = await new Promise((resolve, reject) => {
      const timeout = setTimeout(() => reject(new Error(`Wrangler startup timed out\n${logs}`)), 30000);
      const consume = (chunk) => {
        logs = (logs + chunk).slice(-20000);
        const match = /Ready on (http:\/\/127\.0\.0\.1:\d+)/.exec(logs);
        if (match) { clearTimeout(timeout); resolve(match[1]); }
      };
      child.stdout.on("data", consume); child.stderr.on("data", consume);
      child.once("error", (error) => { clearTimeout(timeout); reject(error); });
      child.once("exit", (code) => { clearTimeout(timeout); reject(new Error(`Wrangler exited (${code})\n${logs}`)); });
    });
    return { origin, stop, logs: () => logs };
  } catch (error) { await stop(); throw error; }
}

export function headers(format = 0) { return { authorization: `Bearer ${TOKEN}`, "x-test-format": String(format), "content-type": "application/x-git-receive-pack-request" }; }

export function chunked(bytes, length = 31) {
  let offset = 0;
  return new ReadableStream({ pull(controller) {
    if (offset === bytes.length) { controller.close(); return; }
    const end = Math.min(offset + length, bytes.length);
    controller.enqueue(bytes.subarray(offset, end)); offset = end;
  } });
}
