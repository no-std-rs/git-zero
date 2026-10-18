// Runtime fixture for the Rust ignored contract, never a test runner. Invoke
// Wrangler with retirement-fault.toml; all storage and accounting use real glue.
import worker, { RepositoryServices as Services } from "../src/index.js";
export default worker;

export class RepositoryServices extends Services {
  constructor(ctx, env) {
    let failed = false;
    const blobs = new Proxy(env.BLOBS, {
      get(target, name) {
        if (name === "delete") return async (key) => {
          if (!failed && key.endsWith("/chunks/f0907009f48aab66413b6d0e171ed5fd3543954d0d939a060651b6db8b8a4116")) {
            failed = true;
            // The real R2 binding rejects this argument. Production remove's
            // existing operation wrapper records that failed binding attempt.
            return target.delete(Symbol("injected retirement failure"));
          }
          return target.delete(key);
        };
        const value = Reflect.get(target, name);
        return typeof value === "function" ? value.bind(target) : value;
      },
    });
    super(ctx, { ...env, BLOBS: blobs });
  }
}
