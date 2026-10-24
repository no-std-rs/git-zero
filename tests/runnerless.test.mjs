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
const pull = { number: 54, state: "open" };

class Value {
  constructor(json = "null") { this.value = JSON.parse(json); this.json = json; }
  get(key) { return new Value(JSON.stringify(this.value?.[key] ?? null)); }
  isNull() { return this.value === null; }
  integer() { return this.value; }
  string() { return this.value; }
}

function execute({ event = "status", state = "open", number = pull.number } = {}) {
  const reports = [];
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
    event: { isMerge: () => false, details: () => new Value(JSON.stringify({ event, pullNumber: number, fullName: "no-std-rs/git-zero" })) },
    pulls: { get: () => ({ ...pull, state }) },
    policies: { validate() { validations++; } },
    checks: { report(name, passed, summary) { reports.push({ name, passed, summary }); } },
    jobs: { lookup: () => ({ ok: false }) },
  });
  runInContext(source, context);
  runInContext("run()", context);
  return { reports, validations, recipes };
}

test("PR and recovery deliveries regenerate title and commit policies without GPU reports", () => {
  for (const event of ["pull_request", "status", "runnerless_completion", "issue_comment"]) {
    const result = execute({ event });
    assert.equal(result.validations, 1, event);
    assert.equal(result.recipes, 1, event);
    assert.deepEqual(result.reports, [], "retired comparisons must not emit a merge gate");
  }
});

test("closed PRs and default-branch deliveries do not emit PR policies", () => {
  for (const options of [{ state: "closed" }, { number: null }]) {
    const result = execute(options);
    assert.equal(result.validations, 0);
    assert.equal(result.recipes, 1);
    assert.deepEqual(result.reports, []);
  }
});
