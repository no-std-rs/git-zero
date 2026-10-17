// Only a trusted loader/deployment chooses permission. No request wire field or
// captured snapshot can grant access to objects in another repository view.
export class WantPolicyError extends Error {
  constructor(message) { super(message); this.status = 503; }
}

export function wantPolicy(module, value = "advertised-only", maxWantLines = 64, ...entrypoints) {
  if (value === "advertised-only") return null;
  if (value !== "all-visible-reachable" || !Number.isSafeInteger(maxWantLines) || maxWantLines < 0 || maxWantLines > 0xffffffff) {
    throw new WantPolicyError("Invalid host want policy");
  }
  const exports = WebAssembly.Module.exports(module);
  for (const name of entrypoints) {
    if (!exports.some((entry) => entry.kind === "function" && entry.name === name)) {
      throw new WantPolicyError(`Want policy requires Wasm export ${name}`);
    }
  }
  return Object.freeze({ maxWantLines });
}
