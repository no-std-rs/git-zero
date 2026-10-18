# Run the local Cloudflare host

The local twin uses workerd R2 and SQLite Durable Object bindings, with the
Rust/native Git codec outside workerd. No Cloudflare account is required.
See the [design](hosting-design.md) for storage boundaries and current limits.

## Run

From the repository root:

```sh
npm ci --prefix examples/repository-host/cloudflare
cargo build --profile hosting -p git-zero-host --bins --locked
```

Start the emulator in one terminal:

```sh
WRANGLER_LOG_PATH=/tmp/git-zero-wrangler-logs \
WRANGLER_HIDE_BANNER=true WRANGLER_SEND_METRICS=false \
X_LOCAL_EXPLORER=false X_LOCAL_OBSERVABILITY=false \
node examples/repository-host/cloudflare/node_modules/wrangler/bin/wrangler.js dev \
  --config examples/repository-host/cloudflare/wrangler.toml --local \
  --ip 127.0.0.1 --port 8798 --persist-to /tmp/git-zero-cloudflare-state \
  --show-interactive-dev-session=false
```

Start and register the codec in another terminal:

```sh
target/hosting/repository-host --cloudflare http://127.0.0.1:8798 \
  --repo demo --token git-zero-local-services --view-cache warm \
  /tmp/git-zero-codec-demo 8910
```

Then push an existing repository and clone through the Worker gateway:

```sh
git -C /path/to/source push http://127.0.0.1:8798/demo/repo.git HEAD:refs/heads/main
git clone http://127.0.0.1:8798/demo/repo.git /tmp/git-zero-cloudflare-clone
```

The Worker exposes `/health`. Codec diagnostics at
<http://127.0.0.1:8910/_object-stats> report object inventory, binding operations
and cost estimates. Diagnostic operations are recorded separately from Git work.

## State and restarts

Keep the emulator's `--persist-to` directory to retain R2/SQLite state. After
restarting the emulator, restart the codec to register it again. One codec owns
each repository; changing its port requires unregistering the previous origin
first. Run the codec command with `--unregister` in place of its local root and
port to release that registration.

The checked-in token is for local development. Service routes require a token
and repository namespace; this example supplies no production authentication.
Keep the emulator and codec on loopback. The native view is disposable; R2 and
the Durable Object manifest are authoritative.

For [service-contract and restart tests](validation.md#local-cloudflare-service-contracts),
use the same persistent directory across restart phases. For measurements, see
[the Rust benchmark commands and cost model](hosting-performance.md).
