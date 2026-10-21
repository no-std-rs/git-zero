export class RequestError extends Error {}

// Preserve storage exceptions and their retry metadata/identity. Weak ownership
// does not retain failed requests or the objects attached to their errors.
const storageFailures = new WeakSet();
export function storageFailure(error) {
  if ((typeof error !== "object" || error === null) && typeof error !== "function") error = new Error(String(error));
  storageFailures.add(error); return error;
}
export function isStorageFailure(error) { return storageFailures.has(error); }
