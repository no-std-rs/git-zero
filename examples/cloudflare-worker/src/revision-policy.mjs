// Trusted host configuration, copied before awaiting any untrusted request body.
// These limits only enable supported history cuts; they never authorize wants.
export function revisionPolicy(module, value, ...entrypoints) {
  if (value === undefined || value === null) return null;
  if (typeof value !== "object" || Array.isArray(value)) throw new TypeError("Invalid revision limits");
  const { maxExclusions, graphReadLimit } = value;
  if (!Number.isSafeInteger(maxExclusions) || maxExclusions < 0 || maxExclusions > 8 ||
      !Number.isSafeInteger(graphReadLimit) || graphReadLimit < 0 || graphReadLimit > 1048576) {
    throw new TypeError("Revision limits exceed fixed Wasm budgets");
  }
  const exports = WebAssembly.Module.exports(module);
  for (const name of entrypoints) {
    if (!exports.some((entry) => entry.kind === "function" && entry.name === name)) {
      throw new TypeError(`Revision history requires Wasm export ${name}`);
    }
  }
  return Object.freeze({ maxExclusions, graphReadLimit });
}

export function environmentRevisions(module, env) {
  if (env.GIT_REVISION_HISTORY === undefined || env.GIT_REVISION_HISTORY === "disabled") return null;
  if (env.GIT_REVISION_HISTORY !== "enabled") throw new TypeError("Invalid revision history setting");
  return revisionPolicy(module, {
    maxExclusions: env.GIT_MAX_EXCLUSIONS === undefined ? 8 : Number(env.GIT_MAX_EXCLUSIONS),
    graphReadLimit: env.GIT_GRAPH_READ_LIMIT === undefined ? 1048576 : Number(env.GIT_GRAPH_READ_LIMIT),
  }, "stream_start_revisions", "stream_start_repository_revisions");
}
