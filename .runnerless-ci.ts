import { configuration, event, policies } from "runnerless/v1";

// Runnerless compiles this program to Wasm using its versioned SDK.
// Pull requests execute the trusted base revision of the program.
export function configure(): void {
  configuration.workflow("pull-request-policy", ["pull_request"]);
}

export function run(): void {
  const details = event.details();
  if (details.get("event").string() != "pull_request"
    || details.get("state").string() != "open") return;

  policies.validate();
}
