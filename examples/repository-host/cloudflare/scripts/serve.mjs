// Process startup and environment validation only. Git runs in the same
// no_std Wasm module used by the Cloudflare deployment.
import { spawn } from "node:child_process";
import { mkdir } from "node:fs/promises";
import { isAbsolute } from "node:path";
import { fileURLToPath } from "node:url";

for (const name of ["HOST_TOKEN", "R2_S3_URL", "R2_ACCESS_KEY_ID", "R2_SECRET_ACCESS_KEY", "GIT_ZERO_STATE_DIR"]) {
  if (!process.env[name]) throw new Error(`required environment variable ${name} is absent`);
}
const state = process.env.GIT_ZERO_STATE_DIR;
if (!isAbsolute(state)) throw new Error("GIT_ZERO_STATE_DIR must be absolute");
await mkdir(state, { recursive: true });
const root = new URL("../", import.meta.url);
const child = spawn(fileURLToPath(new URL("node_modules/workerd/bin/workerd", root)), [
  "serve", "workerd.capnp", "--experimental", "--directory-path", `state=${state}`,
  "--socket-addr", `http=${process.env.GIT_ZERO_LISTEN ?? "127.0.0.1:8787"}`,
], { cwd: fileURLToPath(root), stdio: "inherit", env: { ...process.env, TEST_MODE: "false" } });
for (const signal of ["SIGINT", "SIGTERM"]) process.once(signal, () => child.kill(signal));
child.once("error", (error) => { process.stderr.write(`workerd startup failed: ${error.message}\n`); process.exitCode = 1; });
child.once("exit", (code, signal) => { process.exitCode = code ?? (signal === "SIGINT" || signal === "SIGTERM" ? 0 : 1); });
