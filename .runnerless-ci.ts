import { badges, BadgeSpec } from "runnerless/v1";
import {
  ArtifactComment, ArtifactSpec, ArtifactStatus, artifacts,
  checks, comments, configuration, event, PageRequest, policies, pulls, repo, Value, workflow,
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

const program = workflow().publishArtifacts("complexity", comparison);
const ratioPrefix = "LM-CC/token data: `";
const publisherPrefix = "<!-- codex-review-artifact-publisher -->\n<!-- ci-toolkit:complexity -->\n";
// The same My Toolkit bot that publishes the verified comparison envelope.
const publisherActorId: i64 = 317265358;

function reportRatio(details: Value): void {
  if (details.get("pullNumber").isNull()) return;
  const pr = pulls.get();
  if (pr.state != "open") return;
  // A verified envelope proves publication identity, not measurement provenance.
  // Accept ratios only when the workflow and producer match trusted base code.
  const producers = ["buildbuddy.yaml", "scripts/llm-cc-ratio.py"];
  for (let i = 0; i < producers.length; i++) {
    const path = producers[i];
    if (repo.readText(path, "head") != repo.readText(path, "base")) {
      checks.report("LM-CC/token", false,
        "The comparison producer changed: " + path + "; deploy producer updates through a trusted rollout.");
      return;
    }
  }
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
        || identity.get("pr_number").integer() != pr.number
        || identity.get("head_sha").string() != pr.head
        || identity.get("target_sha").string() != pr.base) continue;
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
  checks.report("LM-CC/token", false,
    "Waiting for verified LM-CC/token data for the current PR head and base.");
}

// Runnerless compiles this program to Wasm using its versioned SDK.
// Pull requests execute the trusted base revision of the program.
export function configure(): void {
  badges.define(new BadgeSpec("badge", "medium", "slowest-job"));
  program.configure();
  configuration.workflow("pull-request-policy", ["pull_request"]);
  configuration.workflow("llm-cc-token-ratio", [
    "pull_request", "status", "runnerless_completion", "issue_comment",
  ]);
}

export function run(): void {
  program.run();
  if (event.isMerge()) artifacts.cleanup();
  const details = event.details();
  reportRatio(details);
  if (details.get("event").string() != "pull_request"
    || details.get("state").string() != "open") return;

  policies.validate();
}
