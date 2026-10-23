// Instance-scoped resource measurements. Missing billing dimensions stay unknown.
export const FIELDS = ["sqlCalls", "sqlFailures", "sqlRowsRead", "sqlRowsWritten", "r2Gets", "r2Puts", "r2Deletes", "r2Failures", "r2Retries", "r2ReadBytes", "r2WrittenBytes"];
const SQL = new Set(FIELDS.slice(0, 4));
const ROWS = new Set(["sqlRowsRead", "sqlRowsWritten"]);
const INVENTORY = ["objects", "refs", "stagedObjects", "stagedChunks", "temporaryObjects", "temporaryChunks"];
export const UNKNOWN_BILLING = ["Worker CPU", "Durable Object duration", "monthly SQLite storage", "bytes transferred by failed or partial R2 responses"];

function counter(values, field) {
  if (!Object.hasOwn(values, field)) throw new Error("missing service counter: " + field);
  const value = values[field];
  if (value === null && ROWS.has(field)) return null;
  if (!Number.isSafeInteger(value) || value < 0) throw new Error("invalid service counter: " + field);
  return value;
}

function difference(before, after, field) {
  const left = counter(before, field), right = counter(after, field);
  if (left === null || right === null) return null;
  if (right < left) throw new Error("service counter reset: " + field);
  return right - left;
}

export function usageDelta(before, after) {
  if (!before.epoch || before.epoch !== after.epoch) throw new Error("repository instance changed during measurement");
  return Object.fromEntries(FIELDS.map((field) => {
    const total = difference(before.usage, after.usage, field);
    const observer = SQL.has(field) ? difference(before.observerUsage, after.observerUsage, field) : 0;
    if (total === null || observer === null) return [field, null];
    if (observer > total) throw new Error("observer exceeds measured service work: " + field);
    return [field, total - observer];
  }));
}

export function checkReadOnly(before, after, usage) {
  if (INVENTORY.some((field) => counter(before, field) !== counter(after, field))) throw new Error("fetch changed published or retained storage");
  if (usage.r2Puts || usage.r2Deletes) throw new Error("fetch mutated R2 storage");
}

export function measuredUsage(before, after) {
  return { usage: usageDelta(before, after), epoch: after.epoch,
    accounting: { rawBefore: before.usage, rawAfter: after.usage, observerBefore: before.observerUsage, observerAfter: after.observerUsage },
    inventory: Object.fromEntries([...INVENTORY, "databaseBytes"].map((field) => [field, after[field]])) };
}
