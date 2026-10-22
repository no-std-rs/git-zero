import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { stripTypeScriptTypes } from "node:module";
import { test } from "node:test";
import { createContext, runInContext } from "node:vm";

// Execute the checked-in program against bounded SDK responses. Only the host
// calls are substituted; the policy ordering and envelope checks are real code.
const source = stripTypeScriptTypes(readFileSync(new URL("../.runnerless-ci.ts", import.meta.url), "utf8"))
  .replace(/^import[\s\S]*?from "runnerless\/v1";\n/gm, "")
  .replace(/^export /gm, "");
const prefix = "<!-- codex-review-artifact-publisher -->\n<!-- ci-toolkit:complexity -->\n";
const pull = { number: 54, state: "open", head: "current-head", base: "current-base" };

class Value {
  constructor(json = "null") { this.value = JSON.parse(json); this.json = json; }
  get(key) { return new Value(JSON.stringify(this.value?.[key] ?? null)); }
  isNull() { return this.value === null; }
  integer() { return this.value; }
  string() { return typeof this.value === "string" ? this.value : ""; }
}

function envelope(changes = {}, identity = {}) {
  const data = {
    schema_version: 1, status: "complete", base_ratio: 0.1, head_ratio: 0.09,
    identity: { repository: "no-std-rs/git-zero", pr_number: pull.number, head_sha: pull.head, target_sha: pull.base, ...identity },
    ...changes,
  };
  return { actorId: 317265358, body: `${prefix}LM-CC/token data: \`${JSON.stringify(data)}\`` };
}

function execute({ event = "status", state = "open", number = pull.number, comment = envelope(), publication = {}, files = {},
  lookup = () => new Value(JSON.stringify({ state: "available", head: pull.head, base: pull.base, comment, ...publication })),
} = {}) {
  const reports = [], reads = [], lookups = [];
  let validations = 0, recipes = 0;
  class Spec {
    file() { return this; }
    releasePlease() { return this; }
    publishArtifacts() { return this; }
    run() { recipes++; }
  }
  const context = createContext({
    Value, F64: { parseFloat }, isFinite,
    ArtifactSpec: Spec, ArtifactStatus: Spec,
    ArtifactComment: { generated() { return {}; } },
    OperationRef: Spec,
    workflow: () => new Spec(),
    event: { isMerge: () => false, details: () => new Value(JSON.stringify({ event, pullNumber: number, fullName: "no-std-rs/git-zero", ...(event === "pull_request" ? { state } : {}) })) },
    pulls: { get: () => ({ ...pull, state }) },
    policies: { validate() { validations++; } },
    checks: { report(name, passed, summary) { reports.push({ name, passed, summary }); } },
    repo: { readText(path, ref) { reads.push({ path, ref }); return files[path]?.[ref] ?? "trusted"; } },
    comments: { artifact(publisher) {
      lookups.push(publisher);
      return lookup();
    }, list() { assert.fail("comparison checks must never enumerate comment history"); } },
    jobs: { lookup: () => ({ ok: false }) },
  });
  runInContext(source, context);
  runInContext("run()", context);
  return { reports, reads, lookups, validations, recipes };
}

function assertWaiting(options) {
  assert.deepEqual(execute(options), {
    reports: [{ name: "LM-CC/token", passed: false,
      summary: "Waiting for verified LM-CC/token data for the current PR head and base." }],
    reads: [], lookups: ["complexity"], validations: 1, recipes: 1,
  });
}

test("PR and recovery deliveries regenerate required title and commit policies", () => {
  for (const event of ["pull_request", "status", "runnerless_completion", "issue_comment"]) {
    const result = execute({ event });
    assert.equal(result.validations, 1, event);
    assert.equal(result.recipes, 1, event);
    assert.equal(result.reports[0].passed, true, event);
    assert.equal(result.reads.length, 4, "both producers must match head and base");
  }
});

// These cases share the same contract: one lookup, no producer reads, a
// failing measurement, and required policies still regenerated.
for (const [name, cases] of [
  ["a stale canonical report fails closed through one indexed lookup",
    [{ head_sha: "previous-head" }, { target_sha: "previous-base" }]
      .map(identity => ({ comment: envelope({}, identity) }))],
  ["pending publisher comments fail closed with plain or Markdown line breaks",
    ["", "  "].map(spacing => ({ comment: { actorId: 317265358,
      body: prefix.replaceAll("\n", spacing + "\n") + "## Build artifact\nPublication: pending" } }))],
  ["publisher authentication and repository, PR and schema identities remain mandatory", [
    { ...envelope(), actorId: 1 },
    { ...envelope(), body: envelope().body.replace("ci-toolkit:complexity", "ci-toolkit:other") },
    envelope({}, { repository: "other/repository" }),
    envelope({}, { pr_number: 999 }),
    envelope({ schema_version: 2 }),
  ].map(comment => ({ comment }))],
  ["absent measurements stay failing while required policies are regenerated", [{ comment: null }]],
  ["non-current or unavailable publication sets never supply a ratio", [
    { state: "missing" }, { state: "pending" }, { state: "expired" }, { state: "unavailable" },
    { head: "previous-head" }, { base: "previous-base" }, { comment: null },
  ].map(publication => ({ publication }))],
]) {
  test(name, () => {
    for (const options of cases) assertWaiting(options);
  });
}

test("a changed comparison producer cannot supply a passing ratio", () => {
  for (const changedProducer of ["buildbuddy.yaml", "scripts/llm-cc-ratio.py"]) {
    const result = execute({ files: { [changedProducer]: { head: "changed" } } });
    assert.equal(result.reports[0].passed, false);
    assert.ok(result.reports[0].summary.includes(changedProducer));
  }
});

test("regressions, incomplete reports and invalid ratios fail closed", () => {
  for (const changes of [
    { head_ratio: 0.11 }, { status: "pending" },
    { base_ratio: null }, { head_ratio: null },
    { base_ratio: -1 }, { head_ratio: -1 },
  ]) {
    assert.equal(execute({ comment: envelope(changes) }).reports[0].passed, false);
  }
  for (const changes of [{ head_ratio: 0.1 }, { base_ratio: 0, head_ratio: 0 }]) {
    assert.equal(execute({ comment: envelope(changes) }).reports[0].passed, true);
  }
});

test("closed PRs and default-branch deliveries do not emit PR policies", () => {
  for (const options of [{ state: "closed" }, { number: null }]) {
    const result = execute(options);
    assert.equal(result.validations, 0);
    assert.deepEqual(result.lookups, []);
    assert.deepEqual(result.reports, []);
  }
});

test("an index/backend failure remains an error rather than a missing measurement", () => {
  assert.throws(() => execute({ lookup() { throw new Error("host_failure"); } }), /host_failure/);
});
