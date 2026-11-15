// Thin reference deployment. Embedders import host.mjs with their own codec,
// route/binding names, prefix and resource limits instead of importing this file.
import module from "../build/git_zero_host_wasm.wasm";
import { createRepositoryHost } from "./host.mjs";
import { bucket } from "./r2-s3.mjs";
const host = createRepositoryHost({
  codec: module, bindings: { repositories: "REPOSITORIES", objects: "BLOBS", token: "HOST_TOKEN", grantKey: "GRANT_KEY" },
  bucket, authMode: (env) => env.AUTH_MODE ?? "token", routePrefix: "", keyPrefix: "",
  healthPath: "/health", testPrefix: "/_test", testEnabled: (env) => env.TEST_MODE === "true",
  implicitCreate: (env) => env.IMPLICIT_CREATE ?? true, objectFormat: (env) => Number(env.REPO_FORMAT ?? 0),
  policy: (env) => ({ allowNonFastForward: env.ALLOW_NON_FAST_FORWARD === "true", maxStoredBytes: env.MAX_STORED_BYTES || null, maxPushBytes: env.MAX_PUSH_BYTES || null }),
  // Preserve the reference suite's independent concurrent idle-body requests.
  // Embedded scripts default to one active job and should budget their isolate.
});
export const OwnRepository = host.Repository;
export default { fetch: host.fetch };
