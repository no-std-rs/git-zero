# Cloudflare metadata as a Git repository

This demo wraps git-zero's allocator-free Rust Wasm server in a JavaScript
Cloudflare Worker. It serves a synthetic repository containing:

| File | Contents |
| --- | --- |
| `cf.json` | All serializable metadata available on the capture request's `request.cf` |
| `headers.json` | The capture request's `cf-*` headers |
| `vars.json` | Public string/JSON bindings named in `EXPOSED_VARS` |
| `runtime.json` | Demo platform and snapshot format information |

There is one deterministic commit with no parents, on `main`. Cloudflare does
not expose every internal variable or Wrangler's own process environment to a
Worker. Local emulation may supply different or empty `request.cf` metadata.
Bindings such as KV/R2/services are not serializable environment variables.

## Run locally

```sh
rustup target add wasm32-unknown-unknown
cd examples/cloudflare-worker
npm ci
npm run dev
```

Wrangler builds `git-zero-worker-wasm` with Cargo and bundles the resulting Wasm
module. Open `http://localhost:8787/`, copy `clone_url` from the JSON response,
and run:

```sh
git clone "$CLONE_URL" cloudflare-snapshot
GIT_SMART_HTTP=0 git clone "$CLONE_URL" cloudflare-snapshot-dumb
```

Set `CLONE_URL` to the returned URL before running those commands. Both protocols
serve the same tree. Deploy explicitly with `npm run deploy` when ready; setup
and tests do not deploy anything.

## Immutable snapshot URLs

`GET /` captures metadata once and returns `/r/<base64url-snapshot>/repo.git`.
The token contains the four UTF-8 JSON file contents. Every Git request rebuilds
that exact snapshot, even in another isolate or after a retry. Neither discovery
nor an object request captures new metadata. Visit `/` again for a fresh snapshot.

This deliberately stateless demo needs no KV binding. Encoded snapshots are
public data and can appear in URLs/logs. Raw snapshot data is limited to 8 KiB;
request bodies are limited to 32 KiB. The Rust ABI has 64 KiB input and 128 KiB
output buffers. Larger repositories and shorter snapshot URLs need the planned
persistent snapshot/cache backend.

The Worker instantiates the precompiled module for each synchronous call and
copies the response before returning. That keeps Rust's fixed buffers independent
across overlapping requests. There are no Wasm imports, Rust allocator, Rust
`std`, `wasm-bindgen`, or Rust Worker SDK dependencies. The JavaScript host uses
normal runtime allocation.

## Public variables

Edit `[vars]` and the comma-separated `EXPOSED_VARS` list in `wrangler.toml`.
Wrangler string variables and secrets are indistinguishable through `env` at
runtime, so only deliberately selected public bindings are exported. Do not put
credentials in that list. Request authorization/cookie headers are excluded;
the demo intentionally exposes Cloudflare request metadata.

## Checks

```sh
npm test
npx wrangler deploy --dry-run --outdir /tmp/git-zero-worker-bundle
```

The tests exercise the actual compiled Wasm module, clone it with real Git over
smart and dumb HTTP, check `git fsck --strict`, and verify one commit, stable
snapshot IDs, variable selection, and overlapping requests. Direct ABI tests
enforce a 24-page (1.5 MiB) initial linear-memory ceiling and no memory growth
across repeated discovery/pack/loose requests, full-capacity inputs, malformed
frames and independent instances. The 64 KiB input and 128 KiB output capacities
are asserted separately; JavaScript allocation is outside this Wasm budget.
The raw ABI's `serve_with_format(length, method, format)` accepts format 0 for
SHA-1 and 1 for SHA-256; `serve` and the Worker use SHA-1. SHA-256 tests compare
independently computed snapshot IDs, pack trailers and loose objects, and reject
format mismatches while retaining the same memory ceiling.
The dry run validates
Wrangler packaging without deploying.

References: [Workers Wasm](https://developers.cloudflare.com/workers/runtime-apis/webassembly/),
[request metadata](https://developers.cloudflare.com/workers/runtime-apis/request/),
[environment bindings](https://developers.cloudflare.com/workers/configuration/environment-variables/).
