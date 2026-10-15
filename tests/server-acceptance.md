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
`S1`/`S2` are smart v1/v2, `D` is dumb HTTP, and `1/256` means SHA-1 and SHA-256. Worker real-Git clone
tests use SHA-1. Its SHA-256 ABI checks use independent Node hashes, not a Git
reference-server comparison. Worker tests do not prove deployed Cloudflare
behavior. Rust allocation counters and Wasm memory ceilings are distinct checks.

## Objects, refs and negotiation

| Feature | Named local case / oracle evidence | Host, protocol, format | Remaining gap |
| --- | --- | --- | --- |
| Empty/bare/unborn HEAD | Native `head_states::real_git_empty_and_unborn_servers_match_git_http_backend`; `references::real_git_sha256_empty_repositories_match_git_servers` | N, S0/D, 1/256; Git backend/static oracle | No Worker empty fixture. |
| Detached HEAD independent of refs | Native `references::real_git_detached_head_beyond_ref_closures_matches_git_servers` and SHA-256 counterpart; `head_states::real_git_detached_servers_with_and_without_refs_match_git_http_backend` | N, S0/D, 1/256; Git oracle | Worker only exposes its configured snapshot HEAD. |
| Independent branches, symbolic chains, annotated/nested and lightweight tags, peeled/tree/blob targets | Native `references` symbolic/unborn cases and SHA-256 counterparts, via `server_references.py::compare` | N, S0/D, 1/256; discovery, full/single-branch clone and selected fetch versus Git | No equivalent Worker fixture. |
| Nested/empty trees, executables, symlinks, binary blobs | Native `snapshot::real_git_clones_nested_modes_empty_trees_and_custom_refs`; `storage::real_git_clones_file_backed_nested_snapshots_with_and_without_prepacked_data`; `references::real_git_deep_binary_modes_and_packed_fixtures_match_git_servers_in_both_formats` | N, S0/S1/S2/D, 1/256 rich checkout/mode/object comparisons; empty trees N S0/D, 1 | Worker metadata is flat and has no matching rich fixture. |
| Large/generated bounded object bodies | Native `snapshot::real_git_clones_generated_objects_with_bounded_read_scratch`; core `reader::generated_pack_and_http_bytes_match_borrowed_snapshots_for_every_budget` | N, S0/D, 1; Git clone/fsck; C byte equivalence | No Worker generated provider; kernel-scale prep remains planned. |
| Deep trees/history, large binary and packed independent source | Native `references::real_git_deep_binary_modes_and_packed_fixtures_match_git_servers_in_both_formats`, via `server_rich.py`; 40-level tree, 32 changing historical blobs, merge, 384 KiB binary, unusual ref and nested tags | N, S0/S1/S2/D, 1/256; full/incremental clone/fetch, refs, raw object bodies, recursive tree modes, checkout and strict fsck versus packed-only Git oracle with verified deltas | git-zero serves exported raw objects as stored-block packs/loose objects. This does not establish compressed/delta pack generation/reuse or matching Worker coverage. |
| Incremental snapshot builder and stable IDs | Core `builder::git_strictly_accepts_a_builder_snapshot_with_custom_commit`; Worker `SHA-256 Wasm snapshots match independent IDs and retain fixed memory` | C strict Git import, 1; W ABI hashes, 256 | Broader cross-host custom commit/identity fixtures remain. |
| Object indexes and graph validation | Core `validation::strict_git_accepts_validated_streamed_merge_history_and_gitlinks`, `references` and `sha256` invalid graph tests; native `references` validates Git-exported file providers | C/N, 1/256; strict import and Git oracle | Exhaustive graph/name policy is outside the validator's documented contract. |
| Multiple/duplicate/overlapping wants, merges and exact selected closures | `server_references.py::compare_wants` in native `references`; core `references::upload_pack_enforces_advertised_wants_and_preflights_all_selection_budgets` | N, S0, 1/256; matched requests and exact objects versus Git | Only advertised HEAD/ref/peeled IDs are allowed; broader want policies remain unadvertised. |
| Single ACK, multi_ack, multi_ack_detailed and common-history exclusion | `server_negotiation.py::compare_negotiation` in native symbolic `references` cases; core `upload` and `references::common_commit_closures_exclude_shared_merge_history_and_reset_between_requests` | N, S0, 1/256; matched Git rounds, incremental pack membership/import and real fetch; W ACK ABI, 1/256 | No ready/no-done shortcuts; complete-pack hooks may resend history. Worker lacks a multi-commit backend. |
| Sideband and no-progress | Same native negotiation comparison; core `upload::sideband_modes_preserve_large_pack_bytes_checksums_and_packet_bounds`; Worker `negotiated ACK and sideband responses keep fixed Wasm memory in both formats` | N, S0, 1/256; both modes versus Git; W ABI, 1/256; real-Git Worker clone, 1 | No progress telemetry. Pack payload uses channel 1; terminal failures use the replacement ERR API. |
| Terminal error packets and malformed uploads | Native `compare_negotiation` checks rejected want ERR; core `upload::bounded_error_packets_replace_failed_output_without_backend_details` and parser truncation/phase/capability tests | N/C, S0; framing/error assertions, not exact Git diagnostics | Hosts opt into `write_upload_error` after discarding failed buffered output. Client error decoding remains deferred. |
| Protocol v1/v2, ls-refs and fetch | `server_protocol.py::compare_protocol` in native `references`; core `protocol`; Worker v0/v1/v2 real-Git clones and v1/v2 memory tests | N, S1/S2, 1/256; Git backend oracle, exact imported packs/ACKs/tags, empty/unborn/symbolic/detached refs; W Git SHA-1, ABI 1/256 | Bounded depth/blob filters are covered below; other optional extensions remain absent. No deployed Worker oracle. |
| Incremental protocol I/O, backpressure, cancellation and resume | Core `streaming` byte equivalence, retry/guards/one-byte pulls/cancel; native `references` Git-oracle suite uses streamed responses and `snapshot` streams a generated 1 MiB blob; native disconnected socket cancellation; Worker pull/cancel ABI tests and real Git clones | N S0/S1/S2/D 1/256 via references; W Git SHA-1, ABI 1/256; allocation-free C state | Requests accumulate within caller-owned bounds before parsing. HTTP chunked intake, nonblocking native polling and deployed Worker behavior remain host work. |
| Shallow/deepen/filter | `server_shallow.py::compare_bounded_fetch` in native symbolic `references`; core `references::bounded_fetch`; Worker clone and memory tests | N S0/S1/S2 1/256: Git depth/merge clones, absolute/relative deepening/unshallow, exact shallow files and delivered commit/tree/blob bodies; W real-Git SHA-1 and ABI 1/256 | Relative depth uses Git 2.55 wanted-history semantics; the versioned oracle explicitly verifies Git 2.53's unrelated-branch difference. Time/exclusion deepening and other filters rejected. Partial clones require --no-checkout: unadvertised promised-object wants remain rejected. Worker has one root commit. |
| HTTP methods, media types, service/version selection and normalized paths | Core `protocol::strict_http_metadata_and_canonical_routes_precede_all_output`, malformed/truncated v2 commands and routing seeds; native/Worker metadata forwarding and gzip requests | C negative guards/budgets; N S0/S1/S2 1/256 Git clients; W SHA-1 Git and gzip/415 checks | HTTP framing, chunking and deployment policy remain host work. Canonical routes accept at most one leading slash; encoded service parameter names are rejected. |

## Packing and caches

| Feature | Named local case / oracle evidence | Host, protocol, format | Remaining gap |
| --- | --- | --- | --- |
| Loose objects and stored-block packs | Native `references`, `snapshot`, `storage`; core `pack_git::git_accepts_empty_duplicate_and_multiblock_objects` | N, S0/D, 1/256 via references; strict import/objects versus Git | Baseline pack encoding is uncompressed, without deltas. |
| Packed dumb HTTP and pack/index publication | `objects/info/packs` currently empty; loose D clone cases above | N, D loose-only | Implement valid pack/index downloads and packed-only/mixed clone/fetch comparisons. Client `upstream-packed-dumb` is not server evidence. |
| Actual bounded DEFLATE compression | No server encoder case | No parity evidence | Implement compression budgets and compare objects with stored fallback. Decoder tests do not establish encoder support. |
| Existing pack/prepacked reuse | Native `storage::real_git_clones_file_backed_nested_snapshots_with_and_without_prepacked_data` and `prepacked_files_reject_corruption_truncation_extras_and_wrong_closure` | N, S0, 1; Git clone/fsck and exact byte verification | Only this encoder's complete HEAD closure. Requested-closure keys and arbitrary compressed/prepacked entries remain planned. |
| Optional delta generation | No server generator case | No parity evidence | Add strict CPU/memory budgets and Git importer/clone comparisons. Client delta decoding is separate. |
| Streaming pack index creation | No server index writer case | No parity evidence | Generate index/CRCs/fanout alongside pack and validate using Git. |
| Versioned closure/format/encoder cache keys | Core `cache::encoder_version_is_part_of_the_cache_identity`; SHA-256 cache checks in `sha256` | C, 1/256; current complete-HEAD keys only | Version requested-closure keys before caching selected packs. |
| Fixed-memory caching, eviction and failures | Native `storage::real_git_fetches_changed_file_snapshots_with_cache_eviction_and_return_to_old_head`; core `cache` failure tests | N, S0/D, 1; Git A → B → A checkout/object checks; C failure paths | Native disk and async KV/R2, coordinated writers, corruption verification and concurrent eviction remain planned. |

## Hosts, resources and completion

| Feature | Named local case / oracle evidence | Host, protocol, format | Remaining gap |
| --- | --- | --- | --- |
| Native listener and HTTP framing | All native server comparison suites use IPv4 loopback, bounded HTTP/1.0 requests/responses | N, S0/D, 1/256 | IPv6/TLS, HTTP/1.1 keep-alive/chunking, configurable timeouts/body/connection limits and poll-based independent connections remain planned. |
| Streaming Wasm ABI and host callbacks | Worker pull ABI equivalence/cancel/restart and independent-instance tests; ReadableStream pull/cancel callbacks; real-Git clone tests use streaming output | W, S0/S1/S2/D; real-Git SHA-1 clone; ABI 1/256; fixed 24-page memory, no allocator/imports | Requests remain bounded complete frames; async storage and deployed-edge testing remain planned. |
| Worker async storage, immutable persistence and isolate reuse | Worker `metadata snapshot exposes available public data and pins objects across requests`; overlapping real-Git clones | W, S0/D, 1; self-consistency and Git fsck | No independent Git backend comparison, durable storage or deployed-edge test. URLs still carry immutable snapshot data. |
| Tree-shaped metadata and public binding projection | Worker metadata/public-variable assertions | W, flat metadata only | Tree-shaped projection, deployed auth/storage policy and retention configuration remain planned. |
| Allocation and memory ceilings | Native `storage::provider_validation_pack_verification_and_serving_allocate_nothing`; core `upload`, `references`; Worker memory tests; `test_benchmark_memory.py` and sampled-tree CI caps | C/N allocator counters; W fixed 24-page initial ceiling and no growth, 1/256; N opt-in bounded live server/descendant RSS sweeps | Sampled RSS can miss between-sweep peaks and counts shared pages per process. JS/isolate memory and production Worker CPU/retention diagnostics remain unmeasured. |
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
