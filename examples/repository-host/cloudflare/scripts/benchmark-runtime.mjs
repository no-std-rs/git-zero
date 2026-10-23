// Freeze the streamed host before either benchmark starts its measurements.
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { copyFile, mkdir, readFile, readdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { start } from "../test/wrangler.mjs";
import { startStandalone } from "../test/standalone.mjs";

const root = fileURLToPath(new URL("../", import.meta.url));
const execute = promisify(execFile);
const environment = { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_TERMINAL_PROMPT: "0", WRANGLER_SEND_METRICS: "false" };

async function bundleFiles(directory, prefix = "") {
  const files = [];
  for (const entry of await readdir(join(directory, prefix), { withFileTypes: true })) {
    const path = join(prefix, entry.name);
    if (entry.isDirectory()) files.push(...await bundleFiles(directory, path));
    else if (entry.isFile()) {
      const bytes = await readFile(join(directory, path));
      files.push({ path, bytes: bytes.length, sha256: createHash("sha256").update(bytes).digest("hex") });
    }
  }
  return files.sort((left, right) => left.path.localeCompare(right.path));
}

export async function freezeRuntime(bundle, runtime, format, { signal, run = (binary, args) => execute(binary, args, { cwd: root, env: environment, signal }) } = {}) {
  if (runtime === "wrangler") {
    await run(process.execPath, [join(root, "node_modules/wrangler/bin/wrangler.js"), "deploy", "--config", "wrangler.toml", "--dry-run", "--outdir", bundle]);
    await writeFile(join(bundle, "wrangler.toml"), [
      'name = "git-zero-own-scale-proof"', 'main = "streamed.js"', 'compatibility_date = "2026-10-06"',
      'send_metrics = false', '[vars]', 'REPO_FORMAT = "' + format + '"',
      '[[r2_buckets]]', 'binding = "BLOBS"', 'bucket_name = "git-zero-repository-data"', 'remote = false',
      '[[durable_objects.bindings]]', 'name = "REPOSITORIES"', 'class_name = "OwnRepository"',
      '[[migrations]]', 'tag = "v1"', 'new_sqlite_classes = ["OwnRepository"]', '',
    ].join("\n"));
  } else {
    await run(process.execPath, ["scripts/build.mjs"]);
    await mkdir(join(bundle, "src"), { recursive: true });
    await mkdir(join(bundle, "build"));
    for (const file of await readdir(join(root, "src"))) if (file.endsWith(".mjs")) await copyFile(join(root, "src", file), join(bundle, "src", file));
    await copyFile(join(root, "build/git_zero_host_wasm.wasm"), join(bundle, "build/git_zero_host_wasm.wasm"));
    await copyFile(join(root, "workerd.capnp"), join(bundle, "workerd.capnp"));
  }
  await writeFile(join(bundle, "package.json"), '{"name":"git-zero-frozen-runtime","private":true,"type":"module"}\n');
  return bundleFiles(bundle);
}

export function startRuntime(persist, bundle, runtime, format) {
  return runtime === "wrangler" ? start(persist, { config: join(bundle, "wrangler.toml"), cwd: bundle, frozen: true })
    : startStandalone(persist, { format, cwd: bundle });
}
