import { badges, BadgeSpec } from "runnerless/v1";
import {
  checks, configuration, event, jobs, OperationRef, OperationRequest,
  packages, policies, pulls, RegistryPublishRequest, repo, Value, workflow, WriteResult,
} from "runnerless/v1";

const program = workflow().releasePlease();

// Runnerless compiles this program to Wasm using its versioned SDK.
// Pull requests execute the trusted base revision of the program.
export function configure(): void {
  configuration.titlePolicy();
  badges.define(new BadgeSpec("badge", "medium", "slowest-job"));
  program.configure();
  configuration.workflow("crate-publishing", ["workflow_run", "runnerless_completion"]);
  configuration.workflow("pull-request-policy", [
    "pull_request", "status", "runnerless_completion", "issue_comment",
  ]);
}

export function run(): void {
  program.run();
  const details = event.details();
  publishCrates(details);
  if (!details.get("pullNumber").isNull() && pulls.get().state == "open") policies.validate();
}

// GitHub builds verified Cargo upload bodies without registry credentials.
// Runnerless verifies the published release and relays them in dependency order.
function publishCrates(details: Value): void {
  if (details.get("event").string() == "runnerless_completion") {
    const result = jobs.lookup<WriteResult>(new OperationRef<WriteResult>("op:publish-git-zero"));
    if (result.ok) {
      const value = result.value;
      checks.report("crates.io", value.get("status").string() == "succeeded"
        && value.get("value").get("failed").json == "false",
        "crates.io publication: " + value.get("status").string() + " " + value.get("value").get("error").string());
    }
    return;
  }
  if (details.get("event").string() != "workflow_run"
    || details.get("action").string() != "completed"
    || details.get("workflowPath").string() != ".github/workflows/publish-release.yml"
    || details.get("conclusion").string() != "success") return;
  const version = repo.readText("version.txt", "head").trim();
  const tag = "v" + version;
  if (details.get("headRef").string() != tag || !packages.releasePublished(tag, version)) return;
  const request = new RegistryPublishRequest(tag, version)
    .add("git-zero-core", "git-zero-core-" + version + ".cargo-upload")
    .add("git-zero-client", "git-zero-client-" + version + ".cargo-upload")
    .add("git-zero-server", "git-zero-server-" + version + ".cargo-upload")
    .add("git-zero", "git-zero-" + version + ".cargo-upload")
    .add("git-zero-rustix", "git-zero-rustix-" + version + ".cargo-upload");
  packages.crates(new OperationRequest("publish-git-zero"), request);
}
