// The pinned Wrangler runtime, with development reload sources disabled for
// long measurements. Bindings and storage still come from the real config.
import { unstable_startWorker } from "wrangler";
import { TOKEN } from "./wrangler.mjs";

const [config, persistTo, hostToken = TOKEN] = process.argv.slice(2);
const worker = await unstable_startWorker({
  config,
  bindings: { ...(hostToken ? { HOST_TOKEN: { type: "plain_text", value: hostToken } } : {}), TEST_MODE: { type: "plain_text", value: "true" } },
  dev: { watch: false, registry: undefined, remote: false, persist: persistTo, inspector: false, server: { hostname: "127.0.0.1", port: 0 } },
});
await worker.ready;
process.stdout.write(`Ready on ${(await worker.url).origin}\n`);
for (const signal of ["SIGINT", "SIGTERM"]) process.once(signal, async () => {
  await worker.dispose(); process.exit(0);
});
