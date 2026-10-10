import { badges, BadgeSpec } from "runnerless/v1";
import {
  ArtifactComment, ArtifactSpec, ArtifactStatus, artifacts,
  configuration, event, policies, workflow,
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

// Runnerless compiles this program to Wasm using its versioned SDK.
// Pull requests execute the trusted base revision of the program.
export function configure(): void {
  badges.define(new BadgeSpec("badge", "medium", "slowest-job"));
  program.configure();
  configuration.workflow("pull-request-policy", ["pull_request"]);
}

export function run(): void {
  program.run();
  if (event.isMerge()) artifacts.cleanup();
  const details = event.details();
  if (details.get("event").string() != "pull_request"
    || details.get("state").string() != "open") return;

  policies.validate();
}
