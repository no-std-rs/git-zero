# Read-only server acceptance matrix

This is the server worklist for ROADMAP sections 1–6. A covered row identifies
git-zero serving a real Git client, or explicitly identifies a narrower core
test. It does not declare the overall server parity gate complete.

The upstream case map is [upstream-http.md](upstream-http.md), pinned to Git
v2.53.0. Fixtures and assertions here are independently authored under MIT OR
Apache-2.0; no GPL upstream test bodies are copied. Running unchanged upstream
tests against Git is a reference baseline, without proving git-zero coverage.

## Evidence and reproduction

Native reference comparisons use real Git as client and `git http-backend`
(smart) or a Git-created static bare repository (dumb) as oracle. They compare
refs, object IDs/kinds/bodies, checkouts and `fsck --strict`. Matched raw requests
also check physical pack membership and strict import; pack encoding can differ.
The native CI matrix runs Linux/macOS and system/pinned Git 2.53.0.

```sh
cargo test -p git-zero-rustix --test head_states --test references --test snapshot --test storage --locked
cargo test -p git-zero --test upload --test references --test sha256 --test fuzz --locked
python3 tests/interop.py --jobs 8 --artifacts /tmp/git-zero-server-acceptance
cd examples/cloudflare-worker
npm test
```

`N` below means the native rustix HTTP host, `W` means the exact Worker handler
hosted by Node with real Wasm, and `C` means a core-only test. `S0` is smart v0,
`S1`/`S2` are smart v1/v2, `D` is dumb HTTP, and `1/256` means SHA-1 and SHA-256.
The default Worker metadata fixture uses SHA-1. Its separate bounded raw-object
fixture compares real-Git S0/S1/S2 clones with a Git-created source in both
formats. Loose D results explicitly separate unassisted missing-empty-tree
behavior from assisted complete-object checks. Worker tests do not prove deployed
Cloudflare behavior. Rust allocation counters and Wasm memory ceilings are
distinct checks.

## Objects, refs and negotiation

| Feature | Named local case / oracle evidence | Host, protocol, format | Remaining gap |
| --- | --- | --- | --- |
| Empty/bare/unborn HEAD | Native `head_states::real_git_empty_and_unborn_servers_match_git_http_backend`; `references::real_git_sha256_empty_repositories_match_git_servers`; Worker `repository-memory.test.mjs` empty/unborn import | N, S0/D, 1/256; Git backend/static oracle; W ABI, 1/256 | No real-Git Worker empty clone fixture. |
| Detached HEAD independent of refs | Native `references::real_git_detached_head_beyond_ref_closures_matches_git_servers` and SHA-256 counterpart; `head_states::real_git_detached_servers_with_and_without_refs_match_git_http_backend`; Worker detached raw import ABI | N, S0/D, 1/256; Git oracle; W ABI, 1/256 | Worker detached real-Git oracle comparison remains. |
| Independent branches, symbolic chains, annotated/nested and lightweight tags, peeled/tree/blob targets | Native `references` cases via `server_references.py::compare`; Worker `repository.test.mjs` scaled 44-object fixture | N, S0/D, 1/256; W, S0/S1/S2, 1/256; discovery, full/single-branch clone and incremental fetch versus Git | Worker fixture has one immediate symbolic alias and nested/lightweight tags, not every native ref target/chain case. |
| Nested/empty trees, executables, symlinks, binary blobs | Native `snapshot`, `storage` and rich `references` cases; Worker `repository.test.mjs` compares canonical objects, recursive modes and checkout bytes | N, S0/S1/S2/D, 1/256; W, S0/S1/S2, 1/256; W D assisted | Default metadata remains flat. Worker loose D omits one referenced empty tree, also omitted by unchanged Git loose control; tests verify direct HTTP bytes/ID and explicitly import it before 44-object fsck. |
| Large/generated bounded object bodies | Native `snapshot::real_git_clones_generated_objects_with_bounded_read_scratch`; core `reader::generated_pack_and_http_bytes_match_borrowed_snapshots_for_every_budget`; Worker scaled 96 KiB blob and exact 128 KiB arena tests | N, S0/D, 1; W, S0/S1/S2, 1/256; C byte equivalence | Worker imports complete bounded bodies; a generated or asynchronous host-backed read provider and kernel-scale prep remain planned. |
| Deep trees/history, large binary and packed independent source | Native rich `references` via `server_rich.py`: 40 levels, 32 changing historical blobs, merge, 384 KiB binary and packed-only Git oracle with deltas; Worker `test/fixture.py`: 44 raw objects, eight directory components/nine tree levels, eight mainline commits plus side/merge, 96 KiB binary, modes and nested tags | N, S0/S1/S2/D, 1/256; W, S0/S1/S2, 1/256; full/incremental refs, canonical bodies, modes, checkout and strict fsck versus Git-created source | Scaled Worker coverage does not match the full native fixture. Stored-block output does not establish compressed/delta generation/reuse. Worker loose D needs explicit empty-tree import before complete fsck. |
| Incremental snapshot builder and stable IDs | Core `builder::git_strictly_accepts_a_builder_snapshot_with_custom_commit`; Worker `SHA-256 Wasm snapshots match independent IDs and retain fixed memory` | C strict Git import, 1; W ABI hashes, 256 | Broader cross-host custom commit/identity fixtures remain. |
| Object indexes and graph validation | Core `validation::strict_git_accepts_validated_streamed_merge_history_and_gitlinks`, `references` and `sha256` invalid graph tests; native `references` validates Git-exported file providers | C/N, 1/256; strict import and Git oracle | Exhaustive graph/name policy is outside the validator's documented contract. |
| Multiple/duplicate/overlapping wants, merges and exact selected closures | `server_references.py::compare_wants` in native `references`; core `references::upload_pack_enforces_advertised_wants_and_preflights_all_selection_budgets` | N, S0, 1/256; matched requests and exact objects versus Git | Only advertised HEAD/ref/peeled IDs are allowed; broader want policies remain unadvertised. |
| Single ACK, multi_ack, multi_ack_detailed and common-history exclusion | `server_negotiation.py::compare_negotiation` in native symbolic `references` cases; core `upload` and `references::common_commit_closures_exclude_shared_merge_history_and_reset_between_requests`; Worker raw multi-commit full/incremental clones | N, S0, 1/256; matched Git rounds and exact membership; W ACK ABI and real-Git incremental, 1/256 | No ready/no-done shortcuts; complete-pack hooks may resend history. Worker does not independently compare every ACK round. |
| Sideband and no-progress | Same native negotiation comparison; core `upload::sideband_modes_preserve_large_pack_bytes_checksums_and_packet_bounds`; Worker `negotiated ACK and sideband responses keep fixed Wasm memory in both formats` | N, S0, 1/256; both modes versus Git; W ABI, 1/256; real-Git Worker clone, 1 | No progress telemetry. Pack payload uses channel 1; terminal failures use the replacement ERR API. |
| Terminal error packets and malformed uploads | Native `compare_negotiation` checks rejected want ERR; core `upload::bounded_error_packets_replace_failed_output_without_backend_details` and parser truncation/phase/capability tests | N/C, S0; framing/error assertions, not exact Git diagnostics | Hosts opt into `write_upload_error` after discarding failed buffered output. Client error decoding remains deferred. |
| Protocol v1/v2, ls-refs and fetch | `server_protocol.py::compare_protocol` in native `references`; core `protocol`; Worker metadata and raw44 v0/v1/v2 real-Git clones and ABI tests | N, S1/S2, 1/256; Git backend oracle, exact imported packs/ACKs/tags, empty/unborn/symbolic/detached refs; W Git/ABI 1/256 | Bounded depth/blob filters are covered below; other optional extensions remain absent. No deployed Worker oracle. |
| Incremental protocol I/O, backpressure, cancellation and resume | Core `streaming` byte equivalence, retry/guards/one-byte pulls/cancel; native `references` Git-oracle suite and generated 1 MiB snapshot; disconnected sockets; Worker pull/cancel, raw arena reuse and real Git full/incremental clones | N S0/S1/S2/D 1/256; W Git S0/S1/S2 1/256, D assisted and ABI 1/256; allocation-free C state | Requests accumulate within caller-owned bounds before parsing. HTTP chunked intake, nonblocking native polling and deployed Worker behavior remain host work. |
| Shallow/deepen/filter | `server_shallow.py::compare_bounded_fetch` in native symbolic `references`; core `references::bounded_fetch`; Worker metadata clone, shallow and memory tests | N S0/S1/S2 1/256: Git depth/merge clones, absolute/relative deepening/unshallow, exact shallow files and delivered commit/tree/blob bodies; W real-Git SHA-1 and ABI 1/256 | Relative depth uses Git 2.55 wanted-history semantics; the versioned oracle verifies Git 2.53's unrelated-branch difference. Time/exclusion deepening and other filters rejected. Partial clones require --no-checkout: unadvertised promised-object wants remain rejected. Worker shallow cases still use one-root metadata, not the raw history fixture. |
| HTTP methods, media types, service/version selection and normalized paths | Core `protocol::strict_http_metadata_and_canonical_routes_precede_all_output`, malformed/truncated v2 commands and routing seeds; native/Worker metadata forwarding and gzip requests | C negative guards/budgets; N S0/S1/S2 1/256 Git clients; W SHA-1 Git and gzip/415 checks | HTTP framing, chunking and deployment policy remain host work. Canonical routes accept at most one leading slash; encoded service parameter names are rejected. |

## Packing and caches

| Feature | Named local case / oracle evidence | Host, protocol, format | Remaining gap |
| --- | --- | --- | --- |
| Loose objects and stored-block packs | Native `references`, `snapshot`, `storage`; core `pack_git::git_accepts_empty_duplicate_and_multiblock_objects` | N, S0/D, 1/256 via references; strict import/objects versus Git | Baseline pack encoding is uncompressed, without deltas. |
| Packed dumb HTTP and pack/index publication | `objects/info/packs` currently empty; loose D clone cases above | N, D loose-only | Implement valid pack/index downloads and packed-only/mixed clone/fetch comparisons. Client `upstream-packed-dumb` is not server evidence. |
| Actual bounded DEFLATE compression | No server encoder case | No parity evidence | Implement compression budgets and compare objects with stored fallback. Decoder tests do not establish encoder support. |
| Existing pack/prepacked reuse | Native `storage::real_git_clones_file_backed_nested_snapshots_with_and_without_prepacked_data` and `prepacked_files_reject_corruption_truncation_extras_and_wrong_closure` | N, S0, 1; Git clone/fsck and exact byte verification | Only this encoder's complete HEAD closure. Requested-closure keys and arbitrary compressed/prepacked entries remain planned. |
| Optional delta generation | No server generator case | No parity evidence | Add strict CPU/memory budgets and Git importer/clone comparisons. Client delta decoding is separate. |
| Streaming pack index creation | Core `pack_index::streamed_indexes_match_gits_independent_indexer_in_both_formats` and generated/selected/large-offset/error cases | C, 1/256; byte-identical Git indexer output, `verify-pack` and `show-index`, allocation counters | Caller-owned entry metadata; stored-block encoding only. Packed dumb HTTP publication remains planned. |
| Versioned closure/format/encoder cache keys | Core `cache::encoder_version_is_part_of_the_cache_identity`; SHA-256 cache checks in `sha256` | C, 1/256; current complete-HEAD keys only | Version requested-closure keys before caching selected packs. |
| Fixed-memory caching, eviction and failures | Native `storage::real_git_fetches_changed_file_snapshots_with_cache_eviction_and_return_to_old_head`; core `cache` failure tests | N, S0/D, 1; Git A → B → A checkout/object checks; C failure paths | Native disk and async complete-pack KV/R2 caches, coordinated writers, corruption verification and concurrent eviction remain planned. Snapshot persistence below is separate. |

## Hosts, resources and completion

| Feature | Named local case / oracle evidence | Host, protocol, format | Remaining gap |
| --- | --- | --- | --- |
| Native listener and HTTP framing | IPv4 native comparison suites; `git-zero6-smart` and `git-zero6-dumb` add real-Git clone/fetch/fsck on `::1`, including smart v0/v1/v2 shallow/filter checks. Socket tests verify IPv6-only binding, assigned addresses, close-on-exec and accepted-socket timeouts. | N, S0/S1/S2/D; IPv6 CLI fixture SHA-1, existing IPv4 suites 1/256 | TLS, HTTP/1.1 keep-alive/chunking, configurable timeouts/body/connection limits and poll-based independent connections remain planned. |
| Streaming Wasm ABI and host callbacks | Worker legacy pull/cancel/restart tests; `repository-memory.test.mjs` versioned import, exact 64 objects/eight refs/128 KiB, malformed manifests, mixed legacy/raw sessions and independent-instance interleaving; shared ReadableStream callbacks | W, S0/S1/S2 1/256; D assisted; no allocator/imports, fixed 24-page ceiling/no growth | Requests remain bounded complete frames. Imported bodies are fully resident; asynchronous provider waits during Rust preparation/pulls and deployed-edge testing remain planned. |
| Worker async storage, immutable persistence and isolate reuse | `storage.test.mjs` KV/R2-shaped adapters: awaited publication, fresh isolates, concurrent readers, paused-response eviction, cancellation, expiry, corruption, oversized records and read/write failures; real-Git v2/dumb full/incremental clones verify captured files/fsck | W, S2/D, 1; local async adapter fixtures and independently owned Wasm responses | Durable immutable snapshot URLs are optional; tests do not provision KV/R2 or measure propagation/deployed latency. Complete-pack storage/cache remains separate. |
| Tree-shaped metadata and public binding projection | `metadata.test.mjs` explicit public-header selection, Access credential exclusion, own JSON-only selected bindings and capture budgets; `worker.test.mjs` real-Git public-object audit | W, flat metadata; S2/D, 1 | New captures are bounded and explicitly selected; old immutable IDs are preserved. Tree-shaped projection and deployed authentication/retention diagnostics remain planned. Raw repository import does not change the metadata layout. |
| Allocation and memory ceilings | Native `storage::provider_validation_pack_verification_and_serving_allocate_nothing`; core `upload`, `references`; Worker legacy/raw memory tests; `test_benchmark_memory.py` and sampled-tree CI caps | C/N allocator counters; W measured release initial 22 pages under 24-page ceiling, exact raw capacity and no growth, 1/256; N opt-in bounded live server/descendant RSS sweeps | Sampled RSS can miss between-sweep peaks and counts shared pages per process. JS/isolate memory and production Worker CPU/retention diagnostics remain unmeasured. |
| Fixed-budget and optional kernel benchmarks | `test_benchmark_limits.py`, `test_benchmark_source.py`, `test_benchmark_memory.py`; CI benchmark smoke/default-policy and pinned-caller fixtures, opt-in sampled process-tree RSS caps | N, S0/D, SHA-1, paired Git host and identical prepared objects; aggregate server RSS/observer overhead per HTTP operation | Kernel-scale preparation, controlled cold-cache runs and exact/unique physical memory peaks remain planned. Partial or failed observations cannot pass sampled-tree limits. Timing does not establish parity. |
| Fuzzing malformed input and bounded state | `fuzz::seeded_fuzz_targets_replay_and_mutate_the_corpus`, including v0/v2 routing/negotiation/sideband, shallow/filter selection and gzip seeds; CI ASan campaigns | C, bounded packets/graphs/decoders/routing | Pack-index/HTTP framing and concurrent host state need coverage as their APIs are implemented. |
| Retained traces and cross-platform coverage | Native `references` retains fixtures, responses, packs and Git packet/curl logs on failure; `interop.py` retains sharded artifacts | Linux/macOS × system/Git 2.53.0; W CI Node host | Deployed Worker and unsupported protocol/format combinations remain gaps. |
| Upstream fixture substitution | Reusable `server_fixture.py::export_fixture` exports reachable objects, refs and HEAD from Git-created packed/loose fixtures; optional `scripts/test-upstream-git.sh` baseline | Native local comparisons, 1/256; reused by refs and rich server fixtures | No substitution into Git's Apache harness. Adapter and assertions are independently authored; preserve GPL provenance if upstream bodies are ever copied. |
| Read-only server completion gate | Rows above are the auditable worklist | Partial S0/D coverage | Shallow/filter, streaming/concurrency, packed dumb publication and full native/Worker oracle coverage must pass before claiming parity or starting further client work. |

Git-zero client discovery/download/decoder tests remain regression coverage.
`upstream-packed-smart` and `upstream-packed-dumb` exercise Git serving a git-zero
client; they do not prove git-zero can serve arbitrary prepacked/delta repositories.
Authentication and deployment policy belong to hosts. Read/write receive-pack
is a later, separate roadmap gate.
