// Register the same workload against both real local storage runtimes.
import { test } from "node:test";
import { start } from "./wrangler.mjs";
import { startStandalone } from "./standalone.mjs";

export function testRuntimes(title, options, run, { frozen = false } = {}) {
  const wrangler = frozen ? (persist) => start(persist, { frozen: true }) : start;
  for (const [runtime, serve] of [["Wrangler R2/SQLite", wrangler], ["direct workerd with S3/SQLite", startStandalone]]) {
    test(title.replace("{runtime}", runtime), { ...options }, (context) => run(serve, context, runtime));
  }
}
