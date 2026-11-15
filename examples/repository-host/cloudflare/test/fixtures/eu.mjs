import codec from "../../build/git_zero_host_wasm.wasm";
import { createRepositoryHost } from "../../src/host.mjs";
const host = createRepositoryHost({ codec, authMode: "gateway", bindings: { repositories: "EU_REPOS", objects: "DATA", grantKey: "KEY" }, routePrefix: "", keyPrefix: "eu-host/", jurisdiction: "eu" });
export const EURepository = host.Repository;
// workerd explicitly does not implement jurisdiction placement. Emulate the
// binding interface with distinct real native object IDs, without a production
// fallback: both the caller and the DO must still use exactly the same scope.
function scopedEnv(env) {
  const base = env.EU_REPOS;
  return { ...env, EU_REPOS: {
    idFromName: (name) => base.idFromName(name), get: (id) => base.get(id),
    jurisdiction(scope) {
      if (scope !== "eu") throw new Error("unexpected jurisdiction");
      return { idFromName: (name) => base.idFromName(`fixture-jurisdiction-eu:${name}`), get: (id) => base.get(id) };
    },
  } };
}
export class LocalEURepository extends host.Repository {
  constructor(ctx, env) { super(ctx, scopedEnv(env)); }
}
export default { fetch(request, env) {
  env = scopedEnv(env);
  const url = new URL(request.url);
  if (url.searchParams.has("direct")) {
    const ns = url.pathname.split("/")[1].slice(0, -4), headers = new Headers(request.headers); headers.set("x-git-zero-namespace", ns);
    const binding = url.searchParams.get("direct") === "wrong" ? env.EU_REPOS : env.EU_REPOS.jurisdiction("eu");
    return binding.get(binding.idFromName(ns)).fetch(new Request(request, { headers }));
  }
  return host.fetch(request, env);
} };
