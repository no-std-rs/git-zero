# Independent R17 Worker socket RPC design review

**Conditional design approval for a first bounded transport proof; no production source approval.** An authenticated Native-to-Worker WebSocket that internally dispatches one ordinary DO HTTP request per RPC has a credible >=1% known-request opportunity. No R17 code, runtime proof, measured R14/R15/R16/R17 score, acceptance or stopping decision follows. Source implementation should start only from the exact frozen reviewed predecessor authorized by root; the first real pinned-workerd proof must succeed before broad integration.

This independent review used source/retained-report reads, offline Python arithmetic and official documentation lookup on 2026-10-18 only. No source edits, dependencies, builds, services, probes, benchmark runs, CI actions or deployment occurred. No MJS was created or executed.

## Price and frozen-accounting check

The official rule supports the transport hypothesis: a Worker WebSocket Upgrade is a billed request, while messages do not add Worker requests; Worker subrequests do not add inbound Worker charges. Every proposed internal DO HTTP dispatch remains one ordinary DO request. DO WebSocket ratios/hibernation are irrelevant here. [Workers pricing](https://developers.cloudflare.com/workers/platform/pricing/).

I directly read all four original R13 reports from run `39097951585`, checked their exact SHA256 against the opportunity input, indexed all 704 measured rows and independently summed their actual workload service/gateway/DO requests per profile/sample. Both samples reproduce:

| Profile | Actual R13 service HTTP callbacks | Gateway requests | DO requests | Conditional post-R15/R16 service callbacks |
| --- | ---: | ---: | ---: | ---: |
| 16 MiB cold | 191 | 176 | 367 | 119 |
| 16 MiB warm | 32 | 176 | 208 | 32 |
| 64 MiB cold | 316 | 176 | 492 | 244 |
| 64 MiB warm | 34 | 176 | 210 | 34 |
| Sum of profile means | **573** | **704** | **1,277** | **429** |

573 is measured. 429 subtracts the unmeasured R15/R16 cohort hypotheses and is **conditional**, not an observed predecessor trace. At frozen Worker pricing and 1,000 repetitions the respective gross opportunities are `$0.1719` and `$0.1287`. Four Upgrades add `$0.0012` in their actual physical scopes, yielding only illustrative `$0.1707`/`$0.1275` before all other effects. Do not subtract a startup Upgrade from a fixed-workload score that excludes startup, or hide a lazy timed Upgrade outside its triggering operation. Warmup/sample sockets and reconnects cannot be treated as one invisible global connection.

A useful conservative falsification threshold: at the measured R13 $1.80461843618 denominator, a 1% request gain requires at least **61 net removed Worker requests** at $.0003 each. A few reconnects do not erase a 429-callback hypothesis, but per-RPC reconnecting does. Actual new predecessor traces must replace 429, and any extra R2/SQL/recovery/fallback cost must be charged. Added CPU/duration/Native cost is unknown; full bill remains NULL.

I inspected the frozen accounting model, evaluator and orchestration source. `UsageCounters` prices `worker_requests` and `do_requests` separately. `benchmark_cost.rs` checks Worker gateway+service+observer category reconciliation, not Worker==DO equality. The existing DO receipt validator still requires exactly one DO request with zero Worker fields. Thus truthful Worker<DO counts need no pricing change or weakened invariant. Do not add per-message billable Worker increments, rewrite old HTTP counters, change objective/fixtures/rates or make unknown compute zero. Diagnostics for socket messages/bytes/IDs/outcomes belong outside billed `UsageCounters` and the strict DO receipt field set.

## Mandatory structural conditions

1. Factor the existing authenticated service bridge into a transport-neutral helper and invoke **that helper plus `dispatchService` directly** from a socket message. No self-fetch, top-level HTTP recursion or DO WebSocket is permitted. Preserve all current pre-I/O route/body/capability checks, private-header stripping, trusted catalog flags, proof context/issuance, strict typed response shape and per-DO serialization.
2. Strict exact opt-in registration leaves old clients on HTTP. Authenticate the actual Upgrade with repo/token/subprotocol/session, never a token in a URL; bind its lifetime to the captured registration. Recheck registration identity before dispatch and response. Replacement/unregister closes the old socket. A mutable registry is not durable global authority and must not become a lease.
3. Versioned binary frames have exact IDs/method/allowlisted route/phase/headers/body lengths and bounded response status/headers/body. One mutex owns one outstanding Native RPC; reject pipelining rather than queueing bodies. Reject unknown routes, duplicate/out-of-order IDs, cross-repo/session messages and private-header spoofing before DO I/O. Do not expose arbitrary URLs. Ping/pong/close is transport-only, with no storage dispatch.
4. Once a frame **might have reached** the Worker, any write/read/timeout/close/missing or malformed reply is ambiguous: return the existing error and preserve dirty/recovery behavior. Never resend or fall back to HTTP in the same logical RPC. Only an explicitly unsupported handshake or known-ineligible route can choose HTTP before storage I/O. Reconnect for a later distinct RPC with a fresh counted Upgrade; reconnection is not permission to replay an uncertain PUT/CAS/delete. Retirement proof epoch/session rules remain unchanged.
5. Add the real DO receipt exactly once before response framing/proof/send can fail. A lost reply does not erase charged work. Missing/invalid dispatch receipts mark accounting incomplete, not zero. Native accepts proof only after exact reply ID/status/body validation. A reconnect cannot repair an aggregate Worker epoch reset into a valid benchmark trace. Diagnostics must expose upgrade attempts, message received/dispatched/completed/failed counts and bytes, closes and incomplete outcomes without bounded-history blob retention.
6. Leave registration/unregister/metrics observers on HTTP initially. Keep startup, observer, timed workload, fallback and reconstruction receipts in their actual scopes. Public Git gateway and R15/R16 internal first-read authority dispatches remain ordinary fresh checks. R17 must not accidentally move these existing gateway DO calls through the Native service socket or claim they were eliminated.

## Caps, memory and compatibility: issues that can falsify the design

**Legacy scalar responses are a hard transport selection issue.** `HttpServices::call` permits generic scalar responses up to 128 MiB. A response whose size is not known before I/O to fit the socket must stay HTTP. Do not read/HEAD first merely to decide, or retry over HTTP after an oversized result. Bulk reads, known bounded PUT/upload/CAS/retire operations can be allowlisted; generic scalar GET remains productive HTTP from the start. This does not remove the measured bulk/publication cohort.

**Do not broaden fused-upload caps.** Existing GZUP `upload_frame` caps the complete frame at `MAX_BULK_RESPONSE = 16 MiB + 2,060` bytes even though metadata has a separate 1,500,000-byte bound. Therefore a full 16-MiB chunk plus 1.5-MB metadata is deliberately unsupported for fused upload and uses ordinary pre-I/O fallback. The first socket control must prove valid full-size bulk/PUT and metadata boundaries separately, or demonstrate rejection/fallback for the combined oversized fused body. A larger WS envelope is not authorization to expand a route's body/API cap. Preserve all 256-entry/16-MiB bulk, 1.5-MB metadata, 32-MiB retained, 64-MiB incoming and 128-MiB snapshot limits.

Current official incoming Worker messages are limited to 32 MiB and are fully buffered before JS message delivery; binary delivery now defaults to Blob at this compatibility date, so explicitly select ArrayBuffer before accept. Runtime updates/crashes can disconnect sockets. These limits require actual pinned-runtime proof rather than trusting a modern doc or an experimental flag. [WebSocket API](https://developers.cloudflare.com/workers/runtime-apis/websockets/).

**A message-level busy check alone is insufficient to claim an isolate memory bound.** By the time it runs the entire incoming frame has already been buffered. For this local experiment, one authorized active socket per registration, a single synchronous SDK sender/mutex, explicit one-outstanding enforcement and no pending payload map/queue provide a practical legitimate-client bound. Verify that admission occurs before storage I/O and invalid pipelining closes promptly. Broader concurrent-repository isolate memory remains a production limitation, not a new malicious-client/global-concurrency security project or an assumed solved property. Keep generic HTTP productive when socket capacity is unavailable before storage I/O.

**Library retained capacity matters.** The host currently has no locked WebSocket client dependency. A reviewed pinned blocking implementation should audit handshake/masking/control/fragment reassembly, message AND frame bounds, uncompressed parsing, timeouts, dependency/MSRV impact and fresh random masks. Avoid inventing partial RFC6455 handling. Charge retained client read/write/message buffers, mutex/session/ID structures inside the existing joint 32-MiB owning-allocation budget and trim the response cache accordingly. One outstanding call does not prove bounded retained capacity: a library may retain maximum request and response capacities after returning. Observe allocation retention after maximum success/error frames; transfer/drop complete message ownership promptly.

Official limits are per invocation and finite; do not assume a socket removes CPU/subrequest budgets. The current paid subrequest default is 10,000, but local proof still does not establish session CPU/reset behavior. If unresolved, conservatively rotate at a bounded **pre-I/O** RPC count/age boundary and count every Upgrade; do not interrupt an in-flight mutation. [Platform limits](https://developers.cloudflare.com/workers/platform/limits/). For illustration only, a maximum of 16 RPCs per socket yields at least `ceil(119/16)+ceil(32/16)+ceil(244/16)+ceil(34/16)=29` Upgrades for the conditional profile counts, or $.0087 known request cost, leaving $.1200 conditional request opportunity before other effects. This is still credible >=1% potential, not measured net cost; startup/observer traffic can add more. No production global capacity or low CPU/duration claim follows from local workerd.

## Required first real gate, then broader correctness

Before broad implementation, use a Rust-driven harness at the exact pinned workerd/config/source:

- One authenticated Upgrade followed by two real RPCs: exact **one new Worker request and two DO HTTP receipts**, correct phases/status/body/header/proof behavior and unchanged real R2/SQL attempts/bytes. Compare ordinary HTTP controls and retain message diagnostics. This demonstrates topology/accounting, not accepted cost.
- Maximum valid 16-MiB PUT/bulk and metadata-bound frames with current route total bounds, exact response framing and reassembled-message limits; no cap overrides or experimental large-frame flags. Prove generic legacy >32-MiB scalar GET chooses productive HTTP before I/O.
- Verify admission, binary/control/fragment/close semantics, session/auth/ID/header/length rejection and retained buffer ownership. If the installed runtime cannot satisfy the first gate, stop broad implementation and retain the failed evidence.

Only after this gate should full receiving/fetch integration be considered: genuine small/large/thin Git pushes, immediate post-retirement historical/latest fetches, existing first-read opt-outs, strict refs/fsck/archive, legacy HTTP and old-Native compatibility; disconnect faults before and after actual PUT, coupled CAS/sync/DELETE and committed reply; correct same-key/proof barriers and subsequent dirty recovery; observer scopes, caps and realistic SQLite/storage shape. Independent exact-source/fault/accounting review precedes any pushed four-profile performance screen. Retain every latency/resource failure and unknown billing dimension.

This candidate remains credible but unmeasured. It neither proves a global floor nor provides a stopping decision. **STOP READY.**

## Exact reviewed inputs and source identities

The source checkout is mutable R15 preparation; these hashes identify inspected source, not an approved implementation SHA.

- `/tmp/git-zero-worker-socket-rpc-design-audit.md` SHA256 `a21de53639c78d5d5343563bd09628780300605c4943e0d499132daba5d4661c` (17196 bytes).
- `/tmp/git-zero-worker-socket-rpc-opportunity.json` SHA256 `0ad67a04dbb650e36716f1d3598b02879003bc641e42f8d58ae9c11476ddfad7` (8228 bytes).

| Source under `/tmp/git-zero-hosting-gateway-first-read` | SHA256 | Bytes |
| --- | --- | ---: |
| `examples/repository-host/src/cloudflare.rs` | `e36fe507b12858da91f5145cacddf4359fd454a30c3391017361118cd24f6a94` | 100246 |
| `examples/repository-host/cloudflare/src/index.js` | `bd42e9b9fc4039688b554c97a47d89202df4422fa5b91e008ec892811bb39f17` | 89194 |
| `examples/repository-host/src/accounting.rs` | `4f2f265d0766c78bcb1540e8b57a85eaf94392aa55692ae8d6ed33859cfae000` | 10273 |
| `examples/repository-host/src/benchmark_cost.rs` | `0dc340f39d51c4baf86e7d8b142f0a02ffaf9dc10345af7c22b27d59fc581401` | 93500 |
| `examples/repository-host/src/native.rs` | `838ff4672d8ceabbb0a04c4d8efc40f6fe60aa1153c25f008eef2e2604780874` | 103312 |
| `examples/repository-host/Cargo.toml` | `336a79704487da8c19cad6ca8b155c5f2847db7cb19b8647a65d9a3393804ec2` | 675 |
| `Cargo.lock` | `29f82b6c33ed8dece62460cfa73752f025215130ffa5377e6d7170cc0b6d1af5` | 15837 |
| `examples/repository-host/cloudflare/wrangler.toml` | `4d35c7af5e0725605b99801be715e53405689036a53c9df2682eba411cea353d` | 493 |
| `.github/workflows/hosting-isolated.yml` | `9ceb00f9740b9bb4b1652ba3d93175cc3bad9aa937b653ece1d3b186cd5503eb` | 26107 |

Written at 2026-10-18T06:02:11.300146+00:00.
