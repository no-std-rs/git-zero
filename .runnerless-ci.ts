import { badges, BadgeSpec } from "runnerless/v1";
import {
  ArtifactComment, ArtifactSpec, ArtifactStatus, artifacts,
  checks, comments, configuration, event, jobs, OperationRef, OperationRequest,
  packages, PageRequest, policies, pulls, RegistryPublishRequest, repo, Value, workflow, WriteResult,
} from "runnerless/v1";

const comparison = new ArtifactSpec("public-artifacts",
  new ArtifactStatus("Complexity comparison", 130678843, "https://pawel.buildbuddy.io"), "comparison-files")
  .file("comment.md", "text/plain", 24576)
  .file("publication.json", "application/json", 4096)
  .file("report.json", "application/json", 16777216)
  .file("report.md", "text/plain", 16777216)
  .file("baseline.md", "text/plain", 1048576)
  .file("baseline.json", "application/json", 16777216);
comparison.comment = ArtifactComment.generated("comment.md", "publication.json", 24576);

const program = workflow().releasePlease().publishArtifacts("complexity", comparison);
const ratioPrefix = "LM-CC/token data: `";
const publisherPrefix = "<!-- codex-review-artifact-publisher -->\n<!-- ci-toolkit:complexity -->\n";
// The same My Toolkit bot that publishes the verified comparison envelope.
const publisherActorId: i64 = 317265358;

function waitingForRatio(): void {
  checks.report("LM-CC/token", false,
    "Waiting for verified LM-CC/token data for the current PR head and base.");
}

function reportPullChecks(details: Value): void {
  if (details.get("pullNumber").isNull()) return;
  const pr = pulls.get();
  if (pr.state != "open") return;
  // A status/completion delivery can recover a failed PR delivery. Regenerate
  // its required policies too, rather than leaving their contexts missing.
  policies.validate();
  let nextPage: i32 = 1;
  while (nextPage > 0) {
    const page = comments.list(new PageRequest(nextPage, 20));
    for (let i = 0; i < page.items.length; i++) {
      const comment = page.items[i];
      if (comment.get("actorId").integer() != publisherActorId) continue;
      const body = comment.get("body").string();
      if (!body.startsWith(publisherPrefix)) continue;
      const start = body.indexOf(ratioPrefix);
      if (start < 0) continue;
      const first = start + ratioPrefix.length;
      const end = body.indexOf("`", first);
      if (end < 0) continue;
      const data = new Value(body.substring(first, end));
      const identity = data.get("identity");
      if (data.get("schema_version").integer() != 1
        || identity.get("repository").string() != details.get("fullName").string()
        || identity.get("pr_number").integer() != pr.number) continue;
      // The publisher updates one canonical comment. A stale envelope is a
      // pending comparison, not a reason to scan every historical review page.
      if (identity.get("head_sha").string() != pr.head
        || identity.get("target_sha").string() != pr.base) {
        waitingForRatio();
        return;
      }
      // Verify provenance before accepting any current-head measurement. Pending
      // reports need no producer reads: they already fail the ratio gate.
      const producers = ["buildbuddy.yaml", "scripts/llm-cc-ratio.py"];
      for (let i = 0; i < producers.length; i++) {
        const path = producers[i];
        if (repo.readText(path, "head") != repo.readText(path, "base")) {
          checks.report("LM-CC/token", false,
            "The comparison producer changed: " + path + "; deploy producer updates through a trusted rollout.");
          return;
        }
      }
      const base = F64.parseFloat(data.get("base_ratio").json);
      const head = F64.parseFloat(data.get("head_ratio").json);
      const valid = data.get("status").string() == "complete"
        && isFinite(base) && isFinite(head) && base >= 0 && head >= 0;
      checks.report("LM-CC/token", valid && head <= base,
        valid ? "Repository LM-CC/token: " + base.toString() + " -> " + head.toString()
          + (head <= base ? "; maintained or improved." : "; regression.")
          : "The current comparison has no complete, finite LM-CC/token measurements.");
      return;
    }
    nextPage = page.nextPage;
  }
  waitingForRatio();
}

// Runnerless compiles this program to Wasm using its versioned SDK.
// Pull requests execute the trusted base revision of the program.
export function configure(): void {
  configuration.titlePolicy();
  badges.define(new BadgeSpec("badge", "medium", "slowest-job"));
  program.configure();
  configuration.workflow("crate-publishing", ["workflow_run", "runnerless_completion"]);
  configuration.workflow("pull-request-policy", ["pull_request"]);
  configuration.workflow("llm-cc-token-ratio", [
    "pull_request", "status", "runnerless_completion", "issue_comment",
  ]);
}

export function run(): void {
  program.run();
  if (event.isMerge()) artifacts.cleanup();
  const details = event.details();
  publishCrates(details);
  reportPullChecks(details);
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
