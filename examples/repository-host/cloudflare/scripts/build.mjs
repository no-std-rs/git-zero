import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import { fileURLToPath } from "node:url";

const root = new URL("../../../../", import.meta.url);
const result = spawnSync("cargo", [
  "build", "--locked", "--release", "-p", "git-zero-host-wasm", "--target", "wasm32-unknown-unknown",
], { cwd: fileURLToPath(root), stdio: "inherit" });
if (result.error) throw result.error;
if (result.status !== 0) process.exit(result.status ?? 1);
const build = new URL("../build/", import.meta.url);
mkdirSync(build, { recursive: true });
copyFileSync(new URL("target/wasm32-unknown-unknown/release/git_zero_host_wasm.wasm", root), new URL("git_zero_host_wasm.wasm", build));
