import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import { fileURLToPath } from "node:url";

const rootUrl = new URL("../../../", import.meta.url);
const root = fileURLToPath(rootUrl);
const result = spawnSync("cargo", [
  "build", "--locked", "--release", "-p", "git-zero-worker-wasm",
  "--target", "wasm32-unknown-unknown",
], { cwd: root, stdio: "inherit" });
if (result.error) throw result.error;
if (result.status !== 0) process.exit(result.status ?? 1);
const build = new URL("../build/", import.meta.url);
mkdirSync(build, { recursive: true });
copyFileSync(
  new URL("target/wasm32-unknown-unknown/release/git_zero_worker_wasm.wasm", rootUrl),
  new URL("git_zero_worker_wasm.wasm", build),
);
