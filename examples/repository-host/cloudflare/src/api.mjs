import { RequestError } from "./errors.mjs";
import { requestBody } from "./body.mjs";
export const encode = (value) => new TextEncoder().encode(value);
export const json = (value, status = 200) => Response.json(value, { status, headers: { "Cache-Control": "no-store" } });
export const hidden = () => json({ error: "unknown route" }, 404);
export class ApiError extends RequestError {
  constructor(message, status = 400) { super(message); this.status = status; }
}
export async function readJson(request, limit = 2048) {
  if (request.headers.get("content-type")?.split(";")[0].trim().toLowerCase() !== "application/json") throw new ApiError("application/json required", 415);
  if (request.headers.has("content-encoding") && request.headers.get("content-encoding").toLowerCase() !== "identity") throw new ApiError("JSON content encoding is unsupported", 415);
  const bytes = new Uint8Array(limit); let length = 0;
  for await (const part of requestBody(request.body, 10000)) {
    if (part.length > limit - length) throw new ApiError("JSON body exceeds byte limit", 413);
    bytes.set(part, length); length += part.length;
  }
  let value;
  try { value = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes.subarray(0, length))); }
  catch { throw new ApiError("invalid JSON"); }
  if (value === null || Array.isArray(value) || typeof value !== "object") throw new ApiError("JSON object required");
  return value;
}
export function apiRoute(request) {
  const parts = new URL(request.url).pathname.split("/");
  return parts.length === 5 && parts[2] === "api" && parts[3] === "v1" ? parts[4] : null;
}

export const READ_ROUTES = new Set(["refs", "history", "tree", "blob", "commit", "changes"]);
